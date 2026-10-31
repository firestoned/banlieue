// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A sealed disk opens only with its own vTPM: the cross-member negative
//! test (roadmap 18 invariant 2; ADR-0088, ADR-0112).
//!
//! First-boot sealing gives every clone of one image the same LUKS header
//! UUID (ADR-0088, *Evidence from a live run*), because every clone formats
//! the same partition of the same image. That is harmless only if each
//! clone's volume key is its own and is sealed to its own vTPM. A suite that
//! only shows members *can* seal would also pass with a fleet-shared key;
//! this one shows a sealed disk *cannot* be opened by any other vTPM:
//!
//! 1. member A is sealed through banlieue (`GuestReady=True`,
//!    `sealedPartitions: [COS_PERSISTENT]`) and its console shows
//!    `COS_PERSISTENT` mounted from a device-mapper device: unlocked;
//! 2. A is powered off and its sealed disk copied;
//! 3. the copy boots as a new machine with a freshly manufactured vTPM: it
//!    must never unlock `COS_PERSISTENT`, and its `GuestReady` must never be
//!    `True`;
//! 4. A is powered on again, with its own vTPM, and unlocks: so step 3
//!    failed because of the vTPM, not because the disk was damaged.
//!
//! "Another member's vTPM" is a freshly manufactured one: every member's is
//! (ADR-0065), so one stands for all. The provider never lets a machine reuse
//! another's TPM state, and this suite does not reach around it.
//!
//! **Runs on the host itself, as root**, like `make ch-deferred-e2e`: it reads
//! guests' serial logs and copies a guest's disk into the storage class's
//! image cache. It assumes banlieue is installed and serving this host (see
//! `e2e_pool_claim.rs`).
//!
//! ```sh
//! export KUBECONFIG=~/.kube/<cluster>.yaml
//! BANLIEUE_E2E_PROVIDER=<provider>          # this host's Provider, with `vtpm`
//! BANLIEUE_E2E_IMAGE=<vmimage>              # firstBootSealing, ready for it
//! BANLIEUE_E2E_CLASS=<vmclass>              # tpmEnabled
//! BANLIEUE_E2E_STORAGE_DIR=/srv/banlieue/ch # the class's directory on this host
//!   cargo test -p banlieue-provider-cloud-hypervisor --test e2e_sealing_isolation \
//!     --no-run   # then run the binary through sudo, or: make ch-sealing-isolation-e2e
//! ```
//!
//! **The image needs one probe stage**, so the console says whether the
//! guest unlocked its persistent partition. The sealing report cannot answer
//! that: it says `COS_PERSISTENT` *is* a LUKS container, which a disk copied
//! from a sealed guest also is. Add this layer to the image's cloud-config
//! (it is test-only; production images do not need it):
//!
//! ```yaml
//! #cloud-config
//! stages:
//!   boot:
//!     - name: banlieue-e2e-persistent-probe
//!       if: "[ -f /run/cos/active_mode ] || [ -f /run/cos/passive_mode ]"
//!       commands:
//!         - echo "banlieue-e2e-persistent=$(findmnt -no SOURCE /usr/local || echo none)" > /dev/ttyS0
//! ```
//!
//! Optional: `BANLIEUE_E2E_NAMESPACE` (default `banlieue-system`),
//! `BANLIEUE_E2E_RUN_ROOT` (default `/run/banlieue/ch`).
//!
//! Every run names its objects with a unique prefix, removes exactly those
//! (and the disk copy) including after a failure, and reports anything the
//! teardown left on the host.

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use banlieue_api::banlieue::{Provider, VMImage, VirtualMachine};
use banlieue_api::infrastructure::CloudHypervisorMachine;
use banlieue_provider_cloud_hypervisor::systemd::{Bus, Systemd, UnitState};
use futures::FutureExt as _;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use kube::{Client, ResourceExt};
use serde_json::json;

/// A sealed member: recovery reset, reboot, first-boot encryption. 125 s
/// for a pool of two on one host (ADR-0112); a lone member is quicker.
const PROVISION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Power off, power on, teardown: seconds each, observed.
const TRANSITION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const POLL: Duration = Duration::from_secs(5);
/// The copy's `firstBootSealing.timeoutSeconds`: long enough that a guest
/// which *could* unlock would have announced (a sealed member reports about
/// 100 s after power-on), short enough to keep the run quick. The admission
/// minimum is 60.
const FOREIGN_VTPM_SEALING_TIMEOUT_SECS: u64 = 300;
/// How long past the copy's deadline to wait for `FirstBootSealingTimeout`:
/// the provider re-checks every 30 s while a report is due (ADR-0112).
const DEADLINE_SLACK: Duration = Duration::from_secs(90);
/// The probe line the image prints on every installed boot.
const PROBE_PREFIX: &str = "banlieue-e2e-persistent=";
/// What an unlocked `COS_PERSISTENT` is mounted from.
const MAPPED_SOURCE_PREFIX: &str = "/dev/mapper/";
const SEALED_PARTITION: &str = "COS_PERSISTENT";
const DEFAULT_RUN_ROOT: &str = "/run/banlieue/ch";
const DEFAULT_NAMESPACE: &str = "banlieue-system";
const OS_DISK: &str = "os.raw";
const IMAGE_CACHE: &str = "images";
const IMAGE_CACHE_MODE: u32 = 0o640;
/// Serial-console lines worth showing when a boot did not go as expected.
const CONSOLE_HINTS: [&str; 5] = [
    "KAIROS BOOT FAILED",
    "unlock",
    "TPM",
    "kcrypt",
    PROBE_PREFIX,
];
/// How many hint lines to print.
const CONSOLE_HINT_LINES: usize = 20;

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

struct Settings {
    provider: String,
    image: String,
    class: String,
    storage_dir: PathBuf,
    run_root: PathBuf,
    namespace: String,
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is not set; see the module docs of tests/e2e_sealing_isolation.rs")
    })
}

fn settings() -> Settings {
    Settings {
        provider: required("BANLIEUE_E2E_PROVIDER"),
        image: required("BANLIEUE_E2E_IMAGE"),
        class: required("BANLIEUE_E2E_CLASS"),
        storage_dir: PathBuf::from(required("BANLIEUE_E2E_STORAGE_DIR")),
        run_root: PathBuf::from(
            std::env::var("BANLIEUE_E2E_RUN_ROOT").unwrap_or_else(|_| DEFAULT_RUN_ROOT.into()),
        ),
        namespace: std::env::var("BANLIEUE_E2E_NAMESPACE")
            .unwrap_or_else(|_| DEFAULT_NAMESPACE.into()),
    }
}

// ---------------------------------------------------------------------------
// Waiting and reading
// ---------------------------------------------------------------------------

async fn wait_for<T, F, Fut>(what: &str, timeout: Duration, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = Instant::now();
    eprintln!("  … {what}");
    loop {
        if let Some(v) = check().await {
            eprintln!("  ✓ {what} ({}s)", start.elapsed().as_secs());
            return v;
        }
        assert!(
            start.elapsed() < timeout,
            "timed out after {}s waiting for: {what}",
            timeout.as_secs()
        );
        tokio::time::sleep(POLL).await;
    }
}

fn condition<'a>(conditions: &'a [Condition], kind: &str) -> Option<&'a Condition> {
    conditions.iter().find(|c| c.type_ == kind)
}

fn condition_true(conditions: &[Condition], kind: &str) -> bool {
    condition(conditions, kind).is_some_and(|c| c.status == "True")
}

/// The serial log, lossily: a console carries escape sequences.
fn console(path: &Path) -> String {
    std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// Every probe value printed in `text`, in order.
fn probes(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            l.split_once(PROBE_PREFIX)
                .map(|(_, v)| v.trim().to_string())
        })
        .collect()
}

fn unlocked(probe: &str) -> bool {
    probe.starts_with(MAPPED_SOURCE_PREFIX)
}

/// The console lines that explain a boot, for a failure message.
fn hints(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| CONSOLE_HINTS.iter().any(|h| l.contains(h)))
        .collect();
    let from = lines.len().saturating_sub(CONSOLE_HINT_LINES);
    lines[from..].join("\n    ")
}

// ---------------------------------------------------------------------------
// One run's objects
// ---------------------------------------------------------------------------

struct Scratch {
    client: Client,
    s: Settings,
    prefix: String,
    sd: Systemd,
}

/// A machine on this host, found from its `CloudHypervisorMachine`.
struct Guest {
    unit: String,
    machine_dir: PathBuf,
    run_dir: PathBuf,
}

impl Guest {
    fn serial_log(&self) -> PathBuf {
        self.machine_dir.join("serial.log")
    }
}

impl Scratch {
    async fn new() -> Self {
        let s = settings();
        let client = Client::try_default()
            .await
            .expect("no reachable cluster: set KUBECONFIG");
        let sd = Systemd::connect(Bus::System)
            .await
            .expect("the system bus: this suite runs on the Provider's host");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after the epoch")
            .subsec_nanos();
        let prefix = format!("e2e-ch-iso-{:x}{nanos:x}", std::process::id());
        eprintln!("  namespace: {}, prefix: {prefix}", s.namespace);
        Self {
            client,
            s,
            prefix,
            sd,
        }
    }

    fn name(&self, what: &str) -> String {
        format!("{}-{what}", self.prefix)
    }

    fn vms(&self) -> Api<VirtualMachine> {
        Api::namespaced(self.client.clone(), &self.s.namespace)
    }

    fn machines(&self) -> Api<CloudHypervisorMachine> {
        Api::namespaced(self.client.clone(), &self.s.namespace)
    }

    fn images(&self) -> Api<VMImage> {
        Api::all(self.client.clone())
    }

    /// Where the provider looks for `BackingFile` images in this class.
    fn image_cache(&self) -> PathBuf {
        self.s.storage_dir.join(IMAGE_CACHE)
    }

    fn copy_file_name(&self) -> String {
        format!("{}.raw", self.name("a-disk"))
    }

    /// Fail in seconds, with a clear message, on a fixture that cannot work.
    async fn require_fixture(&self) -> VMImage {
        let p = Api::<Provider>::namespaced(self.client.clone(), &self.s.namespace)
            .get_opt(&self.s.provider)
            .await
            .expect("reading Providers")
            .unwrap_or_else(|| panic!("Provider {} not found", self.s.provider));
        assert!(
            p.status
                .as_ref()
                .is_some_and(|st| condition_true(&st.conditions, "Ready")),
            "Provider {} is not Ready",
            self.s.provider
        );
        let image = self
            .images()
            .get(&self.s.image)
            .await
            .unwrap_or_else(|e| panic!("VMImage {}: {e}", self.s.image));
        assert!(
            image.spec.first_boot_sealing.is_some(),
            "VMImage {} declares no firstBootSealing: nothing would seal",
            self.s.image
        );
        assert!(
            image.status.as_ref().is_some_and(|st| st
                .per_provider
                .iter()
                .any(|r| r.provider_name == self.s.provider && r.ready)),
            "VMImage {} is not ready for Provider {}",
            self.s.image,
            self.s.provider
        );
        eprintln!(
            "  ✓ Provider {} Ready, VMImage {} seals and is ready for it",
            self.s.provider, self.s.image
        );
        image
    }

    async fn make_vm(&self, vm: &str, image: &str) {
        let obj: VirtualMachine = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "VirtualMachine",
            "metadata": { "name": vm },
            "spec": {
                "classRef": { "name": self.s.class },
                "imageRef": { "name": image },
                "placement": {
                    "failureDomainSelector": { "matchLabels": { "name": self.s.provider } }
                },
                "desiredPowerState": "PoweredOn",
                "migrationPolicy": "never",
            },
        }))
        .expect("VirtualMachine fixture");
        self.vms()
            .create(&PostParams::default(), &obj)
            .await
            .unwrap_or_else(|e| panic!("create VirtualMachine {vm}: {e}"));
    }

    async fn set_power(&self, vm: &str, state: &str) {
        self.vms()
            .patch(
                vm,
                &PatchParams::default(),
                &Patch::Merge(json!({ "spec": { "desiredPowerState": state } })),
            )
            .await
            .unwrap_or_else(|e| panic!("power {vm} {state}: {e}"));
    }

    async fn machine(&self, vm: &str) -> Option<CloudHypervisorMachine> {
        self.machines().get_opt(vm).await.ok()?
    }

    async fn guest(&self, vm: &str) -> Option<Guest> {
        let m = self.machine(vm).await?;
        let uid = m.uid()?;
        let host_uid = m.status.as_ref()?.host_uid?;
        Some(Guest {
            unit: format!("banlieue-ch@{host_uid}.service"),
            machine_dir: self.s.storage_dir.join(uid),
            run_dir: self.s.run_root.join(host_uid.to_string()),
        })
    }

    async fn wait_sealed(&self, vm: &str) {
        wait_for(
            &format!("{vm} to seal and announce"),
            PROVISION_TIMEOUT,
            || async {
                let st = self.machine(vm).await?.status?;
                (condition_true(&st.conditions, "GuestReady")
                    && st.sealed_partitions.iter().any(|p| p == SEALED_PARTITION))
                .then_some(())
            },
        )
        .await;
    }

    async fn wait_running(&self, vm: &str) {
        wait_for(&format!("{vm} running"), TRANSITION_TIMEOUT, || async {
            let st = self.machine(vm).await?.status?;
            condition_true(&st.conditions, "Ready").then_some(())
        })
        .await;
    }

    async fn wait_stopped(&self, vm: &str, g: &Guest) {
        wait_for(&format!("{vm} powered off"), TRANSITION_TIMEOUT, || async {
            let state = self.sd.state(&g.unit).await.ok()?;
            (state != Some(UnitState::Active)).then_some(())
        })
        .await;
    }

    /// Wait for a probe line in `g`'s console that appeared after `since`
    /// bytes (the whole file if the VMM started a fresh one), and return it.
    async fn wait_probe(&self, what: &str, g: &Guest, since: usize) -> String {
        let log = g.serial_log();
        wait_for(what, PROVISION_TIMEOUT, || {
            let log = log.clone();
            async move {
                let text = console(&log);
                let fresh = if text.len() < since {
                    &text[..]
                } else {
                    &text[since..]
                };
                probes(fresh).pop()
            }
        })
        .await
    }

    /// Copy the stopped guest's sealed disk into the image cache, owned and
    /// moded like the images already there, sparse (and reflinked where the
    /// filesystem can).
    fn copy_disk(&self, g: &Guest) -> PathBuf {
        let cache = self.image_cache();
        let meta = std::fs::metadata(&cache)
            .unwrap_or_else(|e| panic!("image cache {}: {e}", cache.display()));
        let dest = cache.join(self.copy_file_name());
        let status = std::process::Command::new("cp")
            .args(["--reflink=auto", "--sparse=always"])
            .arg(g.machine_dir.join(OS_DISK))
            .arg(&dest)
            .status()
            .expect("running cp");
        assert!(status.success(), "copying the sealed disk failed: {status}");
        std::os::unix::fs::chown(&dest, Some(meta.uid()), Some(meta.gid()))
            .expect("chown the disk copy");
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(IMAGE_CACHE_MODE))
            .expect("chmod the disk copy");
        dest
    }

    /// A `VMImage` for the copy: the fixture's spec, with this file as its
    /// only source and a shorter sealing deadline.
    async fn make_copy_image(&self, fixture: &VMImage, name: &str) {
        let mut spec = serde_json::to_value(&fixture.spec).expect("fixture spec");
        let mut source = spec["sources"]
            .as_array()
            .and_then(|s| {
                s.iter()
                    .find(|x| x["providerClass"] == "cloud-hypervisor")
                    .cloned()
            })
            .expect("the fixture VMImage has no cloud-hypervisor source");
        source["kind"] = json!("BackingFile");
        source["ref"] = json!(self.copy_file_name());
        spec["sources"] = json!([source]);
        spec["firstBootSealing"]["timeoutSeconds"] = json!(FOREIGN_VTPM_SEALING_TIMEOUT_SECS);
        let obj: VMImage = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "VMImage",
            "metadata": { "name": name },
            "spec": spec,
        }))
        .expect("VMImage fixture");
        self.images()
            .create(&PostParams::default(), &obj)
            .await
            .unwrap_or_else(|e| panic!("create VMImage {name}: {e}"));
        wait_for(
            &format!("VMImage {name} ready for {}", self.s.provider),
            TRANSITION_TIMEOUT,
            || async {
                let st = self.images().get(name).await.ok()?.status?;
                st.per_provider
                    .iter()
                    .any(|r| r.provider_name == self.s.provider && r.ready)
                    .then_some(())
            },
        )
        .await;
    }

    /// Delete this run's objects and the disk copy; report anything left.
    async fn teardown(&self, vms: &[String], image: &str) {
        eprintln!("  tearing down {}", self.prefix);
        let mut guests = Vec::new();
        for vm in vms {
            if let Some(g) = self.guest(vm).await {
                guests.push((vm.clone(), g));
            }
            let _ = self.vms().delete(vm, &DeleteParams::background()).await;
        }
        let deadline = Instant::now() + TRANSITION_TIMEOUT;
        for vm in vms {
            while self.machine(vm).await.is_some() && Instant::now() < deadline {
                tokio::time::sleep(POLL).await;
            }
        }
        let _ = self.images().delete(image, &DeleteParams::default()).await;
        let copy = self.image_cache().join(self.copy_file_name());
        if copy.exists() {
            let _ = std::fs::remove_file(&copy);
        }
        for (vm, g) in guests {
            let mut left = Vec::new();
            if let Ok(Some(st)) = self.sd.state(&g.unit).await {
                left.push(format!("unit {} ({st:?})", g.unit));
            }
            for dir in [&g.machine_dir, &g.run_dir] {
                if dir.exists() {
                    left.push(format!("directory {}", dir.display()));
                }
            }
            if !left.is_empty() {
                eprintln!("  ⚠ LEFT ON HOST by {vm}: {}", left.join(", "));
            }
        }
        if copy.exists() {
            eprintln!("  ⚠ LEFT ON HOST: {}", copy.display());
        }
    }
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// A sealed member's disk does not open with any vTPM but its own.
#[tokio::test]
#[ignore = "needs a Cloud Hypervisor host running banlieue, as root: see the module docs"]
async fn a_sealed_disk_does_not_open_with_another_members_vtpm() {
    let s = Scratch::new().await;
    let fixture = s.require_fixture().await;
    let a = s.name("a");
    let foreign = s.name("foreign");
    let copy_image = s.name("a-disk");

    let outcome =
        std::panic::AssertUnwindSafe(isolation_case(&s, &fixture, &a, &foreign, &copy_image))
            .catch_unwind()
            .await;
    s.teardown(&[a, foreign], &copy_image).await;
    if let Err(p) = outcome {
        std::panic::resume_unwind(p);
    }
}

async fn isolation_case(s: &Scratch, fixture: &VMImage, a: &str, foreign: &str, copy_image: &str) {
    // 1. A seals through banlieue and unlocks with its own vTPM.
    s.make_vm(a, &s.s.image).await;
    s.wait_sealed(a).await;
    let ga = s.guest(a).await.expect("member A's guest on this host");
    let probe = s
        .wait_probe("A's console to report its persistent partition", &ga, 0)
        .await;
    assert!(
        unlocked(&probe),
        "member A sealed but its persistent partition is not unlocked ({PROBE_PREFIX}{probe}); \
         the image's probe or its sealing is wrong:\n    {}",
        hints(&console(&ga.serial_log()))
    );
    eprintln!("  ✓ A unlocked COS_PERSISTENT with its own vTPM ({probe})");

    // 2. A off; its sealed disk copied.
    s.set_power(a, "PoweredOff").await;
    s.wait_stopped(a, &ga).await;
    let copy = s.copy_disk(&ga);
    eprintln!("  ✓ A's sealed disk copied to {}", copy.display());

    // 3. The copy, with a vTPM that is not A's, never unlocks or announces.
    s.make_copy_image(fixture, copy_image).await;
    s.make_vm(foreign, copy_image).await;
    s.wait_running(foreign).await;
    let gf = s
        .guest(foreign)
        .await
        .expect("the copy's guest on this host");
    let deadline = Duration::from_secs(FOREIGN_VTPM_SEALING_TIMEOUT_SECS) + DEADLINE_SLACK;
    let reason = wait_for(
        "the copy to time out without unlocking (FirstBootSealingTimeout)",
        deadline,
        || async {
            let text = console(&gf.serial_log());
            if let Some(p) = probes(&text).into_iter().find(|p| unlocked(p)) {
                panic!(
                    "ISOLATION BROKEN: A's sealed disk unlocked under another vTPM \
                     ({PROBE_PREFIX}{p}):\n    {}",
                    hints(&text)
                );
            }
            let st = s.machine(foreign).await?.status?;
            let ready = condition(&st.conditions, "GuestReady")?;
            assert_ne!(
                ready.status,
                "True",
                "the copy is GuestReady without its persistent partition unlocked ({}): \
                 a guest whose data volume did not open must not be handed out.\n    {}",
                ready.reason,
                hints(&text)
            );
            (ready.reason == "FirstBootSealingTimeout").then(|| ready.reason.clone())
        },
    )
    .await;
    eprintln!(
        "  ✓ the copy did not unlock under a fresh vTPM: GuestReady=False, {reason}\n    {}",
        hints(&console(&gf.serial_log()))
    );
    s.vms()
        .delete(foreign, &DeleteParams::background())
        .await
        .expect("delete the copy's VirtualMachine");

    // 4. A again, with its own vTPM: the disk was not damaged.
    let since = console(&ga.serial_log()).len();
    s.set_power(a, "PoweredOn").await;
    s.wait_running(a).await;
    let again = s
        .wait_probe("A's console after power-on, with its own vTPM", &ga, since)
        .await;
    assert!(
        unlocked(&again),
        "A no longer unlocks with its own vTPM ({PROBE_PREFIX}{again}): the copy step, not \
         the vTPM, may explain step 3.\n    {}",
        hints(&console(&ga.serial_log()))
    );
    eprintln!("  ✓ A unlocked again with its own vTPM ({again})");
    eprintln!("  RESULT a sealed disk opens with its own vTPM only");
}
