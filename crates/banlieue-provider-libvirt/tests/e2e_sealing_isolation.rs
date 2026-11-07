// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A sealed disk opens only with its own vTPM, on libvirt: the cross-member
//! negative test (roadmap 18 invariant 2; ADR-0088), the libvirt port of
//! `banlieue-provider-cloud-hypervisor/tests/e2e_sealing_isolation.rs`.
//!
//! First-boot sealing gives every clone of one image the same LUKS header
//! UUID (ADR-0088, *Evidence from a live run*), because every clone formats
//! the same partition of the same image. That is harmless only if each
//! clone's volume key is its own and is sealed to its own vTPM. A suite that
//! only shows members *can* seal would also pass with a fleet-shared key;
//! this one shows a sealed disk *cannot* be opened by any other vTPM:
//!
//! 1. member A is sealed through banlieue (`GuestReady=True`,
//!    `sealedPartitions: [COS_PERSISTENT]`) and its probe shows
//!    `COS_PERSISTENT` mounted from a device-mapper device: unlocked;
//! 2. A is powered off and its sealed disk cloned, flattened, into a new
//!    volume in the same pool;
//! 3. a `VMImage` whose `BackingFile` source is that volume boots as a new
//!    machine, with the fresh vTPM state libvirt gives every new domain
//!    (swtpm keys its state by domain UUID): it must never unlock
//!    `COS_PERSISTENT`, and its `GuestReady` must end `False`,
//!    `FirstBootSealingTimeout`;
//! 4. A is powered on again, with its own vTPM, and unlocks: so step 3
//!    failed because of the vTPM, not because the disk was damaged.
//!
//! **Production topology, no host shell** (`rules/testing.md`, rule 3): the
//! provider is banlieue's own, running as it is deployed; this suite reads
//! and copies over the same mutual-TLS libvirt connection a provider uses
//! (`LIBVIRT_TLS_DIR` holds a client certificate the host trusts). It never
//! reaches the vTPM state and never runs a command in a guest: the probe is
//! read with the agent's read-only `guest-file-open`/`read`, exactly as the
//! provider reads the sealing report (ADR-0043).
//!
//! ```sh
//! export KUBECONFIG=~/.kube/<cluster>.yaml
//! BANLIEUE_E2E_PROVIDER=<provider>   # libvirt, features: [vtpm]; its host has swtpm
//! BANLIEUE_E2E_IMAGE=<vmimage>       # firstBootSealing, with the probe below
//! BANLIEUE_E2E_CLASS=<vmclass>       # tpmEnabled
//! LIBVIRT_HOST=bar.foo.io            # the Provider's host, as its cert names it
//! LIBVIRT_TLS_DIR="$HOME/.config/banlieue/<host>/libvirt"
//!   make libvirt-sealing-isolation-e2e
//! ```
//!
//! **The image needs one probe stage**, so the guest says whether it
//! unlocked its persistent partition. The sealing report cannot answer that:
//! it says `COS_PERSISTENT` *is* a LUKS container, which a disk copied from
//! a sealed guest also is. Add this layer to the image's cloud-config (it is
//! test-only; production images do not need it):
//!
//! ```yaml
//! #cloud-config
//! stages:
//!   boot:
//!     - name: banlieue-e2e-persistent-probe
//!       if: "[ -f /run/cos/active_mode ] || [ -f /run/cos/passive_mode ]"
//!       commands:
//!         - mkdir -p /run/banlieue
//!         - echo "$(findmnt -no SOURCE /usr/local || echo none)" > /run/banlieue/e2e-persistent
//! ```
//!
//! `/run` is a tmpfs, so a probe read after a power-on is from that boot.
//!
//! **Step 3's evidence is differential here.** banlieue's libvirt domains
//! attach the serial console as a pty, with no log, so unlike the Cloud
//! Hypervisor suite this one cannot read Kairos's own unlock failure
//! (`failed to unlock partition COS_PERSISTENT_LUKS`): it proves the copy
//! never unlocked, and that the same disk still does under its own vTPM.
//!
//! Optional: `BANLIEUE_E2E_NAMESPACE` (default `banlieue-system`, and it
//! must be the Provider's namespace).
//!
//! Every run names its objects with a unique prefix, removes exactly those
//! (and the cloned volume) including after a failure, and reports anything
//! the teardown left on the host. Unset variables and an unreachable host
//! fail the run: this suite never skips.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use banlieue_api::banlieue::{Provider, VMImage, VirtualMachine};
use banlieue_api::common::PowerState;
use banlieue_api::infrastructure::LibvirtMachine;
use banlieue_libvirt::{
    AGENT_TIMEOUT_DEFAULT, DEFAULT_TLS_PORT, Domain, Session, StoragePool, StorageVol, TlsIdentity,
    connect_open, connect_tls, domain_get_state, domain_lookup_by_name, domain_qemu_agent_command,
    is_not_found, raw_volume_xml, storage_pool_lookup_by_name, storage_vol_create_xml_from,
    storage_vol_delete, storage_vol_lookup_by_name,
};
use banlieue_provider_libvirt::guest::{
    guest_file_close_cmd, guest_file_open_cmd, guest_file_read_cmd, parse_file_handle,
};
use banlieue_provider_libvirt::reconciler::libvirtmachine::volume_name;
use futures::FutureExt as _;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::Client;
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncWrite};

/// A sealed member: first boot, first-boot encryption, report. A pool of
/// two warmed in 125 s raw and 65 s pre-laid on libvirt (ADR-0088,
/// ADR-0113); a lone member is quicker.
const PROVISION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Power off, power on, the clone, teardown: seconds to a few minutes.
const TRANSITION_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const POLL: Duration = Duration::from_secs(5);
/// The copy's `firstBootSealing.timeoutSeconds`: long enough that a guest
/// which *could* unlock would have reported (a sealed member reports well
/// within it), short enough to keep the run quick. The admission minimum is
/// 60.
const FOREIGN_VTPM_SEALING_TIMEOUT_SECS: u64 = 300;
/// How long past the copy's deadline to wait for `FirstBootSealingTimeout`:
/// the provider polls while a sealing report is due.
const DEADLINE_SLACK: Duration = Duration::from_secs(120);
/// Where the image's probe stage writes what `/usr/local` is mounted from.
const PROBE_PATH: &str = "/run/banlieue/e2e-persistent";
/// Largest probe read back: one device path.
const PROBE_READ_MAX: usize = 4096;
/// What an unlocked `COS_PERSISTENT` is mounted from.
const MAPPED_SOURCE_PREFIX: &str = "/dev/mapper/";
const SEALED_PARTITION: &str = "COS_PERSISTENT";
const DEFAULT_NAMESPACE: &str = "banlieue-system";
const LIBVIRT_URI: &str = "qemu:///system";
const PROVIDER_CLASS: &str = "libvirt";
const CONDITION_GUEST_READY: &str = "GuestReady";
const REASON_SEALING_TIMEOUT: &str = "FirstBootSealingTimeout";
/// The Provider feature a `tpmEnabled` class needs to be placed at all.
const FEATURE_VTPM: &str = "vtpm";

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

struct Settings {
    provider: String,
    image: String,
    class: String,
    namespace: String,
    libvirt_host: String,
    tls_dir: PathBuf,
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is not set; see the module docs of tests/e2e_sealing_isolation.rs")
    })
}

fn settings() -> Settings {
    let dir = required("LIBVIRT_TLS_DIR");
    let dir = match (dir.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => dir,
    };
    Settings {
        provider: required("BANLIEUE_E2E_PROVIDER"),
        image: required("BANLIEUE_E2E_IMAGE"),
        class: required("BANLIEUE_E2E_CLASS"),
        namespace: std::env::var("BANLIEUE_E2E_NAMESPACE")
            .unwrap_or_else(|_| DEFAULT_NAMESPACE.into()),
        libvirt_host: required("LIBVIRT_HOST"),
        tls_dir: PathBuf::from(dir),
    }
}

fn load_identity(dir: &Path) -> TlsIdentity {
    let read = |name: &str| {
        std::fs::read(dir.join(name))
            .unwrap_or_else(|e| panic!("reading {}/{name}: {e}", dir.display()))
    };
    TlsIdentity {
        ca_pem: read("ca.pem"),
        client_cert_pem: read("client-cert.pem"),
        client_key_pem: read("client-key.pem"),
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

fn unlocked(probe: &str) -> bool {
    probe.starts_with(MAPPED_SOURCE_PREFIX)
}

/// The probe's text from a `guest-file-read` reply: `return.buf-b64`,
/// decoded and trimmed. `None` for anything else.
fn probe_from_read(reply: &str) -> Option<String> {
    use base64::Engine as _;
    let v: serde_json::Value = serde_json::from_str(reply).ok()?;
    let b64 = v.get("return")?.get("buf-b64")?.as_str()?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

// ---------------------------------------------------------------------------
// One run's objects
// ---------------------------------------------------------------------------

struct Scratch {
    client: Client,
    s: Settings,
    prefix: String,
}

/// Where a machine lives on the host, from its `LibvirtMachine`.
struct Guest {
    domain: String,
    pool: String,
    os_volume: String,
}

impl Scratch {
    async fn new() -> Self {
        let s = settings();
        let client = Client::try_default()
            .await
            .expect("no reachable cluster: set KUBECONFIG");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after the epoch")
            .subsec_nanos();
        let prefix = format!("e2e-lv-iso-{:x}{nanos:x}", std::process::id());
        eprintln!("  namespace: {}, prefix: {prefix}", s.namespace);
        Self { client, s, prefix }
    }

    fn name(&self, what: &str) -> String {
        format!("{}-{what}", self.prefix)
    }

    fn vms(&self) -> Api<VirtualMachine> {
        Api::namespaced(self.client.clone(), &self.s.namespace)
    }

    fn machines(&self) -> Api<LibvirtMachine> {
        Api::namespaced(self.client.clone(), &self.s.namespace)
    }

    fn images(&self) -> Api<VMImage> {
        Api::all(self.client.clone())
    }

    /// The cloned volume: raw, so the provider's overlay declares its
    /// backing format as raw (`backing_format` reads the extension).
    fn copy_volume_name(&self) -> String {
        format!("{}.raw", self.name("a-disk"))
    }

    /// A fresh libvirt session: one per step, so a long wait never holds a
    /// connection the host may have dropped.
    async fn libvirt(&self) -> Session<impl AsyncRead + AsyncWrite + Unpin> {
        let identity = load_identity(&self.s.tls_dir);
        let mut session = connect_tls(&self.s.libvirt_host, DEFAULT_TLS_PORT, &identity)
            .await
            .unwrap_or_else(|e| panic!("TLS to libvirt on {}: {e}", self.s.libvirt_host));
        connect_open(&mut session, Some(LIBVIRT_URI), false)
            .await
            .expect("CONNECT_OPEN");
        session
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
        // Without it the controller places nothing (`feature 'vtpm' not
        // available`) and step 1 waits out its whole timeout.
        assert!(
            p.spec
                .capabilities
                .features
                .iter()
                .any(|f| f == FEATURE_VTPM),
            "Provider {} does not declare spec.capabilities.features: [{FEATURE_VTPM}]; \
             a tpmEnabled class cannot be placed on it",
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
        drop(self.libvirt().await);
        eprintln!(
            "  ✓ Provider {} Ready, VMImage {} seals and is ready for it, libvirt reachable",
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

    async fn machine(&self, vm: &str) -> Option<LibvirtMachine> {
        self.machines().get_opt(vm).await.ok()?
    }

    async fn guest(&self, vm: &str) -> Option<Guest> {
        let m = self.machine(vm).await?;
        let os = m.spec.disks.first()?;
        Some(Guest {
            os_volume: volume_name(&m.spec.domain_name, &os.name),
            domain: m.spec.domain_name,
            pool: m.spec.pool,
        })
    }

    async fn wait_sealed(&self, vm: &str) {
        wait_for(
            &format!("{vm} to seal and announce"),
            PROVISION_TIMEOUT,
            || async {
                let st = self.machine(vm).await?.status?;
                (condition_true(&st.conditions, CONDITION_GUEST_READY)
                    && st.sealed_partitions.iter().any(|p| p == SEALED_PARTITION))
                .then_some(())
            },
        )
        .await;
    }

    async fn wait_power(&self, vm: &str, want: PowerState) {
        wait_for(
            &format!("{vm} observed {want:?}"),
            TRANSITION_TIMEOUT,
            || async {
                let st = self.machine(vm).await?.status?;
                (st.observed_power_state.as_ref() == Some(&want)).then_some(())
            },
        )
        .await;
    }

    /// The domain, if libvirtd has it.
    async fn domain<S>(&self, session: &mut Session<S>, name: &str) -> Option<Domain>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        match domain_lookup_by_name(session, name).await {
            Ok(d) => Some(d),
            Err(e) if is_not_found(&e) => None,
            Err(e) => panic!("looking up domain {name}: {e}"),
        }
    }

    /// Wait until libvirtd itself says the domain is not running: a disk is
    /// never cloned out from under a running guest.
    async fn wait_domain_stopped(&self, g: &Guest) {
        wait_for(
            &format!("domain {} stopped", g.domain),
            TRANSITION_TIMEOUT,
            || async {
                let mut session = self.libvirt().await;
                let dom = self.domain(&mut session, &g.domain).await?;
                let state = domain_get_state(&mut session, &dom)
                    .await
                    .expect("DOMAIN_GET_STATE");
                (!state.is_running()).then_some(())
            },
        )
        .await;
    }

    /// The guest's probe, read through the agent; `None` while there is no
    /// agent (still booting, or halted in its initramfs) or no probe yet.
    async fn read_probe(&self, g: &Guest) -> Option<String> {
        let mut session = self.libvirt().await;
        let dom = self.domain(&mut session, &g.domain).await?;
        let open = guest_file_open_cmd(PROBE_PATH);
        let reply = domain_qemu_agent_command(&mut session, &dom, &open, AGENT_TIMEOUT_DEFAULT)
            .await
            .ok()??;
        let handle = parse_file_handle(&reply)?;
        let read = guest_file_read_cmd(handle, PROBE_READ_MAX);
        let read_reply =
            domain_qemu_agent_command(&mut session, &dom, &read, AGENT_TIMEOUT_DEFAULT).await;
        // Always close: leaking guest file handles exhausts the agent's table.
        let close = guest_file_close_cmd(handle);
        let _ = domain_qemu_agent_command(&mut session, &dom, &close, AGENT_TIMEOUT_DEFAULT).await;
        probe_from_read(&read_reply.ok()??)
    }

    async fn wait_probe(&self, what: &str, g: &Guest) -> String {
        wait_for(what, PROVISION_TIMEOUT, || self.read_probe(g)).await
    }

    async fn pool<S>(&self, session: &mut Session<S>, name: &str) -> StoragePool
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        storage_pool_lookup_by_name(session, name)
            .await
            .unwrap_or_else(|e| panic!("storage pool {name}: {e}"))
    }

    /// Clone the stopped guest's sealed OS disk into a new raw volume in the
    /// same pool, flattened (libvirtd converts on the host).
    async fn copy_disk(&self, g: &Guest) -> String {
        let mut session = self.libvirt().await;
        let pool = self.pool(&mut session, &g.pool).await;
        let source = storage_vol_lookup_by_name(&mut session, &pool, &g.os_volume)
            .await
            .unwrap_or_else(|e| panic!("A's OS volume {}: {e}", g.os_volume));
        let name = self.copy_volume_name();
        // Capacity is a floor libvirtd raises to the source's size.
        let xml = raw_volume_xml(&name, 1);
        let copy = storage_vol_create_xml_from(&mut session, &pool, &xml, &source)
            .await
            .unwrap_or_else(|e| panic!("cloning {} into {name}: {e}", g.os_volume));
        copy.key
    }

    /// A `VMImage` for the copy: the fixture's spec, with that volume as its
    /// only source and a shorter sealing deadline.
    async fn make_copy_image(&self, fixture: &VMImage, name: &str) {
        let mut spec = serde_json::to_value(&fixture.spec).expect("fixture spec");
        let mut source = spec["sources"]
            .as_array()
            .and_then(|s| {
                s.iter()
                    .find(|x| x["providerClass"] == PROVIDER_CLASS)
                    .cloned()
            })
            .expect("the fixture VMImage has no libvirt source");
        source["kind"] = json!("BackingFile");
        source["ref"] = json!(self.copy_volume_name());
        if let Some(obj) = source.as_object_mut() {
            obj.remove("importFrom");
            obj.remove("checksum");
        }
        spec["sources"] = json!([source]);
        spec["firstBootSealing"]["timeoutSeconds"] = json!(FOREIGN_VTPM_SEALING_TIMEOUT_SECS);
        if let Some(obj) = spec.as_object_mut() {
            // The copy is already laid out; nothing is built or pre-laid.
            obj.remove("prelay");
            obj.remove("cloudConfigs");
        }
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

    /// Delete this run's objects and the cloned volume; report anything
    /// left.
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

        let mut session = self.libvirt().await;
        let pool_name = guests.first().map(|(_, g)| g.pool.clone());
        let mut left = Vec::new();
        if let Some(pool_name) = pool_name {
            let pool = self.pool(&mut session, &pool_name).await;
            let copy = self.copy_volume_name();
            if let Some(vol) = lookup_volume(&mut session, &pool, &copy).await
                && let Err(e) = storage_vol_delete(&mut session, &vol).await
            {
                left.push(format!("volume {copy} ({e})"));
            }
            for (vm, g) in &guests {
                if self.domain(&mut session, &g.domain).await.is_some() {
                    left.push(format!("domain {} (of {vm})", g.domain));
                }
                if lookup_volume(&mut session, &pool, &g.os_volume)
                    .await
                    .is_some()
                {
                    left.push(format!("volume {} (of {vm})", g.os_volume));
                }
            }
        }
        if !left.is_empty() {
            eprintln!("  ⚠ LEFT ON HOST: {}", left.join(", "));
        }
    }
}

async fn lookup_volume<S>(
    session: &mut Session<S>,
    pool: &StoragePool,
    name: &str,
) -> Option<StorageVol>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match storage_vol_lookup_by_name(session, pool, name).await {
        Ok(v) => Some(v),
        Err(e) if is_not_found(&e) => None,
        Err(e) => panic!("looking up volume {name}: {e}"),
    }
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// A sealed member's disk does not open with any vTPM but its own.
#[tokio::test]
#[ignore = "needs a cluster running banlieue and a libvirt host with swtpm: see the module docs"]
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
    let ga = s.guest(a).await.expect("member A's LibvirtMachine");
    let probe = s
        .wait_probe("A's probe of its persistent partition", &ga)
        .await;
    assert!(
        unlocked(&probe),
        "member A sealed but its persistent partition is not unlocked ({probe}); \
         the image's probe or its sealing is wrong"
    );
    eprintln!("  ✓ A unlocked COS_PERSISTENT with its own vTPM ({probe})");

    // 2. A off; its sealed disk cloned.
    s.set_power(a, "PoweredOff").await;
    s.wait_power(a, PowerState::PoweredOff).await;
    s.wait_domain_stopped(&ga).await;
    let copy = s.copy_disk(&ga).await;
    eprintln!("  ✓ A's sealed disk cloned to {copy}");

    // 3. The copy, with a vTPM that is not A's, never unlocks or announces.
    s.make_copy_image(fixture, copy_image).await;
    s.make_vm(foreign, copy_image).await;
    s.wait_power(foreign, PowerState::PoweredOn).await;
    let gf = s.guest(foreign).await.expect("the copy's LibvirtMachine");
    let deadline = Duration::from_secs(FOREIGN_VTPM_SEALING_TIMEOUT_SECS) + DEADLINE_SLACK;
    let reason = wait_for(
        "the copy to time out without unlocking (FirstBootSealingTimeout)",
        deadline,
        || async {
            if let Some(p) = s.read_probe(&gf).await.filter(|p| unlocked(p)) {
                panic!("ISOLATION BROKEN: A's sealed disk unlocked under another vTPM ({p})");
            }
            let st = s.machine(foreign).await?.status?;
            let ready = condition(&st.conditions, CONDITION_GUEST_READY)?;
            assert_ne!(
                ready.status, "True",
                "the copy is GuestReady without its persistent partition unlocked ({}): \
                 a guest whose data volume did not open must not be handed out",
                ready.reason
            );
            (ready.reason == REASON_SEALING_TIMEOUT).then(|| ready.reason.clone())
        },
    )
    .await;
    eprintln!("  ✓ the copy did not unlock under a fresh vTPM: GuestReady=False, {reason}");
    s.vms()
        .delete(foreign, &DeleteParams::background())
        .await
        .expect("delete the copy's VirtualMachine");

    // 4. A again, with its own vTPM: the disk was not damaged. /run is a
    // tmpfs, so any probe read now is from this boot.
    s.set_power(a, "PoweredOn").await;
    s.wait_power(a, PowerState::PoweredOn).await;
    let again = s
        .wait_probe("A's probe after power-on, with its own vTPM", &ga)
        .await;
    assert!(
        unlocked(&again),
        "A no longer unlocks with its own vTPM ({again}): the copy step, not the vTPM, \
         may explain step 3"
    );
    eprintln!("  ✓ A unlocked again with its own vTPM ({again})");
    eprintln!("  RESULT a sealed disk opens with its own vTPM only");
}
