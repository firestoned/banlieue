// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Pool → real libvirt domains → claim → release, end to end (ADR-0046/0047).
//!
//! The three tiers below this one each cover a piece and none covers the
//! seam:
//!
//! - `claim_plan_tests.rs` — every decision, as pure functions. No cluster.
//! - `banlieue-controller/tests/live_claim.rs` — the bind race, ownership
//!   re-parenting and the finalizer, against a real API server. **No
//!   libvirt**: its members are plain CRs with a stand-in finalizer.
//! - `live_machine.rs` — domains on a real host, with no Kubernetes.
//!
//! What none of them can prove is the sentence the whole feature rests on:
//! **"claim deleted" means "sandbox destroyed"**. That is a claim about a
//! *hypervisor*, reached through a controller, a provider, an infra CR and
//! two finalizers. The only way to check it is to delete a claim and then
//! ask libvirtd whether the domain is still there — which is what this
//! suite does.
//!
//! ```sh
//! export KUBECONFIG=~/dev/kubeconfig/homelab.yaml
//! BANLIEUE_E2E_PROVIDER=<provider> \
//!   BANLIEUE_E2E_IMAGE=<ready VMImage> \
//!   BANLIEUE_E2E_CLASS=<VMClass> \
//!   LIBVIRT_HOST=bar.foo.io \
//!   LIBVIRT_TLS_DIR="$HOME/.config/banlieue/<host>/libvirt" \
//!   cargo test -p banlieue-provider-libvirt --test e2e_pool_claim \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Assumes banlieue is **already installed and running** — it tests the
//! feature, not the install — and that the named `VMImage` already reports
//! Ready, so this suite costs minutes rather than the image build's tens of
//! minutes.
//!
//! ## Running it without a deployed cluster
//!
//! No cluster with the current CRDs handy? A kind cluster plus the two
//! controllers as **local binaries** is enough, and needs no container
//! image build. Verified this way on 2026-09-20; both tests passed in
//! under a minute each.
//!
//! ```sh
//! kind create cluster --name banlieue-e2e
//! kubectl --context kind-banlieue-e2e apply -f deploy/crds/
//! kubectl --context kind-banlieue-e2e create namespace banlieue-system
//!
//! # libvirt mTLS material, as the provider expects it
//! kubectl -n banlieue-system create secret generic libvirt-creds \
//!   --from-file=tls.crt=client-cert.pem --from-file=tls.key=client-key.pem
//! kubectl -n banlieue-system create configmap libvirt-ca --from-file=ca.crt=ca.pem
//!
//! # A ProviderClass, a Provider pointing at the host, a VMClass, and a
//! # VMImage with a `BackingFile` source naming a volume ALREADY in the
//! # pool — that last part is what avoids the image build entirely.
//! kubectl apply -f <your fixtures>
//!
//! kind get kubeconfig --name banlieue-e2e > /tmp/kc.yaml
//! KUBECONFIG=/tmp/kc.yaml ./target/debug/banlieue provider libvirt --no-leader-elect &
//! KUBECONFIG=/tmp/kc.yaml ./target/debug/banlieue controller --no-leader-elect &
//! ```
//!
//! Two things to get right, both of which cost real debugging:
//!
//! - **The Provider's endpoint must match its TLS certificate.** libvirtd's
//!   server cert names specific hosts and IPs; connecting by any other name
//!   fails with `certificate not valid for name ...`. Use a name or address
//!   the cert actually carries.
//! - **`BANLIEUE_E2E_NAMESPACE` must be the Provider's namespace.** Members
//!   only schedule against Providers in their *own* namespace, so a pool
//!   anywhere else produces members that sit Pending forever — which reads
//!   as "pools are broken" rather than "the fixture is in the wrong
//!   place". The suite checks the Provider is Ready up front for this
//!   reason.
//!
//! `LIBVIRT_HOST`/`LIBVIRT_TLS_DIR` are optional: without them everything
//! Kubernetes can see is still asserted and only the on-host checks are
//! skipped, which is a strictly weaker run — the on-host check is the
//! point.
//!
//! Every run names its objects with a unique prefix and removes exactly
//! those, including after a failure. It runs in the Provider's namespace
//! rather than a generated one, so teardown is by prefix and never touches
//! anything else living there. A member that never becomes Ready leaves a
//! domain behind on the host; the teardown reports which, rather than
//! swallowing it — a half-failed teardown that claims success is how stale
//! domains accumulate.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use banlieue_api::banlieue::{
    LABEL_CLAIM, LABEL_POOL, LABEL_POOL_IMAGE_REVISION, VMImage, VirtualMachine,
    VirtualMachineClaim, VirtualMachinePool,
};
use banlieue_libvirt::{
    DEFAULT_TLS_PORT, DEVICE_MODIFY_LIVE, TlsIdentity, connect_open, connect_tls, domain_get_state,
    domain_get_xml_desc, domain_lookup_by_name, domain_update_device_flags, is_not_found,
};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::{Client, ResourceExt};
use serde_json::json;

/// Members are real VMs: a provision is minutes on a homelab host.
const PROVISION_TIMEOUT: Duration = Duration::from_secs(12 * 60);
/// Teardown crosses two finalizers and a hypervisor; give it room.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(6 * 60);
const POLL: Duration = Duration::from_secs(5);

/// Warm members the pool keeps. Two, not one: with one member a refill and
/// a claim are indistinguishable, and "the pool refilled after the claim"
/// is one of the properties under test.
const WARM: u32 = 2;
/// How long a claim holds. Long enough that expiry never races the test —
/// expiry itself is covered in `live_claim.rs`, deterministically.
const CLAIM_TTL_SECS: u64 = 3600;

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

struct Fixture {
    provider: String,
    image: String,
    class: String,
}

fn fixture() -> Option<Fixture> {
    Some(Fixture {
        provider: std::env::var("BANLIEUE_E2E_PROVIDER").ok()?,
        image: std::env::var("BANLIEUE_E2E_IMAGE").ok()?,
        class: std::env::var("BANLIEUE_E2E_CLASS").ok()?,
    })
}

fn libvirt_settings() -> Option<(String, PathBuf)> {
    let host = std::env::var("LIBVIRT_HOST").ok()?;
    let dir = std::env::var("LIBVIRT_TLS_DIR").ok()?;
    let dir = match (dir.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => dir,
    };
    Some((host, PathBuf::from(dir)))
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

/// Whether libvirtd has a domain by this name.
///
/// A lookup, not a list: "is this specific domain present" is the question,
/// and `is_not_found` distinguishes an absent domain from a broken session,
/// which a `list().contains()` would quietly conflate.
async fn domain_exists(host: &str, dir: &Path, domain: &str) -> bool {
    let identity = load_identity(dir);
    let mut session = connect_tls(host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS to the libvirt host");
    connect_open(&mut session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN");
    match banlieue_libvirt::domain_lookup_by_name(&mut session, domain).await {
        Ok(_) => true,
        Err(e) if is_not_found(&e) => false,
        Err(e) => panic!("looking up domain {domain}: {e}"),
    }
}

/// The domain name banlieue derives for a member — namespace-qualified, so
/// two namespaces can hold same-named VMs on one host
/// (`reconciler/infra.rs::domain_name_for`).
fn domain_name(namespace: &str, member: &str) -> String {
    format!("{namespace}-{member}")
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

async fn client() -> Client {
    Client::try_default()
        .await
        .expect("no reachable cluster — set KUBECONFIG")
}

async fn wait_for<T, F, Fut>(what: &str, timeout: Duration, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + timeout;
    eprintln!("  … {what}");
    loop {
        if let Some(v) = check().await {
            eprintln!("  ✓ {what}");
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {}s waiting for: {what}",
            timeout.as_secs()
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One run's objects, in the namespace the `Provider` lives in.
///
/// Deliberately **not** a generated namespace. Candidate `Provider`s are
/// drawn from the VirtualMachine's own namespace
/// (`reconciler/virtualmachine.rs`), so members created anywhere else never
/// schedule — they just sit Pending until the suite times out, which reads
/// as "pools are broken" rather than "the fixture was in the wrong
/// namespace". Isolation is by name prefix instead, and teardown removes
/// exactly what this run created.
struct Scratch {
    client: Client,
    namespace: String,
    prefix: String,
}

impl Scratch {
    async fn new(client: &Client, label: &str) -> Self {
        let namespace =
            std::env::var("BANLIEUE_E2E_NAMESPACE").unwrap_or_else(|_| "banlieue-system".into());
        let mut b = [0u8; 3];
        getrandom::fill(&mut b).unwrap();
        let suffix: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let prefix = format!("e2e-{label}-{suffix}");
        eprintln!("  namespace: {namespace}, prefix: {prefix}");
        Self {
            client: client.clone(),
            namespace,
            prefix,
        }
    }

    /// A run-unique object name, so concurrent runs and leftovers from a
    /// previous one cannot be confused for this run's work.
    fn name(&self, what: &str) -> String {
        format!("{}-{what}", self.prefix)
    }

    fn vms(&self) -> Api<VirtualMachine> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }
    fn pools(&self) -> Api<VirtualMachinePool> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }
    fn claims(&self) -> Api<VirtualMachineClaim> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }

    /// Members of *this run's* pool, or `None` when the API server could
    /// not answer. The distinction matters: an unreachable API is not an
    /// empty pool, and conflating them is how this suite's teardown once
    /// reported "every member deleted" while the cluster was down and
    /// nothing had been deleted at all.
    async fn try_member_names(&self, pool: &str) -> Option<Vec<String>> {
        self.vms()
            .list(&ListParams::default().labels(&format!("{LABEL_POOL}={pool}")))
            .await
            .map(|l| l.into_iter().map(|vm| vm.name_any()).collect())
            .ok()
    }

    /// Members of *this run's* pool. Filtered by the pool label, so other
    /// VirtualMachines in a shared namespace are never touched or counted.
    /// A failed LIST reads as empty — fine mid-test where the next poll
    /// retries, wrong for teardown's final verdict, which uses
    /// [`Self::try_member_names`].
    async fn member_names(&self, pool: &str) -> Vec<String> {
        self.try_member_names(pool).await.unwrap_or_default()
    }

    /// Delete this run's objects and verify they are really gone, on the
    /// host too.
    ///
    /// Never `|| true` without a follow-up check: a teardown that reports
    /// success while leaving domains defined is how stale state accumulates
    /// and how the next run silently reuses it.
    async fn teardown(&self, pool: &str) {
        eprintln!("  tearing down {}", self.prefix);
        let expected: Vec<String> = self
            .member_names(pool)
            .await
            .iter()
            .map(|m| domain_name(&self.namespace, m))
            .collect();

        if let Ok(claims) = self.claims().list(&ListParams::default()).await {
            for c in claims
                .into_iter()
                .filter(|c| c.name_any().starts_with(&self.prefix))
            {
                let _ = self
                    .claims()
                    .delete(&c.name_any(), &DeleteParams::background())
                    .await;
            }
        }
        if let Ok(pools) = self.pools().list(&ListParams::default()).await {
            for p in pools
                .into_iter()
                .filter(|p| p.name_any().starts_with(&self.prefix))
            {
                let _ = self
                    .pools()
                    .delete(&p.name_any(), &DeleteParams::background())
                    .await;
            }
        }

        let gone = wait_for_opt("every member to be deleted", TEARDOWN_TIMEOUT, || async {
            // `try_`: a LIST the API server failed to answer proves nothing.
            self.try_member_names(pool).await?.is_empty().then_some(())
        })
        .await;
        if gone.is_none() {
            eprintln!(
                "  ⚠ members still present: {:?} — check the host for leftover domains",
                self.member_names(pool).await
            );
        }

        if let Some((host, dir)) = libvirt_settings() {
            for domain in &expected {
                if domain_exists(&host, &dir, domain).await {
                    eprintln!("  ⚠ LEFTOVER DOMAIN ON HOST: {domain}");
                }
            }
        }
    }
}

/// `wait_for` that reports failure instead of panicking — teardown must
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

/// Assert the fixture the suite depends on actually exists, so a typo
/// fails in seconds with a clear message rather than as a 12-minute
/// provisioning timeout.
async fn require_ready_provider(s: &Scratch, provider: &str) {
    let api: Api<banlieue_api::banlieue::Provider> =
        Api::namespaced(s.client.clone(), &s.namespace);
    let p = api.get_opt(provider).await.expect("reading Providers");
    let Some(p) = p else {
        panic!(
            "Provider {provider} not found in namespace {}. Members schedule only \
             against Providers in their own namespace — set BANLIEUE_E2E_NAMESPACE \
             to where your Provider lives.",
            s.namespace
        );
    };
    let ready = p
        .status
        .as_ref()
        .map(|st| {
            st.conditions
                .iter()
                .any(|c| c.type_ == "Ready" && c.status == "True")
        })
        .unwrap_or(false);
    assert!(
        ready,
        "Provider {provider} is not Ready; this suite tests pools, not the install"
    );
    eprintln!("  ✓ Provider {provider} is Ready");
}

async fn make_pool(s: &Scratch, f: &Fixture, name: &str) {
    let pool: VirtualMachinePool = serde_json::from_value(json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "VirtualMachinePool",
        "metadata": { "name": name, "namespace": s.namespace },
        "spec": {
            "warmReplicas": WARM,
            "maxReplicas": WARM + 2,
            "maxSurge": WARM,
            // The only value satisfiable today: GuestReady needs ADR-0043.
            "readiness": "InfrastructureReady",
            "template": {
                "spec": {
                    "classRef": { "name": f.class },
                    "imageRef": { "name": f.image },
                    "desiredPowerState": "PoweredOn",
                    // Members are cattle; a migration mid-test would muddy
                    // every on-host assertion below.
                    "migrationPolicy": "never",
                }
            },
        },
    }))
    .expect("pool fixture");
    s.pools()
        .create(&PostParams::default(), &pool)
        .await
        .expect("create pool");
}

async fn make_claim(s: &Scratch, name: &str, pool: &str) {
    let claim: VirtualMachineClaim = serde_json::from_value(json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "VirtualMachineClaim",
        "metadata": { "name": name, "namespace": s.namespace },
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
    s.claims()
        .create(&PostParams::default(), &claim)
        .await
        .expect("create claim");
}

async fn available(s: &Scratch, pool: &str) -> u32 {
    s.pools()
        .get(pool)
        .await
        .ok()
        .and_then(|p| p.status.map(|st| st.available))
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The whole story: a pool of real domains, one handed out, then destroyed.
///
/// The final assertion is the one that matters and the only one no other
/// tier can make — after the claim is gone, libvirtd must not have the
/// domain.
#[tokio::test]
#[ignore = "real cluster + libvirt host; set BANLIEUE_E2E_PROVIDER/_IMAGE/_CLASS"]
async fn a_claimed_member_is_destroyed_on_the_host_when_the_claim_is_released() {
    let Some(f) = fixture() else {
        panic!("set BANLIEUE_E2E_PROVIDER, BANLIEUE_E2E_IMAGE and BANLIEUE_E2E_CLASS");
    };
    let on_host = libvirt_settings();
    if on_host.is_none() {
        eprintln!("  ⚠ LIBVIRT_HOST/LIBVIRT_TLS_DIR unset — skipping every on-host check.");
        eprintln!("    That is the point of this suite; this run proves strictly less.");
    }

    let client = client().await;
    let s = Scratch::new(&client, "poolclaim").await;
    require_ready_provider(&s, &f.provider).await;
    let pool = s.name("pool");
    let result = run_release_case(&s, &f, &pool, on_host.as_ref()).await;
    s.teardown(&pool).await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

async fn run_release_case(
    s: &Scratch,
    f: &Fixture,
    pool: &str,
    on_host: Option<&(String, PathBuf)>,
) -> Result<(), Box<dyn std::any::Any + Send>> {
    use futures::FutureExt as _;
    std::panic::AssertUnwindSafe(async {
        make_pool(s, f, pool).await;

        wait_for(
            "the pool to reach its warm size",
            PROVISION_TIMEOUT,
            || async { (available(s, pool).await >= WARM).then_some(()) },
        )
        .await;

        let members = s.member_names(pool).await;
        assert_eq!(members.len() as u32, WARM, "members: {members:?}");

        if let Some((host, dir)) = on_host {
            for m in &members {
                let d = domain_name(&s.namespace, m);
                assert!(
                    domain_exists(host, dir, &d).await,
                    "member {m} is Ready but libvirtd has no domain {d}"
                );
            }
            eprintln!("  ✓ both warm members exist as real domains");
        }

        // --- claim one -----------------------------------------------------
        let claim = s.name("claim");
        make_claim(s, &claim, pool).await;
        let bound = wait_for("the claim to bind a member", TEARDOWN_TIMEOUT, || async {
            s.claims()
                .get(&claim)
                .await
                .ok()?
                .status?
                .virtual_machine_ref
                .map(|r| r.name)
        })
        .await;
        assert!(members.contains(&bound), "bound a member of this pool");

        let member = s.vms().get(&bound).await.expect("get bound member");
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

        let bound_domain = domain_name(&s.namespace, &bound);
        if let Some((host, dir)) = on_host {
            assert!(
                domain_exists(host, dir, &bound_domain).await,
                "binding must not disturb the running domain"
            );
        }

        // The pool must not count a claimed member as warm, and must refill.
        //
        // Wait on the *replacement*, not on `available >= WARM`: the latter
        // is still true from before the claim until the pool next
        // reconciles, so waiting on it returns instantly and then races the
        // property under test. Waiting on the member count means a pool
        // that genuinely never refills fails here with a timeout, instead
        // of an assertion that fires before the pool has had a chance.
        wait_for(
            "the pool to build a replacement for the claimed member",
            PROVISION_TIMEOUT,
            || async { (s.member_names(pool).await.len() as u32 > WARM).then_some(()) },
        )
        .await;
        wait_for("the pool to be warm again", PROVISION_TIMEOUT, || async {
            (available(s, pool).await >= WARM).then_some(())
        })
        .await;

        // --- release it ----------------------------------------------------
        s.claims()
            .delete(&claim, &DeleteParams::default())
            .await
            .expect("delete claim");

        wait_for("the claim to release", TEARDOWN_TIMEOUT, || async {
            s.claims()
                .get_opt(&claim)
                .await
                .ok()?
                .is_none()
                .then_some(())
        })
        .await;

        assert!(
            s.vms().get_opt(&bound).await.unwrap().is_none(),
            "the member must be gone from the API server once the claim released"
        );

        // THE assertion. Everything above is scaffolding for this line.
        if let Some((host, dir)) = on_host {
            assert!(
                !domain_exists(host, dir, &bound_domain).await,
                "ADR-0047 Decision 6 is violated: the claim is gone but libvirtd \
                 still has domain {bound_domain}. \"Claim deleted\" must mean \
                 \"sandbox destroyed\"."
            );
            eprintln!("  ✓ the released domain is gone from the host");
        }
    })
    .catch_unwind()
    .await
}

/// ADR-0047 Decision 3, on real domains: deleting the pool must not take a
/// sandbox somebody is using with it.
///
/// `live_claim.rs` proves the `ownerReferences` move; only here does that
/// translate into "the VM is still running on the hypervisor afterwards",
/// which is the property an operator actually cares about.
#[tokio::test]
#[ignore = "real cluster + libvirt host; set BANLIEUE_E2E_PROVIDER/_IMAGE/_CLASS"]
async fn deleting_a_pool_leaves_a_claimed_domain_running() {
    let Some(f) = fixture() else {
        panic!("set BANLIEUE_E2E_PROVIDER, BANLIEUE_E2E_IMAGE and BANLIEUE_E2E_CLASS");
    };
    let on_host = libvirt_settings();
    let client = client().await;
    let s = Scratch::new(&client, "poolgc").await;
    require_ready_provider(&s, &f.provider).await;
    let pool = s.name("pool");
    let claim = s.name("claim");

    use futures::FutureExt as _;
    let result = std::panic::AssertUnwindSafe(async {
        make_pool(&s, &f, &pool).await;
        wait_for(
            "the pool to reach its warm size",
            PROVISION_TIMEOUT,
            || async { (available(&s, &pool).await >= WARM).then_some(()) },
        )
        .await;

        make_claim(&s, &claim, &pool).await;
        let bound = wait_for("the claim to bind", TEARDOWN_TIMEOUT, || async {
            s.claims()
                .get(&claim)
                .await
                .ok()?
                .status?
                .virtual_machine_ref
                .map(|r| r.name)
        })
        .await;
        let bound_domain = domain_name(&s.namespace, &bound);

        s.pools()
            .delete(&pool, &DeleteParams::background())
            .await
            .expect("delete pool");

        wait_for(
            "the unclaimed members to be collected",
            TEARDOWN_TIMEOUT,
            || async {
                let remaining: Vec<String> = s
                    .vms()
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

        assert!(
            s.vms().get_opt(&bound).await.unwrap().is_some(),
            "the claimed member must survive its pool"
        );
        if let Some((host, dir)) = &on_host {
            assert!(
                domain_exists(host, dir, &bound_domain).await,
                "deleting the pool destroyed a sandbox in use: domain {bound_domain} is gone"
            );
            eprintln!("  ✓ the claimed domain outlived its pool");
        }
    })
    .catch_unwind()
    .await;

    s.teardown(&pool).await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

// ---------------------------------------------------------------------------
// Rollout: bumping the image replaces every member without going cold
// ---------------------------------------------------------------------------

/// Time the rollout has to replace both members. Two provisions plus two
/// teardowns, sequentially in the worst case.
const ROLLOUT_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Revision labels of the pool's current members, keyed by member name.
async fn member_revisions(s: &Scratch, pool: &str) -> Vec<(String, String)> {
    s.vms()
        .list(&ListParams::default().labels(&format!("{LABEL_POOL}={pool}")))
        .await
        .map(|l| {
            l.into_iter()
                .map(|vm| {
                    let rev = vm
                        .labels()
                        .get(LABEL_POOL_IMAGE_REVISION)
                        .cloned()
                        .unwrap_or_default();
                    (vm.name_any(), rev)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Roadmap 17's definition-of-done item that no tier had ever run: bump the
/// `VMImage` and watch the pool replace every member **while
/// `status.available` never drops below `warmReplicas`**.
///
/// The suite's shared fixture image is never touched: the pool here is built
/// from a scratch copy of it, so the bump — any spec change; the revision of
/// an image with no managed build is its `metadata.generation` — is isolated
/// to this run.
///
/// The invariant is *sampled*: every poll while the rollout runs asserts
/// `available >= warmReplicas`. A dip between two polls can escape it, but a
/// pool that goes cold for a provisioning-length window (the failure this
/// guards against) cannot.
#[tokio::test]
#[ignore = "real cluster + libvirt host; set BANLIEUE_E2E_PROVIDER/_IMAGE/_CLASS"]
async fn bumping_the_image_rolls_the_pool_without_dropping_below_warm() {
    let Some(f) = fixture() else {
        panic!("set BANLIEUE_E2E_PROVIDER, BANLIEUE_E2E_IMAGE and BANLIEUE_E2E_CLASS");
    };
    let client = client().await;
    let s = Scratch::new(&client, "rollout").await;
    require_ready_provider(&s, &f.provider).await;

    let images: Api<VMImage> = Api::all(s.client.clone());
    let image = s.name("img");
    let pool = s.name("pool");

    use futures::FutureExt as _;
    let result = std::panic::AssertUnwindSafe(async {
        // A scratch image, spec-identical to the fixture's.
        let base = images.get(&f.image).await.expect("fixture VMImage");
        let scratch: VMImage = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "VMImage",
            "metadata": { "name": image },
            "spec": serde_json::to_value(&base.spec).unwrap(),
        }))
        .expect("scratch image fixture");
        images
            .create(&PostParams::default(), &scratch)
            .await
            .expect("create scratch image");

        wait_for(
            "the scratch image to be Ready",
            TEARDOWN_TIMEOUT,
            || async {
                let img = images.get(&image).await.ok()?;
                img.status?
                    .conditions
                    .iter()
                    .any(|c| c.type_ == "Ready" && c.status == "True")
                    .then_some(())
            },
        )
        .await;

        let pool_obj: VirtualMachinePool = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "VirtualMachinePool",
            "metadata": { "name": pool, "namespace": s.namespace },
            "spec": {
                "warmReplicas": WARM,
                "maxReplicas": WARM + 2,
                "maxSurge": WARM,
                "readiness": "InfrastructureReady",
                // Explicit, although it is the default: this test IS the
                // recycle path, and a silently flipped default would turn it
                // into a 15-minute timeout.
                "recycleOnImageChange": true,
                "template": {
                    "spec": {
                        "classRef": { "name": f.class },
                        "imageRef": { "name": image },
                        "desiredPowerState": "PoweredOn",
                        "migrationPolicy": "never",
                    }
                },
            },
        }))
        .expect("pool fixture");
        s.pools()
            .create(&PostParams::default(), &pool_obj)
            .await
            .expect("create pool");

        wait_for(
            "the pool to reach its warm size",
            PROVISION_TIMEOUT,
            || async { (available(&s, &pool).await >= WARM).then_some(()) },
        )
        .await;

        let before = member_revisions(&s, &pool).await;
        assert_eq!(before.len() as u32, WARM, "members: {before:?}");
        let old_names: Vec<&String> = before.iter().map(|(n, _)| n).collect();
        let old_revisions: Vec<&String> = before.iter().map(|(_, r)| r).collect();
        eprintln!("  members before the bump: {before:?}");

        // The bump. Any spec change moves `metadata.generation`, and an
        // image with no managed build takes its revision from exactly that
        // (`pool.rs::image_revision`). `osVersion` is inert to a
        // `BackingFile` resolve, so nothing about the members changes except
        // the revision they are labelled with.
        images
            .patch(
                &image,
                &PatchParams::default(),
                &Patch::Merge(json!({ "spec": { "osVersion": "13-rolled" } })),
            )
            .await
            .expect("bump scratch image");

        // Completion is "every member carries a new revision", never
        // "available recovered" — the latter is true before the rollout
        // starts, so waiting on it would race the property under test.
        // The DoD invariant is asserted at every poll on the way.
        wait_for(
            "every member to be replaced with the new revision",
            ROLLOUT_TIMEOUT,
            || async {
                let avail = available(&s, &pool).await;
                assert!(
                    avail >= WARM,
                    "the rollout dropped availability below warmReplicas: {avail} < {WARM}"
                );
                let now = member_revisions(&s, &pool).await;
                let rolled = now.len() as u32 == WARM
                    && now.iter().all(|(n, r)| {
                        !old_names.contains(&n) && !old_revisions.contains(&r) && !r.is_empty()
                    });
                rolled.then_some(())
            },
        )
        .await;

        for (name, _) in &before {
            assert!(
                s.vms().get_opt(name).await.unwrap().is_none(),
                "stale member {name} must be gone after the rollout"
            );
        }
        eprintln!("  ✓ both members rolled; availability held at ≥ {WARM} at every sample");
    })
    .catch_unwind()
    .await;

    s.teardown(&pool).await;
    let _ = images.delete(&image, &DeleteParams::default()).await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

// ---------------------------------------------------------------------------
// Provisioning timeout: a sabotaged member is reaped and replaced
// ---------------------------------------------------------------------------

/// A member still provisioning after this long is poisoned (ADR-0046).
/// Long enough that a healthy member — boot, apt-install the agent,
/// announce — is comfortably inside it; short enough that the reap happens
/// within the test's patience.
const SABOTAGE_PROVISIONING_TIMEOUT_SECS: u64 = 300;
/// How long the sabotage loop hunts for a running domain to cut off.
const SABOTAGE_WINDOW: Duration = Duration::from_secs(6 * 60);
/// The sabotage races the guest's boot, so it polls faster than the suite.
const SABOTAGE_POLL: Duration = Duration::from_secs(2);

/// Installs the guest agent through the NoCloud seed — no available image
/// ships it — and writes the ADR-0043 phase marker, so a member announces
/// once, and only once, apt has succeeded. Severing the member's network
/// before that is what the sabotage exploits: no mirror, no agent, no
/// marker, no `GuestReady`, ever.
const SABOTAGE_SEED: &str = "#cloud-config\n\
package_update: true\n\
packages:\n\
  - qemu-guest-agent\n\
runcmd:\n\
  - [ systemctl, enable, --now, qemu-guest-agent ]\n\
  - [ mkdir, -p, /run/banlieue ]\n\
  - [ sh, -c, \"echo installed > /run/banlieue/phase\" ]\n";

/// Force the first NIC of a **running** domain link-down, live only.
///
/// Returns `false` when the domain is absent or not yet running — the
/// caller retries. The update XML is the domain's own `<interface>` element
/// (MACs are auto-assigned, so it must be read back, not reconstructed)
/// with `<link state='down'/>` added; libvirt matches the device by MAC and
/// applies the link state to the live domain. The persistent definition is
/// left alone on purpose: the provider redefines it each converge, and a
/// live-only change survives that, while a config-only one would be both
/// overwritten and inert.
async fn sabotage_link_down(host: &str, dir: &Path, domain: &str) -> bool {
    let identity = load_identity(dir);
    let mut session = connect_tls(host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS to the libvirt host");
    connect_open(&mut session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN");
    let dom = match domain_lookup_by_name(&mut session, domain).await {
        Ok(d) => d,
        Err(e) if is_not_found(&e) => return false,
        Err(e) => panic!("looking up domain {domain}: {e}"),
    };
    let state = domain_get_state(&mut session, &dom)
        .await
        .expect("DOMAIN_GET_STATE");
    if !state.is_running() {
        return false;
    }
    let xml = domain_get_xml_desc(&mut session, &dom, 0)
        .await
        .expect("DOMAIN_GET_XML_DESC");
    let start = xml.find("<interface").expect("domain has an <interface>");
    let end = xml[start..]
        .find("</interface>")
        .map(|i| start + i + "</interface>".len())
        .expect("interface element is closed");
    let device = xml[start..end].replace("</interface>", "<link state='down'/></interface>");
    domain_update_device_flags(&mut session, &dom, &device, DEVICE_MODIFY_LIVE)
        .await
        .expect("DOMAIN_UPDATE_DEVICE_FLAGS(link down)");
    true
}

/// Roadmap 17's other never-run definition-of-done item: a member whose
/// install is sabotaged never reaches readiness, is deleted at
/// `provisioningTimeoutSeconds`, and is replaced — and the pool still warms.
///
/// The sabotage severs the first member's network (live link-down) before
/// apt can fetch `qemu-guest-agent` from the seed's package list. The
/// original DoD phrasing was "detach the ISO mid-install"; on this suite's
/// no-install `BackingFile` images the install step *is* the seed-driven
/// agent install, and cutting its network is the same category of injury
/// with a far wider window (the whole apt run, not the seconds before
/// cloud-init reads the seed).
///
/// If the sabotage ever loses that race the sabotaged member goes Ready,
/// nothing is reaped, and the "sabotaged member to be reaped" wait times
/// out — a loud wrong-reason failure to rerun, never a silent pass.
#[tokio::test]
#[ignore = "real cluster + libvirt host; set BANLIEUE_E2E_PROVIDER/_IMAGE/_CLASS"]
async fn a_sabotaged_member_is_reaped_at_provisioning_timeout_and_replaced() {
    let Some(f) = fixture() else {
        panic!("set BANLIEUE_E2E_PROVIDER, BANLIEUE_E2E_IMAGE and BANLIEUE_E2E_CLASS");
    };
    let Some((host, dir)) = libvirt_settings() else {
        panic!(
            "set LIBVIRT_HOST and LIBVIRT_TLS_DIR — the sabotage is an on-host act; \
             without them this test cannot run at all"
        );
    };

    let client = client().await;
    let s = Scratch::new(&client, "sabotage").await;
    require_ready_provider(&s, &f.provider).await;

    let configmaps: Api<ConfigMap> = Api::namespaced(s.client.clone(), &s.namespace);
    let userdata = s.name("ud");
    let pool = s.name("pool");

    use futures::FutureExt as _;
    let result = std::panic::AssertUnwindSafe(async {
        let cm: ConfigMap = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": { "name": userdata, "namespace": s.namespace },
            "data": { "user-data": SABOTAGE_SEED },
        }))
        .expect("userdata fixture");
        configmaps
            .create(&PostParams::default(), &cm)
            .await
            .expect("create userdata ConfigMap");

        let pool_obj: VirtualMachinePool = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "VirtualMachinePool",
            "metadata": { "name": pool, "namespace": s.namespace },
            "spec": {
                "warmReplicas": WARM,
                "maxReplicas": WARM + 2,
                "maxSurge": WARM,
                // GuestReady: the member is ready when the guest ANNOUNCES,
                // which is the signal the sabotage suppresses. With
                // InfrastructureReady the domain merely running would count,
                // and there would be nothing to sabotage.
                "readiness": "GuestReady",
                "provisioningTimeoutSeconds": SABOTAGE_PROVISIONING_TIMEOUT_SECS,
                "template": {
                    "spec": {
                        "classRef": { "name": f.class },
                        "imageRef": { "name": f.image },
                        "desiredPowerState": "PoweredOn",
                        "migrationPolicy": "never",
                        "userData": { "configMapRef": { "name": userdata } },
                    }
                },
            },
        }))
        .expect("pool fixture");
        s.pools()
            .create(&PostParams::default(), &pool_obj)
            .await
            .expect("create pool");

        // Hunt: the FIRST member whose domain reaches `running` gets its
        // link cut. 2s polling against a boot-to-apt window measured in
        // tens of seconds.
        let deadline = Instant::now() + SABOTAGE_WINDOW;
        let sabotaged: String = loop {
            let mut hit = None;
            for m in s.member_names(&pool).await {
                let d = domain_name(&s.namespace, &m);
                if sabotage_link_down(&host, &dir, &d).await {
                    hit = Some(m);
                    break;
                }
            }
            if let Some(m) = hit {
                break m;
            }
            assert!(
                Instant::now() < deadline,
                "no member's domain reached `running` within {}s",
                SABOTAGE_WINDOW.as_secs()
            );
            tokio::time::sleep(SABOTAGE_POLL).await;
        };
        let sabotaged_domain = domain_name(&s.namespace, &sabotaged);
        eprintln!("  ✂ severed the network of {sabotaged} ({sabotaged_domain})");

        // The reap: gone from the API server, at roughly the configured
        // timeout. The wait is on the member's absence — the thing that is
        // actually new — with room for the timeout plus a reconcile.
        wait_for(
            "the sabotaged member to be reaped at provisioningTimeoutSeconds",
            Duration::from_secs(SABOTAGE_PROVISIONING_TIMEOUT_SECS) + PROVISION_TIMEOUT,
            || async {
                s.vms()
                    .get_opt(&sabotaged)
                    .await
                    .ok()?
                    .is_none()
                    .then_some(())
            },
        )
        .await;

        // And replaced: the pool still reaches warm, without the sabotaged
        // member in it.
        wait_for(
            "the pool to warm up on the replacement",
            PROVISION_TIMEOUT,
            || async { (available(&s, &pool).await >= WARM).then_some(()) },
        )
        .await;
        let members = s.member_names(&pool).await;
        assert!(
            !members.contains(&sabotaged),
            "the reaped member must not be resurrected: {members:?}"
        );

        assert!(
            !domain_exists(&host, &dir, &sabotaged_domain).await,
            "the reaped member's domain {sabotaged_domain} must be gone from the host"
        );
        eprintln!("  ✓ sabotaged member reaped and replaced; pool warm without it");
    })
    .catch_unwind()
    .await;

    s.teardown(&pool).await;
    let _ = configmaps.delete(&userdata, &DeleteParams::default()).await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}
