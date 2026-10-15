// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Pool → real vCenter VMs → claim → release, end to end (ADR-0046/0047).
//!
//! `VirtualMachinePool` is already confirmed live on vSphere (roadmap 17,
//! 2026-09-23: a 2-member `GuestReady`-gated pool went `Warm=True` in
//! 130.3s). `VirtualMachineClaim`'s bind/hold/release mechanics are
//! backend-neutral — they never touch the provider (ADR-0047 Decision 9) —
//! so this suite is the vSphere twin of
//! `banlieue-provider-libvirt/tests/e2e_pool_claim.rs`, filling the one gap
//! roadmap 17 names explicitly: "a live vSphere claim-bind test".
//!
//! Assumes banlieue is **already installed and running**, the named
//! `Provider` is `Ready`, and the named `VMImage` already reports `Ready` —
//! this tests pools and claims, not the install and not the image pipeline.
//!
//! ```sh
//! export KUBECONFIG=~/dev/kubeconfig/homelab.yaml
//! BANLIEUE_E2E_PROVIDER=<provider> \
//!   BANLIEUE_E2E_IMAGE=<ready VMImage> \
//!   BANLIEUE_E2E_CLASS=<VMClass> \
//!   VSPHERE_ENDPOINT=https://bar.foo.io/sdk \
//!   VSPHERE_USERNAME='svc-banlieue@vsphere.local' \
//!   VSPHERE_PASSWORD='…' \
//!   cargo test -p banlieue-provider-vsphere --test e2e_pool_claim \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! or `make vsphere-pool-claim-e2e`.
//!
//! `BANLIEUE_E2E_NAMESPACE` must be the Provider's namespace (members only
//! schedule against Providers in their own namespace); defaults to
//! `banlieue-system`.
//!
//! Optional, mirroring the libvirt suite:
//!
//! - `BANLIEUE_E2E_READINESS`: the pool's `spec.readiness`, default
//!   `InfrastructureReady`. Use `GuestReady` with a `Deferred` image to
//!   exercise ADR-0043's vSphere transport (`guestinfo.banlieue.phase`).
//! - `BANLIEUE_E2E_USER_DATA`: a cloud-config file every member gets.
//! - `BANLIEUE_E2E_CLAIM_AS` / `_SUBJECT_ID` / `_ISSUER`: impersonate a
//!   claim subject so a cluster running the claim subject policy
//!   (`deploy/admission/virtualmachineclaim-subject-authorization.yaml`)
//!   checks a real identity instead of refusing the suite's admin one.
//! - `BANLIEUE_E2E_POOL`: reuse an already-running `VirtualMachinePool`
//!   instead of creating a scratch one, for
//!   `a_claimed_member_is_destroyed_on_vcenter_when_the_claim_is_released`
//!   only — the other two tests need to own the pool outright (one deletes
//!   it, the other bumps its image) and refuse to run against a shared one.
//!   This run creates and deletes only the claim; the pool's own
//!   `warmReplicas` is read from its live spec rather than assumed, and
//!   `BANLIEUE_E2E_PROVISION_TIMEOUT_SECS` (default 720) should be raised
//!   past the pool's own `provisioningTimeoutSeconds` when it differs from
//!   this suite's default.
//! - `BANLIEUE_E2E_IMAGE_BUMP_SOURCE`: a second real `importFrom` the
//!   rollout test bumps its scratch image's `spec.sources[0]` to. Required
//!   for a managed-build fixture image (`Url` source, like
//!   `debian-dev-v0.4.0`): its revision is the imported `OSArtifact`'s own
//!   Kubernetes UID, which only changes with a new `importFrom` digest —
//!   `spec.osVersion` alone moves `metadata.generation` but never that UID,
//!   so without this the rollout test times out waiting for a roll that
//!   never starts (found live 2026-10-15).
//! - `BANLIEUE_E2E_ROLLOUT_TIMEOUT_SECS`: how long the rollout test waits
//!   for every member to carry the new revision, counted from the bump
//!   (default 2400). A managed-build roll is a full ISO build, a vCenter
//!   import Job per zone, then two provisions; 900 s expired narrowly
//!   while availability was correctly held at `warmReplicas`.
//!
//! On-host verification connects **directly to vCenter** with the same
//! `VSphereClient`/`VimClientFactory` pair `live_vcenter.rs` uses — not
//! `govc`, so the suite proves the exact transport the provider itself
//! speaks (JSON, not SOAP; see ADR-0009). `VSPHERE_ENDPOINT`/`_USERNAME`/
//! `_PASSWORD` are optional, same as libvirt's `LIBVIRT_HOST`/`_TLS_DIR`:
//! without them every on-host assertion is skipped and the run proves
//! strictly less. `VSPHERE_INSECURE=true` / `VSPHERE_CA_BUNDLE=<path>` work
//! as in `live_vcenter.rs`.
//!
//! A member's backend identity is its `VSphereMachine`'s `status.vmRef`
//! moref (set once `clone_vm` returns), reached by reading the claimed
//! `VirtualMachine`'s `status.infrastructureRef`. "Exists" is read through
//! [`VSphereClient::power_state`] rather than a dedicated lookup — vCenter
//! itself distinguishes "gone" from every other failure with
//! `ManagedObjectNotFound`, the same signal `destroy_vm` already matches on
//! (`client/vim.rs`), so this suite reuses that string match instead of
//! introducing a second way to ask the same question.
//!
//! Not ported from the libvirt suite: the sabotage/reap test. Its injury
//! (`DOMAIN_UPDATE_DEVICE_FLAGS(link down)` / eject-installer, live-only) has
//! no vSphere equivalent on `VSphereClient` today — adding one is follow-up
//! work, not this gap. Every run names its objects with a unique prefix and
//! tears down exactly those, including after a failure.
use std::time::{Duration, Instant};

use banlieue_api::banlieue::ProviderConnection;
use banlieue_api::banlieue::{
    LABEL_CLAIM, LABEL_POOL, LABEL_POOL_IMAGE_REVISION, VMImage, VirtualMachine,
    VirtualMachineClaim, VirtualMachinePool,
};
use banlieue_api::common::LocalObjectReference;
use banlieue_api::infrastructure::VSphereMachine;
use banlieue_provider_vsphere::client::{
    Credentials, VSphereClient, VSphereClientFactory, VimClientFactory,
    install_default_crypto_provider,
};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::{Client, ResourceExt};
use serde_json::json;

/// Members are real VMs: a clone + power-on is minutes against a real
/// vCenter. Override with `BANLIEUE_E2E_PROVISION_TIMEOUT_SECS` when a
/// reused pool's own `provisioningTimeoutSeconds` runs longer than this.
const DEFAULT_PROVISION_TIMEOUT_SECS: u64 = 12 * 60;
/// Teardown crosses two finalizers and vCenter; give it room.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(6 * 60);
const POLL: Duration = Duration::from_secs(5);

/// Warm members the pool keeps. Two, not one: with one member a refill and a
/// claim are indistinguishable, and "the pool refilled after the claim" is
/// one of the properties under test.
const WARM: u32 = 2;
/// How long a claim holds. Long enough that expiry never races the test.
const CLAIM_TTL_SECS: u64 = 3600;
/// `spec.readiness` when `BANLIEUE_E2E_READINESS` is unset.
const DEFAULT_READINESS: &str = "InfrastructureReady";
/// The ConfigMap key a member's user-data is read from.
const USER_DATA_KEY: &str = "user-data";
/// Claim subject when `BANLIEUE_E2E_SUBJECT_ID` is unset.
const DEFAULT_SUBJECT_ID: &str = "e2e-subject-0c3b7f2e";
/// Claim issuer when `BANLIEUE_E2E_ISSUER` is unset: the policy's shipped
/// placeholder allowlist entry.
const DEFAULT_ISSUER: &str = "https://issuer.example.com";
/// Time the rollout test has to replace both members, from the bump. A
/// managed-build image (`Url` source) rolls through a full ISO build, one
/// vCenter import Job per zone, and two provisions before the first
/// replacement is Ready; the original 15 minutes expired narrowly on a real
/// vCenter (2026-10-15) while the pool was correctly holding
/// `available >= warmReplicas`. Override with
/// `BANLIEUE_E2E_ROLLOUT_TIMEOUT_SECS`.
const DEFAULT_ROLLOUT_TIMEOUT_SECS: u64 = 40 * 60;

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

struct Fixture {
    provider: String,
    image: String,
    class: String,
    readiness: String,
    user_data: Option<String>,
}

fn provision_timeout() -> Duration {
    let secs = std::env::var("BANLIEUE_E2E_PROVISION_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PROVISION_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

fn rollout_timeout() -> Duration {
    let secs = std::env::var("BANLIEUE_E2E_ROLLOUT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_ROLLOUT_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// An already-running pool to bind against instead of a scratch one —
/// `a_claimed_member_is_destroyed_on_vcenter_when_the_claim_is_released`
/// only; see the module docs.
fn existing_pool_name() -> Option<String> {
    std::env::var("BANLIEUE_E2E_POOL").ok()
}

/// A second, real `importFrom` to bump the rollout test's scratch image to.
///
/// Required for a meaningful rollout on vSphere: `pool.rs::image_revision`
/// prefers `status.buildArtifact.osArtifactUid` — the imported `OSArtifact`'s
/// own Kubernetes UID — over `metadata.generation` whenever an image has a
/// managed build, and a managed build only gets a new `OSArtifact` (and so a
/// new UID) from a new `importFrom` digest. `debian-dev-v0.4.0`'s own image
/// is exactly this shape (`Url` source, managed build): bumping `osVersion`
/// alone moves `metadata.generation` but leaves `importFrom` — and so
/// `osArtifactUid` and the pool's revision — untouched, so the pool
/// correctly never rolls. Found live 2026-10-15: 15 minutes of `osVersion`
/// bumping produced zero `OSArtifact` rebuilds and the wait timed out.
fn image_bump_source() -> Option<String> {
    std::env::var("BANLIEUE_E2E_IMAGE_BUMP_SOURCE").ok()
}

fn fixture() -> Option<Fixture> {
    let user_data = std::env::var("BANLIEUE_E2E_USER_DATA").ok().map(|p| {
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("BANLIEUE_E2E_USER_DATA {p}: {e}"))
    });
    Some(Fixture {
        provider: std::env::var("BANLIEUE_E2E_PROVIDER").ok()?,
        image: std::env::var("BANLIEUE_E2E_IMAGE").ok()?,
        class: std::env::var("BANLIEUE_E2E_CLASS").ok()?,
        readiness: std::env::var("BANLIEUE_E2E_READINESS")
            .unwrap_or_else(|_| DEFAULT_READINESS.into()),
        user_data,
    })
}

/// Settings for a **direct** vCenter connection, used only for the on-host
/// checks — separate from the `Provider`'s own stored credentials, which
/// this suite never reads (it exercises the API-server-facing path, same as
/// `make pool-claim-e2e`).
struct VCenterSettings {
    connection: ProviderConnection,
    credentials: Credentials,
    ca_bundle_pem: Option<String>,
}

fn vcenter_settings() -> Option<VCenterSettings> {
    let endpoint = std::env::var("VSPHERE_ENDPOINT").ok()?;
    let username = std::env::var("VSPHERE_USERNAME").ok()?;
    let password = std::env::var("VSPHERE_PASSWORD").ok()?;
    let insecure = std::env::var("VSPHERE_INSECURE")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);
    let ca_bundle_pem = std::env::var("VSPHERE_CA_BUNDLE").ok().map(|path| {
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading VSPHERE_CA_BUNDLE {path}: {e}"))
    });
    Some(VCenterSettings {
        connection: ProviderConnection {
            endpoint,
            credentials_ref: Some(LocalObjectReference {
                name: "unused-in-this-harness".to_string(),
            }),
            insecure_skip_tls_verify: insecure,
            ca_bundle: None,
        },
        credentials: Credentials { username, password },
        ca_bundle_pem,
    })
}

async fn connect(settings: &VCenterSettings) -> Box<dyn VSphereClient> {
    install_default_crypto_provider();
    VimClientFactory::new()
        .build(
            &settings.connection,
            &settings.credentials,
            settings.ca_bundle_pem.as_deref(),
        )
        .await
        .expect("connecting to vCenter failed")
}

/// Whether vCenter still has a VM at `moref`.
///
/// `power_state` rather than a dedicated existence check: there isn't one on
/// [`VSphereClient`], and `destroy_vm`'s own idempotence already treats
/// `ManagedObjectNotFound` as "gone" (`client/vim.rs`) — this reuses the
/// same signal instead of inventing a second one.
async fn vm_exists(client: &dyn VSphereClient, moref: &str) -> bool {
    match client.power_state(moref).await {
        Ok(_) => true,
        Err(e)
            if e.to_string()
                .to_lowercase()
                .contains("managedobjectnotfound") =>
        {
            false
        }
        Err(e) => panic!("reading power state of {moref}: {e}"),
    }
}

/// The vCenter moref backing a member, read through its `VirtualMachine` →
/// `VSphereMachine` chain — exactly what the controller itself follows
/// (`status.infrastructureRef`), not a name guess.
async fn member_moref(k8s: &Client, namespace: &str, member: &str) -> Option<String> {
    let vms: Api<VirtualMachine> = Api::namespaced(k8s.clone(), namespace);
    let infra_ref = vms.get(member).await.ok()?.status?.infrastructure_ref?;
    let infra_ns = infra_ref.namespace.as_deref().unwrap_or(namespace);
    let machines: Api<VSphereMachine> = Api::namespaced(k8s.clone(), infra_ns);
    machines.get(&infra_ref.name).await.ok()?.status?.vm_ref
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

/// One run's objects, in the namespace the `Provider` lives in. Isolation is
/// by name prefix, not a generated namespace — candidate `Provider`s are
/// drawn from the member's own namespace, so a pool anywhere else never
/// schedules.
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
    fn configmaps(&self) -> Api<ConfigMap> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }

    async fn try_member_names(&self, pool: &str) -> Option<Vec<String>> {
        self.vms()
            .list(&ListParams::default().labels(&format!("{LABEL_POOL}={pool}")))
            .await
            .map(|l| l.into_iter().map(|vm| vm.name_any()).collect())
            .ok()
    }

    async fn member_names(&self, pool: &str) -> Vec<String> {
        self.try_member_names(pool).await.unwrap_or_default()
    }

    /// Delete this run's objects and verify they are really gone, on vCenter
    /// too. Morefs are resolved *before* deleting — once a `VirtualMachine`
    /// is gone its `VSphereMachine` is on its way out as well, and by the
    /// time teardown could otherwise look it up there is nothing left to
    /// read the moref from.
    async fn teardown(&self, on_host: Option<&VCenterSettings>, pool: &str) {
        eprintln!("  tearing down {}", self.prefix);
        let members = self.member_names(pool).await;
        let mut morefs = Vec::new();
        for m in &members {
            if let Some(moref) = member_moref(&self.client, &self.namespace, m).await {
                morefs.push(moref);
            }
        }

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
            self.try_member_names(pool).await?.is_empty().then_some(())
        })
        .await;
        if gone.is_none() {
            eprintln!(
                "  ⚠ members still present: {:?} — check vCenter for leftover VMs",
                self.member_names(pool).await
            );
        }

        if let Ok(cms) = self.configmaps().list(&ListParams::default()).await {
            for cm in cms
                .into_iter()
                .filter(|c| c.name_any().starts_with(&self.prefix))
            {
                let _ = self
                    .configmaps()
                    .delete(&cm.name_any(), &DeleteParams::background())
                    .await;
            }
        }

        if let Some(settings) = on_host {
            let vc = connect(settings).await;
            for moref in &morefs {
                if vm_exists(vc.as_ref(), moref).await {
                    eprintln!("  ⚠ LEFTOVER VM ON VCENTER: {moref}");
                }
            }
        }
    }

    /// [`Self::teardown`]'s reused-pool counterpart: deletes only this run's
    /// claim(s), never `pool` itself or any of its other members. `pool` is
    /// not owned by this run (`BANLIEUE_E2E_POOL`), so there is nothing else
    /// here to delete or verify gone — the pool's own owner is responsible
    /// for it.
    async fn teardown_claim_only(&self, pool: &str) {
        eprintln!(
            "  tearing down {} (reused pool {pool} left untouched)",
            self.prefix
        );
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
        wait_for_opt(
            "this run's claim(s) to be deleted",
            TEARDOWN_TIMEOUT,
            || async {
                let claims = self.claims().list(&ListParams::default()).await.ok()?;
                claims
                    .into_iter()
                    .all(|c| !c.name_any().starts_with(&self.prefix))
                    .then_some(())
            },
        )
        .await;
    }
}

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
    make_pool_with(s, f, name, &f.image, json!({})).await;
}

async fn make_pool_with(
    s: &Scratch,
    f: &Fixture,
    name: &str,
    image: &str,
    extra: serde_json::Value,
) {
    let mut template = json!({
        "classRef": { "name": f.class },
        "imageRef": { "name": image },
        "desiredPowerState": "PoweredOn",
        "migrationPolicy": "never",
    });
    if let Some(data) = &f.user_data {
        let cm: ConfigMap = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": { "name": name, "namespace": s.namespace },
            "data": { USER_DATA_KEY: data },
        }))
        .expect("user-data ConfigMap");
        s.configmaps()
            .create(&PostParams::default(), &cm)
            .await
            .expect("create user-data ConfigMap");
        template["userData"] = json!({ "configMapRef": { "name": name } });
    }
    let mut spec = json!({
        "warmReplicas": WARM,
        "maxReplicas": WARM + 2,
        "maxSurge": WARM,
        "readiness": f.readiness,
        "template": { "spec": template },
    });
    if let (Some(spec), Some(extra)) = (spec.as_object_mut(), extra.as_object()) {
        spec.extend(extra.clone());
    }
    let pool: VirtualMachinePool = serde_json::from_value(json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "VirtualMachinePool",
        "metadata": { "name": name, "namespace": s.namespace },
        "spec": spec,
    }))
    .expect("pool fixture");
    s.pools()
        .create(&PostParams::default(), &pool)
        .await
        .expect("create pool");
}

async fn make_claim(s: &Scratch, name: &str, pool: &str) {
    let issuer = std::env::var("BANLIEUE_E2E_ISSUER").unwrap_or_else(|_| DEFAULT_ISSUER.into());
    let subject =
        std::env::var("BANLIEUE_E2E_SUBJECT_ID").unwrap_or_else(|_| DEFAULT_SUBJECT_ID.into());
    let claim: VirtualMachineClaim = serde_json::from_value(json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "VirtualMachineClaim",
        "metadata": { "name": name, "namespace": s.namespace },
        "spec": {
            "poolRef": { "name": pool },
            "subject": { "issuer": issuer, "id": subject },
            "ttlSeconds": CLAIM_TTL_SECS,
        },
    }))
    .expect("claim fixture");
    let claims: Api<VirtualMachineClaim> =
        Api::namespaced(claim_client(&s.client).await, &s.namespace);
    claims
        .create(&PostParams::default(), &claim)
        .await
        .expect("create claim");
}

async fn claim_client(admin: &Client) -> Client {
    let Ok(user) = std::env::var("BANLIEUE_E2E_CLAIM_AS") else {
        return admin.clone();
    };
    let mut config = kube::Config::infer().await.expect("kubeconfig");
    config.auth_info.impersonate = Some(user);
    Client::try_from(config).expect("impersonating client")
}

async fn available(s: &Scratch, pool: &str) -> u32 {
    s.pools()
        .get(pool)
        .await
        .ok()
        .and_then(|p| p.status.map(|st| st.available))
        .unwrap_or(0)
}

/// `spec.warmReplicas` of a reused pool — read live rather than assumed,
/// since a reused pool is not this suite's own fixture and need not agree
/// with [`WARM`].
async fn pool_warm_replicas(s: &Scratch, pool: &str) -> u32 {
    s.pools()
        .get(pool)
        .await
        .unwrap_or_else(|e| panic!("BANLIEUE_E2E_POOL {pool}: {e}"))
        .spec
        .warm_replicas
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The whole story: a pool of real vCenter VMs, one handed out, then
/// destroyed. The final assertion is the one no other tier can make — after
/// the claim is gone, vCenter must not have the VM.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real cluster + vCenter; set BANLIEUE_E2E_PROVIDER/_IMAGE/_CLASS"]
async fn a_claimed_member_is_destroyed_on_vcenter_when_the_claim_is_released() {
    let Some(f) = fixture() else {
        panic!("set BANLIEUE_E2E_PROVIDER, BANLIEUE_E2E_IMAGE and BANLIEUE_E2E_CLASS");
    };
    let on_host = vcenter_settings();
    if on_host.is_none() {
        eprintln!("  ⚠ VSPHERE_ENDPOINT/_USERNAME/_PASSWORD unset — skipping on-host checks.");
        eprintln!("    That is the point of this suite; this run proves strictly less.");
    }

    let client = client().await;
    let s = Scratch::new(&client, "poolclaim").await;
    require_ready_provider(&s, &f.provider).await;

    let (pool, owns_pool) = match existing_pool_name() {
        Some(name) => {
            eprintln!(
                "  reusing existing pool {name} (BANLIEUE_E2E_POOL) — this run \
                 creates/deletes only the claim, never the pool"
            );
            (name, false)
        }
        None => (s.name("pool"), true),
    };
    let result = run_release_case(&s, &f, &pool, owns_pool, on_host.as_ref()).await;
    if owns_pool {
        s.teardown(on_host.as_ref(), &pool).await;
    } else {
        s.teardown_claim_only(&pool).await;
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

async fn run_release_case(
    s: &Scratch,
    f: &Fixture,
    pool: &str,
    owns_pool: bool,
    on_host: Option<&VCenterSettings>,
) -> Result<(), Box<dyn std::any::Any + Send>> {
    use futures::FutureExt as _;
    std::panic::AssertUnwindSafe(async {
        if owns_pool {
            make_pool(s, f, pool).await;
        }
        let warm = if owns_pool {
            WARM
        } else {
            pool_warm_replicas(s, pool).await
        };

        wait_for(
            "the pool to reach its warm size",
            provision_timeout(),
            || async { (available(s, pool).await >= warm).then_some(()) },
        )
        .await;

        let members = s.member_names(pool).await;
        if owns_pool {
            assert_eq!(members.len() as u32, warm, "members: {members:?}");
        } else {
            assert!(
                members.len() as u32 >= warm,
                "members: {members:?}, want at least {warm}"
            );
        }
        let member_count_before_claim = members.len() as u32;

        let vc = if let Some(settings) = on_host {
            let vc = connect(settings).await;
            for m in &members {
                let moref = member_moref(&s.client, &s.namespace, m)
                    .await
                    .unwrap_or_else(|| panic!("member {m} is Ready but has no vmRef yet"));
                assert!(
                    vm_exists(vc.as_ref(), &moref).await,
                    "member {m} is Ready but vCenter has no VM {moref}"
                );
            }
            eprintln!("  ✓ both warm members exist as real vCenter VMs");
            Some(vc)
        } else {
            None
        };

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

        let bound_moref = member_moref(&s.client, &s.namespace, &bound).await;
        if let (Some(vc), Some(moref)) = (&vc, &bound_moref) {
            assert!(
                vm_exists(vc.as_ref(), moref).await,
                "binding must not disturb the running VM"
            );
        }

        // The pool must not count a claimed member as warm, and must refill.
        // Wait on the replacement, not `available >= warm`: the latter is
        // still true from before the claim until the pool next reconciles.
        // Compared against the member count observed before this claim
        // (not the `warm` target) so a reused pool already above warm
        // (extra members from an earlier claim) still proves a refill
        // happened, rather than passing trivially because it started high.
        wait_for(
            "the pool to build a replacement for the claimed member",
            provision_timeout(),
            || async {
                (s.member_names(pool).await.len() as u32 > member_count_before_claim).then_some(())
            },
        )
        .await;
        wait_for("the pool to be warm again", provision_timeout(), || async {
            (available(s, pool).await >= warm).then_some(())
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
        if let (Some(vc), Some(moref)) = (&vc, &bound_moref) {
            wait_for(
                "vCenter to finish destroying the released VM",
                TEARDOWN_TIMEOUT,
                || async { (!vm_exists(vc.as_ref(), moref).await).then_some(()) },
            )
            .await;
            eprintln!("  ✓ the released VM is gone from vCenter");
        }
    })
    .catch_unwind()
    .await
}

/// ADR-0047 Decision 3, on real vCenter VMs: deleting the pool must not take
/// a sandbox somebody is using with it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real cluster + vCenter; set BANLIEUE_E2E_PROVIDER/_IMAGE/_CLASS"]
async fn deleting_a_pool_leaves_a_claimed_vm_running() {
    assert!(
        existing_pool_name().is_none(),
        "this test deletes the pool it creates — unset BANLIEUE_E2E_POOL \
         (meant for the claim-release test only) before running it"
    );
    let Some(f) = fixture() else {
        panic!("set BANLIEUE_E2E_PROVIDER, BANLIEUE_E2E_IMAGE and BANLIEUE_E2E_CLASS");
    };
    let on_host = vcenter_settings();
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
            provision_timeout(),
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
        let bound_moref = member_moref(&s.client, &s.namespace, &bound).await;

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
        if let (Some(settings), Some(moref)) = (&on_host, &bound_moref) {
            let vc = connect(settings).await;
            assert!(
                vm_exists(vc.as_ref(), moref).await,
                "deleting the pool destroyed a sandbox in use: VM {moref} is gone"
            );
            eprintln!("  ✓ the claimed VM outlived its pool");
        }
    })
    .catch_unwind()
    .await;

    s.teardown(on_host.as_ref(), &pool).await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

// ---------------------------------------------------------------------------
// Rollout: bumping the image replaces every member without going cold
// ---------------------------------------------------------------------------

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

/// Backend-neutral (no on-host assertions): bump the `VMImage` and watch the
/// pool replace every member while `status.available` never drops below
/// `warmReplicas`. Ported from the libvirt suite verbatim in spirit — the
/// invariant lives entirely in the API server.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real cluster + vCenter; set BANLIEUE_E2E_PROVIDER/_IMAGE/_CLASS"]
async fn bumping_the_image_rolls_the_pool_without_dropping_below_warm() {
    assert!(
        existing_pool_name().is_none(),
        "this test creates its own scratch pool and image — unset BANLIEUE_E2E_POOL \
         (meant for the claim-release test only) before running it"
    );
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

        make_pool_with(
            &s,
            &f,
            &pool,
            &image,
            json!({ "recycleOnImageChange": true }),
        )
        .await;

        wait_for(
            "the pool to reach its warm size",
            provision_timeout(),
            || async { (available(&s, &pool).await >= WARM).then_some(()) },
        )
        .await;

        let before = member_revisions(&s, &pool).await;
        assert_eq!(before.len() as u32, WARM, "members: {before:?}");
        let old_names: Vec<&String> = before.iter().map(|(n, _)| n).collect();
        let old_revisions: Vec<&String> = before.iter().map(|(_, r)| r).collect();
        eprintln!("  members before the bump: {before:?}");

        let bump = match image_bump_source() {
            Some(new_source) => {
                let mut sources = base.spec.sources.clone();
                let first = sources
                    .first_mut()
                    .expect("fixture VMImage has at least one source");
                first.import_from = Some(new_source.clone());
                eprintln!("  bumping via BANLIEUE_E2E_IMAGE_BUMP_SOURCE: {new_source}");
                json!({ "spec": { "sources": sources } })
            }
            None => {
                eprintln!(
                    "  ⚠ BANLIEUE_E2E_IMAGE_BUMP_SOURCE unset — bumping osVersion only. \
                     For a managed-build image (Url source, like this fixture) that does \
                     NOT move the revision the pool watches, so this run will very likely \
                     time out waiting for a rollout that never starts. Set \
                     BANLIEUE_E2E_IMAGE_BUMP_SOURCE to a second real importFrom to bump \
                     against for a meaningful run."
                );
                json!({ "spec": { "osVersion": "13-rolled" } })
            }
        };
        images
            .patch(&image, &PatchParams::default(), &Patch::Merge(bump))
            .await
            .expect("bump scratch image");

        wait_for(
            "every member to be replaced with the new revision",
            rollout_timeout(),
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

    s.teardown(None, &pool).await;
    let _ = images.delete(&image, &DeleteParams::default()).await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}
