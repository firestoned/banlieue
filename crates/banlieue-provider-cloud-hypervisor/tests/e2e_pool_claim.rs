// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Pool → Cloud Hypervisor guests → claim → release, end to end on the host
//! (ADR-0046/0047; roadmap 17 phase G).
//!
//! The libvirt suite of the same name proves the pool and claim mechanics
//! against libvirtd. This one proves the same sentence on a Cloud
//! Hypervisor host, where "the sandbox" is a guest unit, a tap and two
//! directories rather than a domain: **"claim deleted" means "sandbox
//! destroyed"**, and **"pool deleted" leaves a claimed sandbox running**.
//! It also prints how long the pool took to become warm, which is the number
//! roadmap 17 phase G records next to vSphere's 130.3 s.
//!
//! **Runs on the host itself**, like `e2e_machine.rs`: the units, taps and
//! directories it checks are there. It assumes banlieue is installed — the
//! controller running, this host's provider running and its `Provider`
//! `Ready`, a `VMImage` ready for it and a `VMClass` the host serves.
//!
//! ```sh
//! export KUBECONFIG=~/.kube/<cluster>.yaml
//! BANLIEUE_E2E_PROVIDER=<provider>          # this host's Provider
//! BANLIEUE_E2E_IMAGE=<vmimage>              # ready for that Provider
//! BANLIEUE_E2E_CLASS=<vmclass>
//! BANLIEUE_E2E_STORAGE_DIR=/srv/banlieue/ch # the class's directory on this host
//!   cargo test -p banlieue-provider-cloud-hypervisor --test e2e_pool_claim \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Optional:
//!
//! - `BANLIEUE_E2E_READINESS` — the pool's `spec.readiness`, default
//!   `InfrastructureReady`. Use `GuestReady` for a `tpmEnabled` class with a
//!   `Deferred` image: that is the phase G configuration, and the only one
//!   where the member is not claimable until the installed system reports.
//! - `BANLIEUE_E2E_USER_DATA` — a cloud-config file every member gets. A
//!   `Deferred` image needs one (the install itself is user-data; see
//!   `e2e_deferred.rs` for a Kairos install that reports over vsock).
//! - `BANLIEUE_E2E_NAMESPACE` — default `banlieue-system`; must be the
//!   Provider's namespace, since members only schedule against Providers in
//!   their own namespace.
//! - `BANLIEUE_E2E_RUN_ROOT` — default `/run/banlieue/ch`.
//!
//! Or `make ch-pool-claim-e2e`.
//!
//! Every run names its objects with a unique prefix and removes exactly
//! those, including after a failure, and reports anything the teardown left
//! on the host rather than swallowing it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use banlieue_api::banlieue::{
    LABEL_CLAIM, LABEL_POOL, Provider, VMImage, VirtualMachine, VirtualMachineClaim,
    VirtualMachinePool, pool_condition_types,
};
use banlieue_api::infrastructure::CloudHypervisorMachine;
use banlieue_provider_cloud_hypervisor::systemd::{Bus, Systemd};
use futures::FutureExt as _;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::api::{Api, DeleteParams, ListParams, PostParams};
use kube::{Client, ResourceExt};
use serde_json::json;

/// A `Deferred` member is an unattended install, a reboot and first-boot
/// encryption: 341 s for one Kairos install in `make ch-deferred-e2e`. Two
/// members install concurrently on one host, so allow well over double.
const PROVISION_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// Teardown crosses two finalizers and a unit stop; 5–6 s observed for one
/// machine.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const POLL: Duration = Duration::from_secs(5);

/// Warm members the pool keeps. Two, not one: with one member a refill and
/// a claim are indistinguishable.
const WARM: u32 = 2;
/// Long enough that expiry never races the test (expiry is `live_claim.rs`).
const CLAIM_TTL_SECS: u64 = 3600;
/// The pool's `provisioningTimeoutSeconds`: a member still provisioning
/// after this long is poisoned and replaced (ADR-0046). An encrypted Kairos
/// install reports in ~140 s; about one in seven hits a Cloud Hypervisor
/// vTPM I/O error mid-install and never does (roadmap 17 phase G). The
/// default, 1800 s, outlasts `PROVISION_TIMEOUT`, so the suite would give up
/// before the pool heals; 600 s lets one replacement land inside it.
const MEMBER_PROVISIONING_TIMEOUT_SECS: u64 = 600;
/// Tap names are `bch` + the first 10 hex digits of the machine UID + NIC.
const TAP_PREFIX: &str = "bch";
const TAP_UID_DIGITS: usize = 10;
const DEFAULT_RUN_ROOT: &str = "/run/banlieue/ch";
const DEFAULT_NAMESPACE: &str = "banlieue-system";
const DEFAULT_READINESS: &str = "InfrastructureReady";
const USER_DATA_KEY: &str = "user-data";

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
    readiness: String,
    user_data: Option<String>,
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is not set; see the module docs of tests/e2e_pool_claim.rs")
    })
}

fn settings() -> Settings {
    let user_data = std::env::var("BANLIEUE_E2E_USER_DATA").ok().map(|p| {
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("BANLIEUE_E2E_USER_DATA {p}: {e}"))
    });
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
        readiness: std::env::var("BANLIEUE_E2E_READINESS")
            .unwrap_or_else(|_| DEFAULT_READINESS.into()),
        user_data,
    }
}

// ---------------------------------------------------------------------------
// Waiting
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

/// `wait_for` that reports failure instead of panicking: teardown must
/// finish its remaining steps even when one of them times out.
async fn wait_for_opt<F, Fut>(what: &str, timeout: Duration, mut check: F) -> Option<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<()>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if check().await.is_some() {
            eprintln!("  ✓ {what}");
            return Some(());
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(POLL).await;
    }
}

fn condition_true(conditions: &[Condition], kind: &str) -> bool {
    conditions
        .iter()
        .any(|c| c.type_ == kind && c.status == "True")
}

// ---------------------------------------------------------------------------
// The host
// ---------------------------------------------------------------------------

/// What one member is on this host: enough to find every trace of it again
/// after the API objects are gone.
#[derive(Debug, Clone)]
struct Guest {
    member: String,
    uid: String,
    unit: String,
    machine_dir: PathBuf,
    run_dir: PathBuf,
}

/// Host interfaces that are this machine's taps.
fn taps_for(uid: &str) -> Vec<String> {
    let hex: String = uid
        .chars()
        .filter(|c| *c != '-')
        .take(TAP_UID_DIGITS)
        .collect();
    let prefix = format!("{TAP_PREFIX}{hex}");
    std::fs::read_dir("/sys/class/net")
        .expect("reading /sys/class/net")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with(&prefix))
        .collect()
}

/// Whether `p` exists, without needing to read inside it (the directories
/// are `2770 guest:banlieue`; their `0711` parents let anyone stat them).
fn present(p: &Path) -> bool {
    match std::fs::symlink_metadata(p) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("stat {}: {e}", p.display()),
    }
}

/// Every trace of `g` still on the host. Empty means the guest is gone.
async fn traces(sd: &Systemd, g: &Guest) -> Vec<String> {
    let mut left = Vec::new();
    if let Some(st) = sd.state(&g.unit).await.expect("reading unit state") {
        left.push(format!("unit {} ({st:?})", g.unit));
    }
    left.extend(taps_for(&g.uid).into_iter().map(|t| format!("tap {t}")));
    for dir in [&g.machine_dir, &g.run_dir] {
        if present(dir) {
            left.push(format!("directory {}", dir.display()));
        }
    }
    left
}

/// Whether `g` is running here: its unit active, its tap and directories in
/// place. Returns what is missing.
async fn missing(sd: &Systemd, g: &Guest) -> Vec<String> {
    use banlieue_provider_cloud_hypervisor::systemd::UnitState;
    let mut gaps = Vec::new();
    let state = sd.state(&g.unit).await.expect("reading unit state");
    if state != Some(UnitState::Active) {
        gaps.push(format!("unit {} is {state:?}", g.unit));
    }
    if taps_for(&g.uid).is_empty() {
        gaps.push("no tap".into());
    }
    for dir in [&g.machine_dir, &g.run_dir] {
        if !present(dir) {
            gaps.push(format!("no directory {}", dir.display()));
        }
    }
    gaps
}

// ---------------------------------------------------------------------------
// One run's objects
// ---------------------------------------------------------------------------

/// One run's objects, in the Provider's namespace, isolated by name prefix.
struct Scratch {
    client: Client,
    s: Settings,
    prefix: String,
    sd: Systemd,
}

impl Scratch {
    async fn new(label: &str) -> Self {
        let s = settings();
        let client = Client::try_default()
            .await
            .expect("no reachable cluster — set KUBECONFIG");
        let sd = Systemd::connect(Bus::System)
            .await
            .expect("the system bus: this suite runs on the Provider's host");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after the epoch")
            .subsec_nanos();
        let prefix = format!("e2e-ch-{label}-{:x}{nanos:x}", std::process::id());
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

    fn api<K>(&self) -> Api<K>
    where
        K: kube::Resource<Scope = k8s_openapi::NamespaceResourceScope>,
        <K as kube::Resource>::DynamicType: Default,
    {
        Api::namespaced(self.client.clone(), &self.s.namespace)
    }

    /// Members of *this run's* pool, by the pool label.
    async fn member_names(&self, pool: &str) -> Vec<String> {
        self.api::<VirtualMachine>()
            .list(&ListParams::default().labels(&format!("{LABEL_POOL}={pool}")))
            .await
            .map(|l| l.into_iter().map(|vm| vm.name_any()).collect())
            .unwrap_or_default()
    }

    /// Fail in seconds, with a clear message, on a fixture that cannot work.
    async fn require_fixture(&self) {
        let p = self
            .api::<Provider>()
            .get_opt(&self.s.provider)
            .await
            .expect("reading Providers")
            .unwrap_or_else(|| {
                panic!(
                    "Provider {} not found in namespace {}; set BANLIEUE_E2E_NAMESPACE \
                     to where it lives",
                    self.s.provider, self.s.namespace
                )
            });
        assert!(
            p.status
                .as_ref()
                .is_some_and(|st| condition_true(&st.conditions, "Ready")),
            "Provider {} is not Ready; this suite tests pools, not the install",
            self.s.provider
        );
        let image = Api::<VMImage>::all(self.client.clone())
            .get(&self.s.image)
            .await
            .unwrap_or_else(|e| panic!("VMImage {}: {e}", self.s.image));
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
            "  ✓ Provider {} Ready, VMImage {} ready for it",
            self.s.provider, self.s.image
        );
    }

    /// The member's guest on this host, once its machine has a host uid.
    async fn guest(&self, member: &str) -> Option<Guest> {
        let m = self
            .api::<CloudHypervisorMachine>()
            .get_opt(member)
            .await
            .ok()??;
        let uid = m.uid()?;
        let host_uid = m.status.as_ref()?.host_uid?;
        Some(Guest {
            member: member.to_string(),
            unit: format!("banlieue-ch@{host_uid}.service"),
            machine_dir: self.s.storage_dir.join(&uid),
            run_dir: self.s.run_root.join(host_uid.to_string()),
            uid,
        })
    }

    async fn make_pool(&self, pool: &str) {
        let mut template = json!({
            "classRef": { "name": self.s.class },
            "imageRef": { "name": self.s.image },
            // Pin members to this host whatever else the Provider serves:
            // the provider labels its failure domain `name=<Provider>`.
            "placement": {
                "failureDomainSelector": { "matchLabels": { "name": self.s.provider } }
            },
            "desiredPowerState": "PoweredOn",
            "migrationPolicy": "never",
        });
        if let Some(data) = &self.s.user_data {
            let cm: ConfigMap = serde_json::from_value(json!({
                "metadata": { "name": pool },
                "data": { USER_DATA_KEY: data },
            }))
            .expect("user-data ConfigMap");
            self.api::<ConfigMap>()
                .create(&PostParams::default(), &cm)
                .await
                .expect("create user-data ConfigMap");
            template["userData"] = json!({ "configMapRef": { "name": pool } });
        }
        let obj: VirtualMachinePool = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "VirtualMachinePool",
            "metadata": { "name": pool },
            "spec": {
                "warmReplicas": WARM,
                "maxReplicas": WARM + 2,
                "maxSurge": WARM,
                "provisioningTimeoutSeconds": MEMBER_PROVISIONING_TIMEOUT_SECS,
                "readiness": self.s.readiness,
                "template": { "spec": template },
            },
        }))
        .expect("pool fixture");
        self.api::<VirtualMachinePool>()
            .create(&PostParams::default(), &obj)
            .await
            .expect("create pool");
    }

    async fn make_claim(&self, claim: &str, pool: &str) {
        let obj: VirtualMachineClaim = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "VirtualMachineClaim",
            "metadata": { "name": claim },
            "spec": {
                "poolRef": { "name": pool },
                "subject": {
                    "issuer": "https://issuer.example.com",
                    "id": "e2e-subject-0c3b7f2e",
                },
                "ttlSeconds": CLAIM_TTL_SECS,
            },
        }))
        .expect("claim fixture");
        self.api::<VirtualMachineClaim>()
            .create(&PostParams::default(), &obj)
            .await
            .expect("create claim");
    }

    async fn wait_bound(&self, claim: &str) -> String {
        wait_for("the claim to bind a member", TEARDOWN_TIMEOUT, || async {
            self.api::<VirtualMachineClaim>()
                .get(claim)
                .await
                .ok()?
                .status?
                .virtual_machine_ref
                .map(|r| r.name)
        })
        .await
    }

    /// `(available, Warm=True)` for the pool.
    async fn warmth(&self, pool: &str) -> (u32, bool) {
        self.api::<VirtualMachinePool>()
            .get(pool)
            .await
            .ok()
            .and_then(|p| p.status)
            .map_or((0, false), |st| {
                (
                    st.available,
                    condition_true(&st.conditions, pool_condition_types::WARM),
                )
            })
    }

    /// Wait for `Warm=True` with `available >= WARM`, and return how long it
    /// took from `since`.
    async fn wait_warm(&self, pool: &str, what: &str, since: Instant) -> Duration {
        wait_for(what, PROVISION_TIMEOUT, || async {
            let (available, warm) = self.warmth(pool).await;
            (warm && available >= WARM).then_some(())
        })
        .await;
        since.elapsed()
    }

    /// Delete this run's objects and report anything left on the host.
    ///
    /// Never trusts the API alone: a member whose finalizer was forced, or a
    /// machine that never reached the host, can leave a unit or a directory
    /// behind, and the report names it.
    async fn teardown(&self, pool: &str, guests: &[Guest]) {
        eprintln!("  tearing down {}", self.prefix);
        let claims = self.api::<VirtualMachineClaim>();
        if let Ok(list) = claims.list(&ListParams::default()).await {
            for c in list
                .into_iter()
                .filter(|c| c.name_any().starts_with(&self.prefix))
            {
                let _ = claims
                    .delete(&c.name_any(), &DeleteParams::background())
                    .await;
            }
        }
        let _ = self
            .api::<VirtualMachinePool>()
            .delete(pool, &DeleteParams::background())
            .await;
        let _ = self
            .api::<ConfigMap>()
            .delete(pool, &DeleteParams::default())
            .await;

        let gone = wait_for_opt("every member to be deleted", TEARDOWN_TIMEOUT, || async {
            self.member_names(pool).await.is_empty().then_some(())
        })
        .await;
        if gone.is_none() {
            eprintln!(
                "  ⚠ members still present: {:?}",
                self.member_names(pool).await
            );
        }
        for g in guests {
            let left = traces(&self.sd, g).await;
            if !left.is_empty() {
                eprintln!("  ⚠ LEFT ON HOST by {}: {}", g.member, left.join(", "));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A pool of real guests warms, one is handed out and the pool refills, and
/// releasing the claim removes that guest from the host.
#[tokio::test]
#[ignore = "needs a Cloud Hypervisor host running banlieue: see the module docs"]
async fn a_claimed_guest_is_destroyed_on_the_host_when_the_claim_is_released() {
    let s = Scratch::new("release").await;
    s.require_fixture().await;
    let pool = s.name("pool");
    let mut guests = Vec::new();

    let result = std::panic::AssertUnwindSafe(release_case(&s, &pool, &mut guests))
        .catch_unwind()
        .await;
    s.teardown(&pool, &guests).await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

async fn release_case(s: &Scratch, pool: &str, guests: &mut Vec<Guest>) {
    let start = Instant::now();
    s.make_pool(pool).await;
    let warm_up = s.wait_warm(pool, "the pool to reach Warm", start).await;

    let members = s.member_names(pool).await;
    assert_eq!(members.len() as u32, WARM, "members: {members:?}");
    for m in &members {
        let g = s
            .guest(m)
            .await
            .unwrap_or_else(|| panic!("member {m} is claimable but its machine has no host uid"));
        let gaps = missing(&s.sd, &g).await;
        assert!(
            gaps.is_empty(),
            "member {m} is claimable but is not running here: {}",
            gaps.join(", ")
        );
        guests.push(g);
    }
    eprintln!("  ✓ both warm members are running guests on this host");

    // --- claim one ---------------------------------------------------------
    let claim = s.name("claim");
    s.make_claim(&claim, pool).await;
    let bound = s.wait_bound(&claim).await;
    assert!(members.contains(&bound), "bound a member of this pool");
    let member = s
        .api::<VirtualMachine>()
        .get(&bound)
        .await
        .expect("get bound member");
    assert_eq!(member.labels().get(LABEL_CLAIM), Some(&claim));
    let owners: Vec<&str> = member
        .owner_references()
        .iter()
        .map(|o| o.kind.as_str())
        .collect();
    assert_eq!(
        owners,
        vec!["VirtualMachineClaim"],
        "ADR-0047 Decision 3: the claim must be the member's sole owner"
    );
    let bound_guest = guests
        .iter()
        .find(|g| g.member == bound)
        .cloned()
        .expect("the bound member was one of the warm ones");
    let gaps = missing(&s.sd, &bound_guest).await;
    assert!(
        gaps.is_empty(),
        "binding disturbed the running guest: {}",
        gaps.join(", ")
    );

    // --- the pool refills --------------------------------------------------
    // Wait on the replacement existing, not on `available >= WARM`, which is
    // still true from before the claim until the pool next reconciles.
    let refill = Instant::now();
    let replacement = wait_for(
        "the pool to create a replacement for the claimed member",
        PROVISION_TIMEOUT,
        || async {
            s.member_names(pool)
                .await
                .into_iter()
                .find(|m| !members.contains(m))
        },
    )
    .await;
    let refill_time = s.wait_warm(pool, "the pool to be Warm again", refill).await;
    if let Some(g) = s.guest(&replacement).await {
        guests.push(g);
    }

    // --- release -----------------------------------------------------------
    s.api::<VirtualMachineClaim>()
        .delete(&claim, &DeleteParams::default())
        .await
        .expect("delete claim");
    let release = Instant::now();
    wait_for(
        "the claim and its member to be gone",
        TEARDOWN_TIMEOUT,
        || async {
            let claim_gone = s
                .api::<VirtualMachineClaim>()
                .get_opt(&claim)
                .await
                .ok()?
                .is_none();
            let vm_gone = s
                .api::<VirtualMachine>()
                .get_opt(&bound)
                .await
                .ok()?
                .is_none();
            (claim_gone && vm_gone).then_some(())
        },
    )
    .await;

    // THE assertion: the released guest has left no trace on the host.
    let left = traces(&s.sd, &bound_guest).await;
    assert!(
        left.is_empty(),
        "ADR-0047 Decision 6 is violated: the claim is gone but the host still has \
         {}. \"Claim deleted\" must mean \"sandbox destroyed\".",
        left.join(", ")
    );
    eprintln!(
        "  ✓ the released guest is gone from the host ({}s)",
        release.elapsed().as_secs()
    );

    eprintln!(
        "\n  RESULT readiness={} warm-up({WARM} members)={}s refill(1 member)={}s\n",
        s.s.readiness,
        warm_up.as_secs(),
        refill_time.as_secs()
    );
}

/// ADR-0047 Decision 3 on real guests: deleting the pool must not take a
/// sandbox somebody is using with it.
#[tokio::test]
#[ignore = "needs a Cloud Hypervisor host running banlieue: see the module docs"]
async fn deleting_a_pool_leaves_a_claimed_guest_running() {
    let s = Scratch::new("poolgc").await;
    s.require_fixture().await;
    let pool = s.name("pool");
    let mut guests = Vec::new();

    let result = std::panic::AssertUnwindSafe(pool_gc_case(&s, &pool, &mut guests))
        .catch_unwind()
        .await;
    s.teardown(&pool, &guests).await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

async fn pool_gc_case(s: &Scratch, pool: &str, guests: &mut Vec<Guest>) {
    s.make_pool(pool).await;
    s.wait_warm(pool, "the pool to reach Warm", Instant::now())
        .await;
    for m in s.member_names(pool).await {
        if let Some(g) = s.guest(&m).await {
            guests.push(g);
        }
    }

    let claim = s.name("claim");
    s.make_claim(&claim, pool).await;
    let bound = s.wait_bound(&claim).await;
    let bound_guest = guests
        .iter()
        .find(|g| g.member == bound)
        .cloned()
        .expect("the bound member was one of the warm ones");

    s.api::<VirtualMachinePool>()
        .delete(pool, &DeleteParams::background())
        .await
        .expect("delete pool");
    wait_for(
        "the unclaimed members to be collected",
        TEARDOWN_TIMEOUT,
        || async {
            let remaining: Vec<String> = s
                .api::<VirtualMachine>()
                .list(&ListParams::default().labels(&format!("{LABEL_POOL}={pool}")))
                .await
                .ok()?
                .into_iter()
                .filter(|vm| vm.metadata.deletion_timestamp.is_none())
                .map(|vm| vm.name_any())
                .collect();
            (remaining == vec![bound.clone()]).then_some(())
        },
    )
    .await;

    let gaps = missing(&s.sd, &bound_guest).await;
    assert!(
        gaps.is_empty(),
        "deleting the pool disturbed a sandbox in use: {}",
        gaps.join(", ")
    );
    eprintln!("  ✓ the claimed guest outlived its pool");

    // The collected members left nothing behind.
    for g in guests.iter().filter(|g| g.member != bound) {
        let g = g.clone();
        wait_for(
            &format!("collected member {} to leave the host", g.member),
            TEARDOWN_TIMEOUT,
            || {
                let g = g.clone();
                async move { traces(&s.sd, &g).await.is_empty().then_some(()) }
            },
        )
        .await;
    }
}
