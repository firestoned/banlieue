// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The workload a `Provider` produces: creation, shape, ownership, status,
//! events, and garbage collection, against a **real Kubernetes API server**
//! (ADR-0014).
//!
//! This is the core of the operator's contract. Every other e2e suite asserts
//! a variation on it — pause, a pinned namespace, a class edit — while this one
//! asserts the straight-line path, once per backend.
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
//! make kind-e2e-workload
//! ```
//!
//! The spawned provider pods are expected to stay NotReady — see
//! `e2e_common` for why that is the contract, not a defect.

mod e2e_common;

use std::collections::BTreeMap;

use banlieue_api::banlieue::{
    ImagePullPolicy, LoggingSpec, Provider, ProviderClass, ProviderClassSpec, ProviderImage,
};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::ServiceAccount;
use k8s_openapi::api::rbac::v1::{ClusterRoleBinding, Role, RoleBinding};
use kube::api::{DeleteParams, PostParams};
use kube::{Api, Resource};

use e2e_common::{
    E2E_CLUSTER_BINDING, E2E_NAMESPACE, E2E_PROVIDER, E2E_SECRET, E2E_WORKLOAD,
    OPERATOR_FIELD_MANAGER, client, e2e_image, provider, setup, split_image, teardown, wait_for,
    wait_until_gone,
};

#[tokio::test]
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e-workload`"]
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
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e-workload`"]
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
