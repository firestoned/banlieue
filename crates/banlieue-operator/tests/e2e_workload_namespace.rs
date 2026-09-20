// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The `workloadNamespace` override — the riskiest branch in the design
//! (ADR-0014).
//!
//! When a `ProviderClass` pins `workloadNamespace` away from the Provider's own
//! namespace, the Deployment and ServiceAccount can no longer carry an
//! `ownerReference`: a cross-namespace owner is treated by the garbage
//! collector as MISSING, which would delete the dependent immediately. So they
//! are left unowned and the finalizer has to clean them up itself.
//!
//! A leak here is silent — the Provider disappears and the workload keeps
//! running, still holding its credentials — which is why this branch gets a
//! suite of its own rather than riding along with the straight-line case.
//!
//! `#[ignore]`d by default so `cargo test` stays hermetic. Run it with:
//!
//! ```sh
//! make kind-e2e-workload-namespace
//! ```

mod e2e_common;

use banlieue_api::banlieue::{Provider, ProviderClass};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::ServiceAccount;
use k8s_openapi::api::rbac::v1::{Role, RoleBinding};
use kube::api::{DeleteParams, PostParams};
use kube::{Api, Resource};

use e2e_common::{
    E2E_NAMESPACE, E2E_PINNED_CLASS, E2E_PINNED_PROVIDER, E2E_PINNED_WORKLOAD,
    E2E_WORKLOAD_NAMESPACE, client, create_workload_namespace, pinned_class, provider, setup,
    teardown, teardown_pinned, wait_for, wait_until_gone,
};

#[tokio::test]
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e-workload-namespace`"]
async fn a_pinned_workload_namespace_drops_owner_refs_and_is_finalizer_cleaned() {
    let client = client().await;
    setup(&client).await;
    teardown_pinned(&client).await;

    // The operator does not create the workload namespace — that is an install
    // concern, not a reconcile one.
    create_workload_namespace(&client).await;

    // A class identical to the default one, except it pins workloadNamespace.
    let classes: Api<ProviderClass> = Api::all(client.clone());
    classes
        .create(&PostParams::default(), &pinned_class())
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
