// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! End-to-end test of the `Provider` → workload contract against a **real
//! Kubernetes API server** (ADR-0014).
//!
//! Every other test in this crate asserts on objects built in memory. None of
//! them can prove the apiserver accepts what we build: a Deployment whose
//! `spec.selector` does not match its pod template, a `resourceNames` rule the
//! RBAC validator rejects, an `ownerReference` with a wrong `apiVersion`, or an
//! SSA patch that silently drops a field because the CRD schema disagrees with
//! the Rust type — all of those pass `cargo test` and fail on first contact
//! with Kubernetes. This suite is where they surface.
//!
//! `#[ignore]`d by default so `cargo test` stays hermetic. Run it with:
//!
//! ```sh
//! make kind-e2e
//! ```
//!
//! # The spawned provider pod is expected to be unhealthy
//!
//! The `Provider` created here points at `vcenter.invalid`, which by RFC 2606
//! can never resolve. Its provider pod will start, fail to reach a backend, and
//! stay NotReady; `status.workload.readyReplicas` will remain `0`.
//!
//! **That is the expected outcome, not a failure.** This suite asserts the
//! operator's contract — that the workload is created, correctly shaped, owned,
//! reported, and garbage-collected. Whether a provider can reach a vCenter is
//! the vSphere provider's concern, tested separately against `vcsim`.
//!
//! Do not "fix" this suite by waiting for pod readiness. It will never be
//! ready, and the job would be permanently and unfixably red.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use banlieue_api::banlieue::{
    ImagePullPolicy, LoggingSpec, Provider, ProviderClass, ProviderClassSpec, ProviderConnection,
    ProviderImage, ProviderSpec,
};
use banlieue_api::common::LocalObjectReference;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{Namespace, Secret, ServiceAccount};
use k8s_openapi::api::rbac::v1::{ClusterRoleBinding, Role, RoleBinding};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{DeleteParams, Patch, PatchParams, PostParams};
use kube::{Api, Client, Resource};

/// Namespace the suite creates and works in. Deliberately not
/// `banlieue-system`, so a failed run cannot leave debris in the install.
const E2E_NAMESPACE: &str = "banlieue-e2e";

/// Cluster-scoped ProviderClass this suite installs.
const E2E_CLASS: &str = "e2e-vsphere";

/// Provider under test.
const E2E_PROVIDER: &str = "e2e-vc";

/// Secret holding the (fake) backend credentials.
const E2E_SECRET: &str = "e2e-vc-creds";

/// Derived name the four NAMESPACED objects share.
const E2E_WORKLOAD: &str = "banlieue-provider-e2e-vsphere-e2e-vc";

/// Derived name of the CLUSTER-SCOPED ClusterRoleBinding, which is
/// namespace-qualified: a cluster-scoped object has no namespace to
/// disambiguate it, so two Providers sharing a name and class in different
/// namespaces would otherwise collide on one object.
const E2E_CLUSTER_BINDING: &str = "banlieue-provider-e2e-vsphere-banlieue-e2e-e2e-vc";

/// `.invalid` is reserved by RFC 2606 and can never resolve, so the spawned
/// provider fails fast and deterministically instead of hanging on DNS.
const UNREACHABLE_ENDPOINT: &str = "https://vcenter.invalid/sdk";

/// Field manager the operator writes under. Must match
/// `banlieue_operator::reconciler::provider::FIELD_MANAGER`.
const OPERATOR_FIELD_MANAGER: &str = "banlieue.io/operator";

/// Namespace used by the `workloadNamespace` override case, where workloads are
/// pinned away from their Provider's namespace.
const E2E_WORKLOAD_NAMESPACE: &str = "banlieue-e2e-workloads";

/// ProviderClass for the override case.
const E2E_PINNED_CLASS: &str = "e2e-vsphere-pinned";

/// Provider for the override case.
const E2E_PINNED_PROVIDER: &str = "e2e-vc-pinned";

/// Derived name of the override case's workload (pinned class + pinned provider).
const E2E_PINNED_WORKLOAD: &str = "banlieue-provider-e2e-vsphere-pinned-e2e-vc-pinned";

/// Derived name after swapping the DEFAULT provider onto the pinned class —
/// pinned class + default provider name, which is a different object again.
const E2E_SWAPPED_WORKLOAD: &str = "banlieue-provider-e2e-vsphere-pinned-e2e-vc";

/// How long to wait for the operator to converge.
const CONVERGE_TIMEOUT: Duration = Duration::from_secs(120);

/// Poll interval while waiting.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Budget for a `ProviderClass` edit to reach the workloads that reference it.
///
/// Deliberately well BELOW the operator's 30s periodic requeue: landing inside
/// this window can only happen if the ProviderClass watch mapped the edit back
/// to its Providers. If the watch regressed, the change would still arrive —
/// just on the requeue — and a generous timeout would call that a pass.
const WATCH_PROPAGATION_BUDGET: Duration = Duration::from_secs(20);

/// Image the spawned provider workload runs. Must already be loaded into the
/// cluster (`make kind-load`); the pod cannot pull from a registry in CI.
fn e2e_image() -> String {
    std::env::var("BANLIEUE_E2E_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/firestoned/banlieue:local-dev".to_string())
}

/// Split `repo:tag` into its parts, tolerating a registry `host:port` prefix by
/// only considering a colon that follows the final `/`.
fn split_image(image: &str) -> (String, String) {
    let tag_start = image.rfind('/').map_or(0, |slash| slash + 1);
    match image[tag_start..].rfind(':') {
        Some(colon) => (
            image[..tag_start + colon].to_string(),
            image[tag_start + colon + 1..].to_string(),
        ),
        None => (image.to_string(), "local-dev".to_string()),
    }
}

/// Connect using the ambient kubeconfig (the kind context in CI).
async fn client() -> Client {
    Client::try_default()
        .await
        .expect("no reachable cluster — run `make kind-e2e`, or point KUBECONFIG at one")
}

/// Poll `check` until it returns `Some`, or fail with `what` after the timeout.
async fn wait_for<T, F, Fut>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + CONVERGE_TIMEOUT;
    loop {
        if let Some(value) = check().await {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {}s waiting for: {what}",
            CONVERGE_TIMEOUT.as_secs()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll until `check` reports the resource is gone.
async fn wait_until_gone<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    wait_for(what, || {
        let fut = check();
        async move { if fut.await { Some(()) } else { None } }
    })
    .await;
}

/// Create the namespace and ProviderClass this suite needs, removing any
/// leftovers from a previous run first so the suite is re-runnable.
async fn setup(client: &Client) {
    teardown(client).await;

    let namespaces: Api<Namespace> = Api::all(client.clone());
    let ns = Namespace {
        metadata: ObjectMeta {
            name: Some(E2E_NAMESPACE.to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    // A namespace terminating from a previous run needs to finish first.
    wait_for("the e2e namespace to be creatable", || {
        let api = namespaces.clone();
        let ns = ns.clone();
        async move { api.create(&PostParams::default(), &ns).await.ok() }
    })
    .await;

    let classes: Api<ProviderClass> = Api::all(client.clone());
    let class = ProviderClass::new(
        E2E_CLASS,
        ProviderClassSpec {
            backend: "vsphere".to_string(),
            image: {
                let (repository, tag) = split_image(&e2e_image());
                ProviderImage {
                    repository,
                    tag,
                    digest: None,
                    // The image is side-loaded into the node, never pulled.
                    pull_policy: Some(ImagePullPolicy::IfNotPresent),
                    pull_secrets: Vec::new(),
                }
            },
            workload_namespace: None,
            replicas: None,
            resources: None,
            node_selector: BTreeMap::new(),
            tolerations: Vec::new(),
            logging: LoggingSpec::default(),
            additional_rules: Vec::new(),
            paused: false,
        },
    );
    classes
        .create(&PostParams::default(), &class)
        .await
        .expect("creating the e2e ProviderClass");

    let secrets: Api<Secret> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(E2E_SECRET.to_string()),
            namespace: Some(E2E_NAMESPACE.to_string()),
            ..Default::default()
        },
        string_data: Some(BTreeMap::from([
            ("username".to_string(), "e2e@vsphere.local".to_string()),
            ("password".to_string(), "not-a-real-password".to_string()),
        ])),
        ..Default::default()
    };
    secrets
        .create(&PostParams::default(), &secret)
        .await
        .expect("creating the e2e credentials Secret");
}

/// Best-effort removal of everything this suite creates.
async fn teardown(client: &Client) {
    let classes: Api<ProviderClass> = Api::all(client.clone());
    let _ = classes.delete(E2E_CLASS, &DeleteParams::default()).await;

    let bindings: Api<ClusterRoleBinding> = Api::all(client.clone());
    let _ = bindings
        .delete(E2E_WORKLOAD, &DeleteParams::default())
        .await;

    let namespaces: Api<Namespace> = Api::all(client.clone());
    let _ = namespaces
        .delete(E2E_NAMESPACE, &DeleteParams::default())
        .await;
}

/// Build the Provider under test.
fn provider(paused: bool) -> Provider {
    let mut p = Provider::new(
        E2E_PROVIDER,
        ProviderSpec {
            provider_class_ref: LocalObjectReference {
                name: E2E_CLASS.to_string(),
            },
            connection: ProviderConnection {
                endpoint: UNREACHABLE_ENDPOINT.to_string(),
                credentials_ref: LocalObjectReference {
                    name: E2E_SECRET.to_string(),
                },
                insecure_skip_tls_verify: true,
                ca_bundle: None,
            },
            capabilities: Default::default(),
            paused,
        },
    );
    p.metadata.namespace = Some(E2E_NAMESPACE.to_string());
    p
}

#[tokio::test]
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e`"]
async fn provider_lifecycle_creates_shapes_and_garbage_collects_its_workload() {
    let client = client().await;
    setup(&client).await;

    let providers: Api<Provider> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    providers
        .create(&PostParams::default(), &provider(false))
        .await
        .expect("creating the e2e Provider");

    // ── Creation ────────────────────────────────────────────────────────────
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    let service_accounts: Api<ServiceAccount> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    let roles: Api<Role> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    let role_bindings: Api<RoleBinding> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    let cluster_role_bindings: Api<ClusterRoleBinding> = Api::all(client.clone());

    let deployment = wait_for("the provider Deployment to be created", || {
        let api = deployments.clone();
        async move { api.get_opt(E2E_WORKLOAD).await.ok().flatten() }
    })
    .await;

    let service_account = wait_for("the provider ServiceAccount", || {
        let api = service_accounts.clone();
        async move { api.get_opt(E2E_WORKLOAD).await.ok().flatten() }
    })
    .await;
    let role = wait_for("the provider Role", || {
        let api = roles.clone();
        async move { api.get_opt(E2E_WORKLOAD).await.ok().flatten() }
    })
    .await;
    let role_binding = wait_for("the provider RoleBinding", || {
        let api = role_bindings.clone();
        async move { api.get_opt(E2E_WORKLOAD).await.ok().flatten() }
    })
    .await;
    let cluster_role_binding = wait_for("the provider ClusterRoleBinding", || {
        let api = cluster_role_bindings.clone();
        async move {
            api.list(&kube::api::ListParams::default().labels(&format!(
                "banlieue.io/provider={E2E_PROVIDER},banlieue.io/provider-namespace={E2E_NAMESPACE}"
            )))
            .await
            .ok()
            .and_then(|l| l.items.into_iter().next())
        }
    })
    .await;
    assert_eq!(
        cluster_role_binding.metadata.name.as_deref(),
        Some(E2E_CLUSTER_BINDING),
        "a cluster-scoped object must carry the Provider's namespace in its name, \
         or two namespaces collide on one binding"
    );

    // ── Shape: the apiserver accepted it, but is it the right thing? ────────
    let pod_spec = deployment
        .spec
        .as_ref()
        .expect("Deployment spec")
        .template
        .spec
        .as_ref()
        .expect("pod spec");
    let args = pod_spec.containers[0]
        .args
        .as_ref()
        .expect("container args");
    assert_eq!(args[0], "provider");
    assert_eq!(args[1], "vsphere");
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--provider-name" && w[1] == E2E_PROVIDER),
        "workload must be scoped to its Provider: {args:?}"
    );
    assert_eq!(
        pod_spec.service_account_name.as_deref(),
        Some(E2E_WORKLOAD),
        "Deployment must run as its own ServiceAccount"
    );

    // ── Least privilege: the whole point of per-instance (ADR-0003) ─────────
    let rules = role.rules.as_ref().expect("Role rules");
    let secret_rule = rules
        .iter()
        .find(|r| {
            r.resources
                .as_ref()
                .is_some_and(|rs| rs.contains(&"secrets".to_string()))
        })
        .expect("a secrets rule");
    assert_eq!(
        secret_rule.resource_names.as_deref(),
        Some([E2E_SECRET.to_string()].as_slice()),
        "the Role must reach exactly one Secret"
    );
    assert!(
        !secret_rule.verbs.contains(&"list".to_string())
            && !secret_rule.verbs.contains(&"watch".to_string()),
        "resourceNames does not constrain list/watch — this would grant every Secret"
    );

    // ── Ownership ───────────────────────────────────────────────────────────
    for (kind, meta) in [
        ("Deployment", deployment.meta()),
        ("ServiceAccount", service_account.meta()),
        ("Role", role.meta()),
        ("RoleBinding", role_binding.meta()),
    ] {
        let owners = meta
            .owner_references
            .as_ref()
            .unwrap_or_else(|| panic!("{kind} must be owned by its Provider"));
        assert_eq!(owners[0].kind, "Provider", "{kind} owner kind");
        assert_eq!(owners[0].name, E2E_PROVIDER, "{kind} owner name");
        assert_eq!(owners[0].controller, Some(true), "{kind} controller flag");
    }
    assert!(
        cluster_role_binding.meta().owner_references.is_none(),
        "a cluster-scoped object owned by a namespaced one is deleted immediately by GC"
    );

    // ── Status ──────────────────────────────────────────────────────────────
    //
    // Only `status.workload` — NOT readiness. The provider cannot reach
    // `vcenter.invalid`, so readyReplicas stays 0 by design (see module docs).
    let observed = wait_for("Provider.status.workload to be published", || {
        let api = providers.clone();
        async move {
            api.get_opt(E2E_PROVIDER)
                .await
                .ok()
                .flatten()
                .and_then(|p| p.status)
                .and_then(|s| s.workload)
        }
    })
    .await;
    assert_eq!(observed.deployment_name, E2E_WORKLOAD);
    assert_eq!(observed.namespace, E2E_NAMESPACE);

    // ── Disjoint status ownership — the central claim of ADR-0012 ───────────
    //
    // The operator must own `status.workload` and NOT `status.conditions`,
    // which belongs to the provider's own field manager. `conditions` is a
    // plain list with no `x-kubernetes-list-type: map`, so two managers writing
    // into it contend over the whole array instead of merging per entry.
    //
    // Asserted over `metadata.managedFields`, which is the apiserver's own
    // record of who owns what. Checking the *values* of conditions instead
    // would be near-vacuous: it passes whenever the operator writes conditions
    // that merely look plausible.
    let current = providers
        .get(E2E_PROVIDER)
        .await
        .expect("re-reading Provider");

    let managed = current
        .metadata
        .managed_fields
        .as_ref()
        .expect("apiserver records managedFields for every server-side apply");

    let operator_entries: Vec<_> = managed
        .iter()
        .filter(|e| e.manager.as_deref() == Some(OPERATOR_FIELD_MANAGER))
        .collect();
    assert!(
        !operator_entries.is_empty(),
        "expected a managedFields entry for {OPERATOR_FIELD_MANAGER}; managers present: {:?}",
        managed
            .iter()
            .filter_map(|e| e.manager.clone())
            .collect::<Vec<_>>()
    );

    let owned = operator_entries
        .iter()
        .filter_map(|e| e.fields_v1.as_ref())
        .map(|f| serde_json::to_string(&f.0).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ");

    assert!(
        owned.contains("workload"),
        "operator should own status.workload, owns: {owned}"
    );
    assert!(
        !owned.contains("conditions"),
        "operator must NOT own status.conditions — that list belongs to the \
         provider's field manager and cannot be merged per-entry by two \
         managers (ADR-0012). Owned fields: {owned}"
    );

    // ── Events: what the controller DID, not just what state it is in ──────
    //
    // Conditions cannot answer "why has this Provider not come up?". Events
    // can, and an operator reaches for `kubectl describe` long before they
    // reach for controller logs — so the events have to actually arrive.
    let events: Api<k8s_openapi::api::events::v1::Event> =
        Api::namespaced(client.clone(), E2E_NAMESPACE);
    let applied = wait_for("a WorkloadApplied event on the Provider", || {
        let api = events.clone();
        async move {
            api.list(&kube::api::ListParams::default())
                .await
                .ok()?
                .items
                .into_iter()
                .find(|e| {
                    e.reason.as_deref() == Some("WorkloadApplied")
                        && e.regarding.as_ref().and_then(|r| r.name.as_deref())
                            == Some(E2E_PROVIDER)
                })
        }
    })
    .await;
    assert_eq!(
        applied.reporting_controller.as_deref(),
        Some("banlieue.io/operator"),
        "events must be attributable to this operator"
    );
    assert!(
        applied
            .note
            .as_ref()
            .is_some_and(|n| n.contains(E2E_WORKLOAD)),
        "the event should name the workload it applied: {:?}",
        applied.note
    );

    // ── Deletion: GC for the owned four, finalizer for the fifth ────────────
    providers
        .delete(E2E_PROVIDER, &DeleteParams::default())
        .await
        .expect("deleting the e2e Provider");

    wait_until_gone("the Provider to be released by its finalizer", || {
        let api = providers.clone();
        async move {
            api.get_opt(E2E_PROVIDER)
                .await
                .map(|p| p.is_none())
                .unwrap_or(false)
        }
    })
    .await;

    wait_until_gone("the ClusterRoleBinding to be finalizer-deleted", || {
        let api = cluster_role_bindings.clone();
        async move {
            api.get_opt(E2E_WORKLOAD)
                .await
                .map(|b| b.is_none())
                .unwrap_or(false)
        }
    })
    .await;

    wait_until_gone("the Deployment to be garbage-collected", || {
        let api = deployments.clone();
        async move {
            api.get_opt(E2E_WORKLOAD)
                .await
                .map(|d| d.is_none())
                .unwrap_or(false)
        }
    })
    .await;

    wait_until_gone("the ServiceAccount to be garbage-collected", || {
        let api = service_accounts.clone();
        async move {
            api.get_opt(E2E_WORKLOAD)
                .await
                .map(|s| s.is_none())
                .unwrap_or(false)
        }
    })
    .await;

    teardown(&client).await;
}

#[tokio::test]
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e`"]
async fn a_paused_provider_gets_no_workload() {
    let client = client().await;
    setup(&client).await;

    let providers: Api<Provider> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    providers
        .create(&PostParams::default(), &provider(true))
        .await
        .expect("creating the paused e2e Provider");

    // There is no positive event to wait for, so allow the operator ample time
    // to have reconciled and then assert nothing was created.
    tokio::time::sleep(Duration::from_secs(20)).await;

    let deployments: Api<Deployment> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    assert!(
        deployments
            .get_opt(E2E_WORKLOAD)
            .await
            .expect("querying deployments")
            .is_none(),
        "a paused Provider must not be given a workload"
    );

    // Absence on its own proves nothing: a completely broken operator creates
    // no workload either, and this assertion would pass just as happily. (It
    // did exactly that on the first e2e run, while every reconcile was 403ing.)
    // Unpausing and requiring the workload to appear is what makes the check
    // above meaningful — it establishes the operator was alive and willing.
    providers
        .patch(
            E2E_PROVIDER,
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({ "spec": { "paused": false } })),
        )
        .await
        .expect("unpausing the e2e Provider");

    wait_for(
        "the workload to appear once the Provider is unpaused",
        || {
            let api = deployments.clone();
            async move { api.get_opt(E2E_WORKLOAD).await.ok().flatten() }
        },
    )
    .await;

    teardown(&client).await;
}

// ---------------------------------------------------------------------------
// workloadNamespace override
//
// The riskiest branch in the design. When a ProviderClass pins
// `workloadNamespace` away from the Provider's own namespace, the Deployment
// and ServiceAccount can no longer carry an ownerReference — a cross-namespace
// owner is treated by the garbage collector as MISSING, which would delete the
// dependent immediately. So they are left unowned and the finalizer has to
// clean them up itself.
//
// A leak here is silent: the Provider disappears and the workload keeps
// running, still holding its credentials.
// ---------------------------------------------------------------------------

/// Remove everything the override case creates.
async fn teardown_pinned(client: &Client) {
    let classes: Api<ProviderClass> = Api::all(client.clone());
    let _ = classes
        .delete(E2E_PINNED_CLASS, &DeleteParams::default())
        .await;

    let bindings: Api<ClusterRoleBinding> = Api::all(client.clone());
    let _ = bindings
        .delete_collection(
            &DeleteParams::default(),
            &kube::api::ListParams::default()
                .labels(&format!("banlieue.io/provider-namespace={E2E_NAMESPACE}")),
        )
        .await;

    let namespaces: Api<Namespace> = Api::all(client.clone());
    let _ = namespaces
        .delete(E2E_WORKLOAD_NAMESPACE, &DeleteParams::default())
        .await;
}

#[tokio::test]
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e`"]
async fn a_pinned_workload_namespace_drops_owner_refs_and_is_finalizer_cleaned() {
    let client = client().await;
    setup(&client).await;
    teardown_pinned(&client).await;

    // The operator does not create the workload namespace — that is an install
    // concern, not a reconcile one.
    let namespaces: Api<Namespace> = Api::all(client.clone());
    wait_for("the pinned workload namespace to be creatable", || {
        let api = namespaces.clone();
        async move {
            api.create(
                &PostParams::default(),
                &Namespace {
                    metadata: ObjectMeta {
                        name: Some(E2E_WORKLOAD_NAMESPACE.to_string()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .ok()
        }
    })
    .await;

    // A class identical to the default one, except it pins workloadNamespace.
    let classes: Api<ProviderClass> = Api::all(client.clone());
    let (repository, tag) = split_image(&e2e_image());
    let mut pinned = ProviderClass::new(
        E2E_PINNED_CLASS,
        ProviderClassSpec {
            backend: "vsphere".to_string(),
            image: ProviderImage {
                repository,
                tag,
                digest: None,
                pull_policy: Some(ImagePullPolicy::IfNotPresent),
                pull_secrets: Vec::new(),
            },
            workload_namespace: Some(E2E_WORKLOAD_NAMESPACE.to_string()),
            replicas: None,
            resources: None,
            node_selector: BTreeMap::new(),
            tolerations: Vec::new(),
            logging: LoggingSpec::default(),
            additional_rules: Vec::new(),
            paused: false,
        },
    );
    pinned.metadata.name = Some(E2E_PINNED_CLASS.to_string());
    classes
        .create(&PostParams::default(), &pinned)
        .await
        .expect("creating the pinned ProviderClass");

    let providers: Api<Provider> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    let mut provider = provider(false);
    provider.metadata.name = Some(E2E_PINNED_PROVIDER.to_string());
    provider.spec.provider_class_ref.name = E2E_PINNED_CLASS.to_string();
    providers
        .create(&PostParams::default(), &provider)
        .await
        .expect("creating the pinned Provider");

    // ── Placement: workload and RBAC land in DIFFERENT namespaces ───────────
    let workload_deployments: Api<Deployment> =
        Api::namespaced(client.clone(), E2E_WORKLOAD_NAMESPACE);
    let workload_sas: Api<ServiceAccount> = Api::namespaced(client.clone(), E2E_WORKLOAD_NAMESPACE);
    let provider_roles: Api<Role> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    let provider_role_bindings: Api<RoleBinding> = Api::namespaced(client.clone(), E2E_NAMESPACE);

    let deployment = wait_for("the pinned Deployment in the workload namespace", || {
        let api = workload_deployments.clone();
        async move { api.get_opt(E2E_PINNED_WORKLOAD).await.ok().flatten() }
    })
    .await;
    let service_account = wait_for("the pinned ServiceAccount", || {
        let api = workload_sas.clone();
        async move { api.get_opt(E2E_PINNED_WORKLOAD).await.ok().flatten() }
    })
    .await;

    // The Role must be created next to the Secret it grants access to, which
    // lives with the Provider — NOT with the Deployment.
    let role = wait_for("the pinned Role in the Provider's namespace", || {
        let api = provider_roles.clone();
        async move { api.get_opt(E2E_PINNED_WORKLOAD).await.ok().flatten() }
    })
    .await;
    let role_binding = wait_for("the pinned RoleBinding", || {
        let api = provider_role_bindings.clone();
        async move { api.get_opt(E2E_PINNED_WORKLOAD).await.ok().flatten() }
    })
    .await;

    // ── Ownership: split by namespace ───────────────────────────────────────
    assert!(
        deployment.meta().owner_references.is_none(),
        "a Deployment in another namespace than its Provider must be UNOWNED — \
         a cross-namespace ownerReference makes the GC delete it immediately"
    );
    assert!(
        service_account.meta().owner_references.is_none(),
        "the ServiceAccount is cross-namespace too and must be unowned"
    );
    assert!(
        role.meta().owner_references.is_some(),
        "the Role shares the Provider's namespace, so it can and should be owned"
    );
    assert!(
        role_binding.meta().owner_references.is_some(),
        "the RoleBinding shares the Provider's namespace, so it should be owned"
    );

    // The binding lives with the Secret but must name the ServiceAccount in the
    // namespace it actually exists in, or it grants nothing.
    let subject = &role_binding.subjects.as_ref().expect("subjects")[0];
    assert_eq!(
        subject.namespace.as_deref(),
        Some(E2E_WORKLOAD_NAMESPACE),
        "RoleBinding subject must point at the ServiceAccount's real namespace"
    );

    // ── Deletion: the finalizer, not the GC, has to do this ─────────────────
    providers
        .delete(E2E_PINNED_PROVIDER, &DeleteParams::default())
        .await
        .expect("deleting the pinned Provider");

    wait_until_gone("the pinned Provider to be released", || {
        let api = providers.clone();
        async move {
            api.get_opt(E2E_PINNED_PROVIDER)
                .await
                .map(|p| p.is_none())
                .unwrap_or(false)
        }
    })
    .await;

    wait_until_gone(
        "the cross-namespace Deployment to be finalizer-deleted (GC cannot reach it)",
        || {
            let api = workload_deployments.clone();
            async move {
                api.get_opt(E2E_PINNED_WORKLOAD)
                    .await
                    .map(|d| d.is_none())
                    .unwrap_or(false)
            }
        },
    )
    .await;

    wait_until_gone(
        "the cross-namespace ServiceAccount to be finalizer-deleted",
        || {
            let api = workload_sas.clone();
            async move {
                api.get_opt(E2E_PINNED_WORKLOAD)
                    .await
                    .map(|s| s.is_none())
                    .unwrap_or(false)
            }
        },
    )
    .await;

    teardown_pinned(&client).await;
    teardown(&client).await;
}

/// Build the workloadNamespace-pinned class used by the override and swap tests.
fn pinned_class() -> ProviderClass {
    let (repository, tag) = split_image(&e2e_image());
    ProviderClass::new(
        E2E_PINNED_CLASS,
        ProviderClassSpec {
            backend: "vsphere".to_string(),
            image: ProviderImage {
                repository,
                tag,
                digest: None,
                pull_policy: Some(ImagePullPolicy::IfNotPresent),
                pull_secrets: Vec::new(),
            },
            workload_namespace: Some(E2E_WORKLOAD_NAMESPACE.to_string()),
            replicas: None,
            resources: None,
            node_selector: BTreeMap::new(),
            tolerations: Vec::new(),
            logging: LoggingSpec::default(),
            additional_rules: Vec::new(),
            paused: false,
        },
    )
}

/// Changing `spec.providerClassRef` must not leave the previous workload behind.
///
/// `deploy/admission/provider-immutability.yaml` makes that field immutable, but
/// ADR-0007 ships those policies as **optional** hardening and states the
/// controller must not depend on them, "falling back to the controller's
/// delete-and-recreate semantics". kind has no admission policy applied, so this
/// is the unhardened path every such cluster runs.
///
/// The stale ClusterRoleBinding is the dangerous one: nothing owns it, so
/// garbage collection cannot reach it, and a name-based cleanup computed from
/// the Provider's *current* class could never find it again — it would leak
/// permanently, still granting a deleted workload's ServiceAccount.
///
/// This swap also moves the workload namespace, so the prune has to find the
/// orphan in a namespace the Provider no longer points at.
#[tokio::test]
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e`"]
async fn changing_the_provider_class_prunes_the_previous_workload() {
    let client = client().await;
    setup(&client).await;
    teardown_pinned(&client).await;

    let namespaces: Api<Namespace> = Api::all(client.clone());
    wait_for("the pinned workload namespace to be creatable", || {
        let api = namespaces.clone();
        async move {
            api.create(
                &PostParams::default(),
                &Namespace {
                    metadata: ObjectMeta {
                        name: Some(E2E_WORKLOAD_NAMESPACE.to_string()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .ok()
        }
    })
    .await;

    let classes: Api<ProviderClass> = Api::all(client.clone());
    classes
        .create(&PostParams::default(), &pinned_class())
        .await
        .expect("creating the pinned ProviderClass");

    // Start on the default class: workload lands in the Provider's namespace.
    let providers: Api<Provider> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    providers
        .create(&PostParams::default(), &provider(false))
        .await
        .expect("creating the e2e Provider");

    let original_deployments: Api<Deployment> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    let original_sas: Api<ServiceAccount> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    let cluster_role_bindings: Api<ClusterRoleBinding> = Api::all(client.clone());

    wait_for("the original workload", || {
        let api = original_deployments.clone();
        async move { api.get_opt(E2E_WORKLOAD).await.ok().flatten() }
    })
    .await;

    // Record the original cluster-scoped binding, which GC can never reclaim.
    let original_binding = wait_for("the original ClusterRoleBinding", || {
        let api = cluster_role_bindings.clone();
        async move {
            api.list(&kube::api::ListParams::default().labels(&format!(
                "banlieue.io/provider={E2E_PROVIDER},banlieue.io/provider-namespace={E2E_NAMESPACE}"
            )))
            .await
            .ok()
            .and_then(|l| l.items.into_iter().next())
        }
    })
    .await;
    let original_binding_name = original_binding
        .metadata
        .name
        .clone()
        .expect("binding has a name");

    // ── The swap ────────────────────────────────────────────────────────────
    providers
        .patch(
            E2E_PROVIDER,
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({
                "spec": { "providerClassRef": { "name": E2E_PINNED_CLASS } }
            })),
        )
        .await
        .expect("changing providerClassRef");

    // The new workload appears in the newly-pinned namespace.
    let pinned_deployments: Api<Deployment> =
        Api::namespaced(client.clone(), E2E_WORKLOAD_NAMESPACE);
    wait_for("the replacement workload in the pinned namespace", || {
        let api = pinned_deployments.clone();
        async move { api.get_opt(E2E_SWAPPED_WORKLOAD).await.ok().flatten() }
    })
    .await;

    // ── And the previous one must be gone ───────────────────────────────────
    wait_until_gone(
        "the superseded Deployment to be pruned (two provider pods would both hold credentials)",
        || {
            let api = original_deployments.clone();
            async move {
                api.get_opt(E2E_WORKLOAD)
                    .await
                    .map(|d| d.is_none())
                    .unwrap_or(false)
            }
        },
    )
    .await;

    wait_until_gone("the superseded ServiceAccount to be pruned", || {
        let api = original_sas.clone();
        async move {
            api.get_opt(E2E_WORKLOAD)
                .await
                .map(|s| s.is_none())
                .unwrap_or(false)
        }
    })
    .await;

    wait_until_gone(
        "the superseded ClusterRoleBinding to be pruned — GC cannot reach it and a \
         name-based cleanup would never find it again",
        || {
            let api = cluster_role_bindings.clone();
            let name = original_binding_name.clone();
            async move {
                api.get_opt(&name)
                    .await
                    .map(|b| b.is_none())
                    .unwrap_or(false)
            }
        },
    )
    .await;

    // Clean up: deleting the Provider must take the replacement with it.
    providers
        .delete(E2E_PROVIDER, &DeleteParams::default())
        .await
        .expect("deleting the e2e Provider");
    wait_until_gone("the Provider to be released", || {
        let api = providers.clone();
        async move {
            api.get_opt(E2E_PROVIDER)
                .await
                .map(|p| p.is_none())
                .unwrap_or(false)
        }
    })
    .await;

    teardown_pinned(&client).await;
    teardown(&client).await;
}

/// The operator must *manage* a workload, not merely create one.
///
/// Every other test here asserts the create path. If server-side apply were
/// wrong — an immutable field in the patch, a selector that cannot change, a
/// field the operator does not actually own — creation would still look
/// perfect while every subsequent edit silently did nothing.
///
/// `ProviderClass` is not watched (mapping a class back to its Providers needs
/// an async lookup kube's synchronous mapper cannot do), so the change lands on
/// the next periodic requeue rather than instantly. That delay is expected.
#[tokio::test]
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e`"]
async fn editing_the_class_image_rolls_the_existing_workload() {
    let client = client().await;
    setup(&client).await;

    let providers: Api<Provider> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    providers
        .create(&PostParams::default(), &provider(false))
        .await
        .expect("creating the e2e Provider");

    let deployments: Api<Deployment> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    let original = wait_for("the initial workload", || {
        let api = deployments.clone();
        async move { api.get_opt(E2E_WORKLOAD).await.ok().flatten() }
    })
    .await;
    let original_image = original
        .spec
        .as_ref()
        .and_then(|s| s.template.spec.as_ref())
        .map(|p| p.containers[0].image.clone())
        .expect("initial image");

    // A tag that certainly differs from whatever the suite installed.
    const ROLLED_TAG: &str = "e2e-rolled";
    let classes: Api<ProviderClass> = Api::all(client.clone());
    let edited_at = Instant::now();
    classes
        .patch(
            E2E_CLASS,
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({
                "spec": { "image": { "tag": ROLLED_TAG } }
            })),
        )
        .await
        .expect("editing the ProviderClass image tag");

    let rolled = wait_for("the workload to pick up the new image", || {
        let api = deployments.clone();
        async move {
            let deployment = api.get_opt(E2E_WORKLOAD).await.ok().flatten()?;
            let image = deployment
                .spec
                .as_ref()
                .and_then(|s| s.template.spec.as_ref())
                .map(|p| p.containers[0].image.clone())?;
            image
                .as_deref()
                .is_some_and(|i| i.ends_with(ROLLED_TAG))
                .then_some(image)
        }
    })
    .await;

    assert_ne!(
        rolled, original_image,
        "the Deployment image must actually change"
    );

    // The edit must have arrived via the ProviderClass watch, not the periodic
    // requeue. Anything inside this budget is far too fast to be the 30s timer.
    let elapsed = edited_at.elapsed();
    assert!(
        elapsed < WATCH_PROPAGATION_BUDGET,
        "class edit took {elapsed:?}, which is slower than the {WATCH_PROPAGATION_BUDGET:?} \
         budget — the ProviderClass watch has probably regressed and the change is arriving \
         on the periodic requeue instead"
    );

    teardown(&client).await;
}

/// A paused *class* suspends every Provider of that class, not just one.
///
/// Paired with an unpause, so the absence assertion cannot pass vacuously
/// against a broken operator (bug-105).
#[tokio::test]
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e`"]
async fn a_paused_class_suspends_its_providers() {
    let client = client().await;
    setup(&client).await;

    let classes: Api<ProviderClass> = Api::all(client.clone());
    classes
        .patch(
            E2E_CLASS,
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({ "spec": { "paused": true } })),
        )
        .await
        .expect("pausing the ProviderClass");

    let providers: Api<Provider> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    providers
        .create(&PostParams::default(), &provider(false))
        .await
        .expect("creating the e2e Provider");

    tokio::time::sleep(Duration::from_secs(20)).await;

    let deployments: Api<Deployment> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    assert!(
        deployments
            .get_opt(E2E_WORKLOAD)
            .await
            .expect("querying deployments")
            .is_none(),
        "a Provider whose CLASS is paused must not be given a workload"
    );

    classes
        .patch(
            E2E_CLASS,
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({ "spec": { "paused": false } })),
        )
        .await
        .expect("unpausing the ProviderClass");

    wait_for("the workload to appear once the class is unpaused", || {
        let api = deployments.clone();
        async move { api.get_opt(E2E_WORKLOAD).await.ok().flatten() }
    })
    .await;

    teardown(&client).await;
}

/// The libvirt backend must produce a correctly shaped workload too.
///
/// Both backends ship a ClusterRole, but until now only vsphere was exercised
/// end to end. This also confirms in-cluster that the spawned libvirt workload
/// is invoked with the flags the builder emits — `--provider-name` among them,
/// which the libvirt provider had to learn to accept.
///
/// Asserts on the Deployment, not on pod health: the endpoint is unreachable by
/// design, exactly as in the vsphere case.
#[tokio::test]
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e`"]
async fn the_libvirt_backend_produces_a_correctly_shaped_workload() {
    const LIBVIRT_CLASS: &str = "e2e-libvirt";
    const LIBVIRT_PROVIDER: &str = "e2e-kvm";
    const LIBVIRT_WORKLOAD: &str = "banlieue-provider-e2e-libvirt-e2e-kvm";

    let client = client().await;
    setup(&client).await;

    let classes: Api<ProviderClass> = Api::all(client.clone());
    let _ = classes
        .delete(LIBVIRT_CLASS, &DeleteParams::default())
        .await;

    let (repository, tag) = split_image(&e2e_image());
    let class = ProviderClass::new(
        LIBVIRT_CLASS,
        ProviderClassSpec {
            backend: "libvirt".to_string(),
            image: ProviderImage {
                repository,
                tag,
                digest: None,
                pull_policy: Some(ImagePullPolicy::IfNotPresent),
                pull_secrets: Vec::new(),
            },
            workload_namespace: None,
            replicas: None,
            resources: None,
            node_selector: BTreeMap::new(),
            tolerations: Vec::new(),
            logging: LoggingSpec::default(),
            additional_rules: Vec::new(),
            paused: false,
        },
    );
    classes
        .create(&PostParams::default(), &class)
        .await
        .expect("creating the libvirt ProviderClass");

    let providers: Api<Provider> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    let mut libvirt_provider = provider(false);
    libvirt_provider.metadata.name = Some(LIBVIRT_PROVIDER.to_string());
    libvirt_provider.spec.provider_class_ref.name = LIBVIRT_CLASS.to_string();
    // `.invalid` again: never resolves, so the pod fails fast and predictably.
    libvirt_provider.spec.connection.endpoint = "qemu+tls://kvm.invalid/system".to_string();
    providers
        .create(&PostParams::default(), &libvirt_provider)
        .await
        .expect("creating the libvirt Provider");

    let deployments: Api<Deployment> = Api::namespaced(client.clone(), E2E_NAMESPACE);
    let deployment = wait_for("the libvirt workload", || {
        let api = deployments.clone();
        async move { api.get_opt(LIBVIRT_WORKLOAD).await.ok().flatten() }
    })
    .await;

    let args = deployment
        .spec
        .as_ref()
        .and_then(|s| s.template.spec.as_ref())
        .map(|p| p.containers[0].args.clone().unwrap_or_default())
        .expect("container args");

    assert_eq!(args[0], "provider");
    assert_eq!(args[1], "libvirt", "must select the libvirt backend");
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--provider-name" && w[1] == LIBVIRT_PROVIDER),
        "the libvirt provider must be scoped to its Provider: {args:?}"
    );

    // The ClusterRoleBinding must reference the libvirt role, not vsphere's.
    let bindings: Api<ClusterRoleBinding> = Api::all(client.clone());
    let binding = wait_for("the libvirt ClusterRoleBinding", || {
        let api = bindings.clone();
        async move {
            api.list(&kube::api::ListParams::default().labels(&format!(
                "banlieue.io/provider={LIBVIRT_PROVIDER},banlieue.io/provider-namespace={E2E_NAMESPACE}"
            )))
            .await
            .ok()
            .and_then(|l| l.items.into_iter().next())
        }
    })
    .await;
    assert_eq!(
        binding.role_ref.name, "banlieue-provider-libvirt",
        "must bind the libvirt backend's shared ClusterRole"
    );

    providers
        .delete(LIBVIRT_PROVIDER, &DeleteParams::default())
        .await
        .expect("deleting the libvirt Provider");
    wait_until_gone("the libvirt Provider to be released", || {
        let api = providers.clone();
        async move {
            api.get_opt(LIBVIRT_PROVIDER)
                .await
                .map(|p| p.is_none())
                .unwrap_or(false)
        }
    })
    .await;
    let _ = classes
        .delete(LIBVIRT_CLASS, &DeleteParams::default())
        .await;

    teardown(&client).await;
}
