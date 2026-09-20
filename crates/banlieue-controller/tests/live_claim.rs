// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VirtualMachineClaim` against a **real API server** (ADR-0047).
//!
//! `claim_plan_tests.rs` drives every decision as a pure function, which
//! proves the state machine but not the three things that make a claim
//! trustworthy — and all three are API-server behaviours that no offline
//! test can reach:
//!
//! 1. **The bind is exclusive.** The `resourceVersion` precondition either
//!    produces a 409 for the loser of a race, or it does not. A fake that
//!    accepts every patch would pass the unit tests while handing one VM to
//!    two subjects, which is the one outcome this design exists to prevent.
//! 2. **Ownership actually moves.** Re-parenting is a merge patch on
//!    `ownerReferences`; whether the apiserver ends up with the claim as the
//!    sole controller is a fact about merge semantics, not about our code.
//! 3. **The finalizer really blocks.** "Claim deleted means sandbox
//!    destroyed" is a claim about deletion ordering that only a deletion can
//!    settle.
//!
//! Deliberately *not* a libvirt test: no provider is involved, and members
//! are plain `VirtualMachine` CRs the test creates itself. The backend half
//! lives in `banlieue-provider-libvirt/tests/live_machine.rs`, and the two
//! meet in `e2e_pool_claim.rs`.
//!
//! ```sh
//! KUBECONFIG=~/.kube/config \
//!   cargo test -p banlieue-controller --test live_claim -- --ignored --nocapture
//! ```
//!
//! Needs only the banlieue CRDs installed — **no controller running**; the
//! test drives `claim::reconcile` itself. If a controller *is* running it
//! will race the test, so point this at a cluster where it is not, or accept
//! the flakes. Every test works in its own generated namespace and deletes
//! it on the way out, including on failure.

use std::sync::Arc;
use std::time::Duration;

use banlieue_api::banlieue::{
    ANNOTATION_SUBJECT_ID, ANNOTATION_SUBJECT_ISSUER, CLAIM_FINALIZER, ClaimPhase, LABEL_CLAIM,
    LABEL_POOL, LABEL_POOL_IMAGE_REVISION, VirtualMachine, VirtualMachineClaim, VirtualMachinePool,
    pool_condition_reasons,
};
use banlieue_api::common::condition_types;
use banlieue_controller::context::Context;
use banlieue_controller::reconciler::claim;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::jiff::Timestamp;
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use kube::{Client, Resource, ResourceExt};
use serde_json::json;

/// A finalizer the test puts on a member to stand in for the provider's.
/// Nothing removes it but the test, so a member's deletion blocks exactly
/// as a real backend teardown would — deterministically, with no provider.
const FAKE_PROVIDER_FINALIZER: &str = "banlieue.io/test-blocks-deletion";

/// Seconds of TTL for the expiry test. Short enough not to slow the suite,
/// long enough that the bind itself cannot race the deadline.
const SHORT_TTL_SECS: u64 = 2;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Connect, or fail loudly.
///
/// **Not a skip.** These tests are `#[ignore]`d, so running them is already
/// an explicit request to go and talk to a cluster — at which point an
/// unreachable one is a failure, not a reason to report success. An earlier
/// version returned `None` and every test printed `ok` while doing nothing,
/// which is strictly worse than having no tests: it answers "are claims
/// covered?" with a confident yes.
async fn client() -> Client {
    let client = Client::try_default()
        .await
        .expect("no kubeconfig — point KUBECONFIG at a cluster with the banlieue CRDs");

    // Prove the connection AND the CRDs, not just the config: a stale
    // kubeconfig constructs fine and fails on first use, and a cluster
    // without the pool/claim CRDs fails in a way that reads like a bug in
    // the reconciler.
    let api: Api<VirtualMachinePool> = Api::all(client.clone());
    api.list(&Default::default()).await.unwrap_or_else(|e| {
        panic!(
            "cannot list VirtualMachinePools on context {:?}: {e}\n\
             This suite needs the banlieue CRDs installed (kubectl apply -f deploy/crds/) \
             and no controller running.",
            std::env::var("KUBECONFIG").unwrap_or_else(|_| "<default kubeconfig>".into()),
        )
    });
    client
}

/// A namespace that exists only for one test.
struct Scratch {
    client: Client,
    namespace: String,
}

impl Scratch {
    async fn new(client: &Client, label: &str) -> Self {
        let namespace = format!("banlieue-claim-{label}-{}", nonce());
        let api: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(client.clone());
        api.create(
            &PostParams::default(),
            &serde_json::from_value(json!({
                "apiVersion": "v1",
                "kind": "Namespace",
                "metadata": { "name": namespace },
            }))
            .unwrap(),
        )
        .await
        .expect("create scratch namespace");
        Self {
            client: client.clone(),
            namespace,
        }
    }

    fn ctx(&self) -> Arc<Context> {
        Arc::new(Context::new(
            self.client.clone(),
            Some(self.namespace.clone()),
        ))
    }

    fn vms(&self) -> Api<VirtualMachine> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }

    fn claims(&self) -> Api<VirtualMachineClaim> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }

    fn pools(&self) -> Api<VirtualMachinePool> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }

    /// Best-effort teardown. Members carry a test finalizer that nothing
    /// else removes, so strip those first or the namespace hangs
    /// Terminating forever — the same trap `destroy_all` fell into in
    /// `bootstrap-k0s-cluster.sh`, and the reason nothing here is `|| true`
    /// without also verifying.
    async fn teardown(&self) {
        let vms = self.vms();
        if let Ok(list) = vms.list(&Default::default()).await {
            for vm in list {
                let _ = vms
                    .patch(
                        &vm.name_any(),
                        &PatchParams::default(),
                        &Patch::Merge(json!({ "metadata": { "finalizers": [] } })),
                    )
                    .await;
            }
        }
        let claims = self.claims();
        if let Ok(list) = claims.list(&Default::default()).await {
            for c in list {
                let _ = claims
                    .patch(
                        &c.name_any(),
                        &PatchParams::default(),
                        &Patch::Merge(json!({ "metadata": { "finalizers": [] } })),
                    )
                    .await;
            }
        }
        let api: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(self.client.clone());
        let _ = api
            .delete(&self.namespace, &DeleteParams::background())
            .await;
    }
}

fn nonce() -> String {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).unwrap();
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Create a pool. Only `spec.readiness` and the template matter here — the
/// pool controller is not running, so nothing acts on the sizing fields.
///
/// Built from JSON rather than a struct literal, like the pool's own unit
/// tests: it exercises the *wire* shape an operator actually writes, and it
/// does not break every time an unrelated optional field is added.
async fn make_pool(s: &Scratch, name: &str, revision: Option<&str>) -> VirtualMachinePool {
    let pool: VirtualMachinePool = serde_json::from_value(json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "VirtualMachinePool",
        "metadata": { "name": name, "namespace": s.namespace },
        "spec": {
            "warmReplicas": 1,
            "maxReplicas": 4,
            "readiness": "InfrastructureReady",
            "template": { "spec": vm_spec() },
        },
    }))
    .expect("pool fixture");

    let created = s
        .pools()
        .create(&PostParams::default(), &pool)
        .await
        .expect("create pool");
    let Some(rev) = revision else { return created };

    s.pools()
        .patch_status(
            name,
            &PatchParams::default(),
            &Patch::Merge(json!({ "status": { "imageRevision": rev } })),
        )
        .await
        .expect("patch pool status")
}

/// The minimum a `VirtualMachine` spec needs. Neither reference has to
/// resolve: the claim reconciler reads members' labels and conditions, never
/// their class or image.
fn vm_spec() -> serde_json::Value {
    json!({
        "classRef": { "name": "small" },
        "imageRef": { "name": "test-image" },
    })
}

/// Create a member owned by `pool`, already Ready, optionally blocking its
/// own deletion the way a provider finalizer would.
async fn make_member(
    s: &Scratch,
    pool: &VirtualMachinePool,
    name: &str,
    revision: &str,
    block_deletion: bool,
) -> VirtualMachine {
    let owner = pool.controller_owner_ref(&()).expect("pool has a uid");
    let finalizers: Vec<String> = if block_deletion {
        vec![FAKE_PROVIDER_FINALIZER.to_string()]
    } else {
        vec![]
    };

    let vm: VirtualMachine = serde_json::from_value(json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "VirtualMachine",
        "metadata": {
            "name": name,
            "namespace": s.namespace,
            "labels": {
                LABEL_POOL: pool.name_any(),
                LABEL_POOL_IMAGE_REVISION: revision,
            },
            "ownerReferences": [owner],
            "finalizers": finalizers,
        },
        "spec": vm_spec(),
    }))
    .expect("member fixture");

    s.vms()
        .create(&PostParams::default(), &vm)
        .await
        .expect("create member");

    // Ready, as the pool's `member_view` reads readiness: the condition
    // named by `spec.readiness`, with status True.
    s.vms()
        .patch_status(
            name,
            &PatchParams::default(),
            &Patch::Merge(json!({ "status": {
                "conditions": [{
                    "type": condition_types::INFRASTRUCTURE_READY,
                    "status": "True",
                    "reason": "Provisioned",
                    "message": "live_claim test fixture",
                    "lastTransitionTime": Time(Timestamp::now()),
                }],
            }})),
        )
        .await
        .expect("patch member status")
}

async fn make_claim(s: &Scratch, name: &str, pool: &str, ttl: u64) -> VirtualMachineClaim {
    let claim: VirtualMachineClaim = serde_json::from_value(json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "VirtualMachineClaim",
        "metadata": { "name": name, "namespace": s.namespace },
        "spec": {
            "poolRef": { "name": pool },
            "subject": {
                "issuer": "https://issuer.example.com",
                "id": format!("subject-{name}"),
            },
            "ttlSeconds": ttl,
        },
    }))
    .expect("claim fixture");

    s.claims()
        .create(&PostParams::default(), &claim)
        .await
        .expect("create claim")
}

/// Reconcile once, from the object as it currently stands.
async fn reconcile(s: &Scratch, name: &str) {
    let claim = s.claims().get(name).await.expect("get claim");
    claim::reconcile(Arc::new(claim), s.ctx())
        .await
        .expect("reconcile");
}

/// Reconcile once, tolerating an error (used where a 409 or a mid-delete
/// read is an expected outcome rather than a failure).
async fn reconcile_lenient(s: &Scratch, name: &str) {
    let Ok(claim) = s.claims().get(name).await else {
        return;
    };
    let _ = claim::reconcile(Arc::new(claim), s.ctx()).await;
}

fn phase(claim: &VirtualMachineClaim) -> ClaimPhase {
    claim.status.as_ref().map(|s| s.phase).unwrap_or_default()
}

/// Run `body`, tearing the namespace down whether or not it panicked.
async fn with_scratch<F, Fut>(label: &str, body: F)
where
    F: FnOnce(Scratch) -> Fut,
    Fut: std::future::Future<Output = Scratch>,
{
    let client = client().await;
    let scratch = Scratch::new(&client, label).await;
    let namespace = scratch.namespace.clone();
    let result = std::panic::AssertUnwindSafe(body(scratch))
        .catch_unwind_or_run()
        .await;
    match result {
        Ok(s) => s.teardown().await,
        Err(payload) => {
            let s = Scratch { client, namespace };
            s.teardown().await;
            std::panic::resume_unwind(payload);
        }
    }
}

/// `futures::FutureExt::catch_unwind` without pulling in the dependency for
/// one call site.
trait CatchUnwindOrRun: Sized {
    type Output;
    async fn catch_unwind_or_run(self) -> Result<Self::Output, Box<dyn std::any::Any + Send>>;
}

impl<F, T> CatchUnwindOrRun for std::panic::AssertUnwindSafe<F>
where
    F: std::future::Future<Output = T>,
{
    type Output = T;
    async fn catch_unwind_or_run(self) -> Result<T, Box<dyn std::any::Any + Send>> {
        use futures::FutureExt as _;
        self.catch_unwind().await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs"]
async fn binds_a_ready_member_and_publishes_everything_a_consumer_needs() {
    with_scratch("bind", |s| async move {
        let pool = make_pool(&s, "p", Some("rev-1")).await;
        make_member(&s, &pool, "m1", "rev-1", false).await;
        make_claim(&s, "c1", "p", 900).await;

        reconcile(&s, "c1").await;

        let claim = s.claims().get("c1").await.unwrap();
        let st = claim.status.as_ref().expect("status published");
        assert_eq!(st.phase, ClaimPhase::Bound);
        assert_eq!(
            st.virtual_machine_ref.as_ref().map(|r| r.name.as_str()),
            Some("m1")
        );
        let nonce = st.nonce.as_ref().expect("a nonce is minted at bind");
        assert_eq!(nonce.len(), 32, "128 bits of hex");
        assert!(st.bound_at.is_some() && st.expires_at.is_some());

        let member = s.vms().get("m1").await.unwrap();
        assert_eq!(
            member.labels().get(LABEL_CLAIM).map(String::as_str),
            Some("c1"),
            "the claim label is what 'claimed' means to the pool"
        );
        let ann = member.annotations();
        assert_eq!(
            ann.get(ANNOTATION_SUBJECT_ISSUER).map(String::as_str),
            Some("https://issuer.example.com")
        );
        assert_eq!(
            ann.get(ANNOTATION_SUBJECT_ID).map(String::as_str),
            Some("subject-c1")
        );
        s
    })
    .await;
}

/// **The invariant the whole design rests on.** Two claims, one Ready
/// member: exactly one must bind it. A fake client that accepted both
/// patches would pass every offline test while handing one VM to two
/// subjects.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs"]
async fn two_claims_cannot_bind_the_same_member() {
    with_scratch("race", |s| async move {
        let pool = make_pool(&s, "p", Some("rev-1")).await;
        make_member(&s, &pool, "only", "rev-1", false).await;
        make_claim(&s, "a", "p", 900).await;
        make_claim(&s, "b", "p", 900).await;

        // Both read the same snapshot, then both try to take it.
        let (ca, cb) = (
            s.claims().get("a").await.unwrap(),
            s.claims().get("b").await.unwrap(),
        );
        let (ra, rb) = tokio::join!(
            claim::reconcile(Arc::new(ca), s.ctx()),
            claim::reconcile(Arc::new(cb), s.ctx()),
        );
        ra.expect("reconcile a");
        rb.expect("reconcile b");

        let bound: Vec<String> = s
            .claims()
            .list(&Default::default())
            .await
            .unwrap()
            .into_iter()
            .filter(|c| phase(c) == ClaimPhase::Bound)
            .map(|c| c.name_any())
            .collect();
        assert_eq!(
            bound.len(),
            1,
            "exactly one claim may bind one member; bound: {bound:?}"
        );

        let member = s.vms().get("only").await.unwrap();
        assert_eq!(
            member.labels().get(LABEL_CLAIM),
            Some(&bound[0]),
            "the member's label must name the winner"
        );
        s
    })
    .await;
}

/// Decision 3: after binding, deleting the pool must not take the sandbox
/// with it. That only holds if the apiserver ends up with the claim as the
/// member's sole controller — a fact about merge-patch semantics, not about
/// our code.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs"]
async fn binding_re_parents_the_member_from_the_pool_to_the_claim() {
    with_scratch("reparent", |s| async move {
        let pool = make_pool(&s, "p", Some("rev-1")).await;
        make_member(&s, &pool, "m1", "rev-1", false).await;
        let before = s.vms().get("m1").await.unwrap();
        assert_eq!(
            before.owner_references()[0].kind,
            "VirtualMachinePool",
            "fixture starts owned by the pool"
        );

        make_claim(&s, "c1", "p", 900).await;
        reconcile(&s, "c1").await;

        let after = s.vms().get("m1").await.unwrap();
        let owners: Vec<&str> = after
            .owner_references()
            .iter()
            .map(|o| o.kind.as_str())
            .collect();
        assert_eq!(
            owners,
            vec!["VirtualMachineClaim"],
            "the claim must be the sole owner; the pool must not remain one"
        );
        s
    })
    .await;
}

/// Decision 6, the guarantee that makes a claim worth holding: the claim
/// object must not disappear while its VM still exists.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs"]
async fn a_claim_survives_its_own_deletion_until_the_member_is_gone() {
    with_scratch("finalizer", |s| async move {
        let pool = make_pool(&s, "p", Some("rev-1")).await;
        // Blocks deletion, standing in for the provider's finalizer.
        make_member(&s, &pool, "m1", "rev-1", true).await;
        make_claim(&s, "c1", "p", 900).await;
        reconcile(&s, "c1").await;

        let claim = s.claims().get("c1").await.unwrap();
        assert!(
            claim.finalizers().contains(&CLAIM_FINALIZER.to_string()),
            "a bound claim must hold its finalizer"
        );

        s.claims()
            .delete("c1", &DeleteParams::default())
            .await
            .expect("delete claim");
        reconcile_lenient(&s, "c1").await;

        let still_there = s.claims().get_opt("c1").await.unwrap();
        assert!(
            still_there.is_some(),
            "the claim must outlive the delete while its member is terminating"
        );
        let member = s.vms().get("m1").await.unwrap();
        assert!(
            member.metadata.deletion_timestamp.is_some(),
            "the member must be terminating"
        );

        // The "backend teardown" completes.
        s.vms()
            .patch(
                "m1",
                &PatchParams::default(),
                &Patch::Merge(json!({ "metadata": { "finalizers": [] } })),
            )
            .await
            .expect("release the member");
        wait_gone(&s, "m1").await;

        reconcile_lenient(&s, "c1").await;
        assert!(
            s.claims().get_opt("c1").await.unwrap().is_none(),
            "once the member is gone the claim must release"
        );
        s
    })
    .await;
}

/// Decision 5: a deadline, not a grace period.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs"]
async fn expiry_destroys_the_member() {
    with_scratch("expiry", |s| async move {
        let pool = make_pool(&s, "p", Some("rev-1")).await;
        make_member(&s, &pool, "m1", "rev-1", false).await;
        make_claim(&s, "c1", "p", SHORT_TTL_SECS).await;
        reconcile(&s, "c1").await;
        assert_eq!(
            phase(&s.claims().get("c1").await.unwrap()),
            ClaimPhase::Bound
        );

        tokio::time::sleep(Duration::from_secs(SHORT_TTL_SECS + 1)).await;
        reconcile_lenient(&s, "c1").await;

        wait_gone(&s, "m1").await;
        assert!(
            s.vms().get_opt("m1").await.unwrap().is_none(),
            "the member must be destroyed at expiry"
        );
        s
    })
    .await;
}

/// Decision 7: terminal, and never a rebind — even with a fresh member
/// sitting right there. A consumer holding this claim believes it is
/// talking to one specific VM.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs"]
async fn a_vanished_member_fails_the_claim_and_is_never_rebound() {
    with_scratch("failed", |s| async move {
        let pool = make_pool(&s, "p", Some("rev-1")).await;
        make_member(&s, &pool, "m1", "rev-1", false).await;
        make_claim(&s, "c1", "p", 900).await;
        reconcile(&s, "c1").await;

        // Yank the member out from under the claim, and offer a replacement.
        s.vms()
            .delete("m1", &DeleteParams::default())
            .await
            .expect("delete member");
        wait_gone(&s, "m1").await;
        make_member(&s, &pool, "m2", "rev-1", false).await;

        reconcile(&s, "c1").await;
        let claim = s.claims().get("c1").await.unwrap();
        assert_eq!(phase(&claim), ClaimPhase::Failed);

        reconcile(&s, "c1").await;
        let claim = s.claims().get("c1").await.unwrap();
        assert_eq!(
            claim
                .status
                .as_ref()
                .and_then(|s| s.virtual_machine_ref.as_ref())
                .map(|r| r.name.as_str()),
            Some("m1"),
            "a Failed claim must never be silently rebound to another member"
        );
        assert!(
            s.vms()
                .get("m2")
                .await
                .unwrap()
                .labels()
                .get(LABEL_CLAIM)
                .is_none(),
            "the replacement must stay unclaimed"
        );
        s
    })
    .await;
}

/// The pool side of the invariant: once a member carries the claim label it
/// is `Claimed` forever, so a second claim finds nothing.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs"]
async fn a_claimed_member_is_never_offered_to_a_second_claim() {
    with_scratch("exclusive", |s| async move {
        let pool = make_pool(&s, "p", Some("rev-1")).await;
        make_member(&s, &pool, "only", "rev-1", false).await;
        make_claim(&s, "first", "p", 900).await;
        reconcile(&s, "first").await;

        make_claim(&s, "second", "p", 900).await;
        reconcile(&s, "second").await;

        let second = s.claims().get("second").await.unwrap();
        assert_eq!(phase(&second), ClaimPhase::Pending);
        let reason = second
            .status
            .as_ref()
            .and_then(|st| st.conditions.first())
            .map(|c| c.reason.clone())
            .unwrap_or_default();
        assert_eq!(reason, pool_condition_reasons::NO_MEMBER_AVAILABLE);
        s
    })
    .await;
}

/// Decision 11: "no capacity" and "that pool does not exist" must be
/// distinguishable without reading a second object.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs"]
async fn a_claim_against_a_missing_pool_says_so() {
    with_scratch("nopool", |s| async move {
        make_claim(&s, "c1", "does-not-exist", 900).await;
        reconcile(&s, "c1").await;

        let claim = s.claims().get("c1").await.unwrap();
        assert_eq!(phase(&claim), ClaimPhase::Pending);
        let cond = claim
            .status
            .as_ref()
            .and_then(|st| st.conditions.first().cloned())
            .expect("a condition explaining the wait");
        assert_eq!(cond.reason, pool_condition_reasons::POOL_NOT_FOUND);
        assert!(cond.message.contains("does-not-exist"), "{}", cond.message);
        s
    })
    .await;
}

/// Poll until an object is really gone. Deletion is asynchronous, and
/// asserting on `get_opt` immediately after a delete is the classic flake.
async fn wait_gone(s: &Scratch, name: &str) {
    const ATTEMPTS: usize = 60;
    for _ in 0..ATTEMPTS {
        if s.vms().get_opt(name).await.unwrap().is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("{name} was still present after waiting for deletion");
}
