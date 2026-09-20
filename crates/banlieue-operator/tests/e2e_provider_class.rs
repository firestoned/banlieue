// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `ProviderClass` changes must reach the workloads that reference the class
//! (ADR-0014).
//!
//! Two failure modes, both invisible to the create path every other suite
//! exercises:
//!
//! - **Swapping `spec.providerClassRef`** must prune the superseded workload.
//!   The stale ClusterRoleBinding is the dangerous one: nothing owns it, so
//!   garbage collection cannot reach it, and a name-based cleanup computed from
//!   the Provider's *current* class could never find it again.
//! - **Editing the class image** must roll the existing workload. If
//!   server-side apply were wrong — an immutable field in the patch, a field
//!   the operator does not actually own — creation would still look perfect
//!   while every subsequent edit silently did nothing.
//!
//! `#[ignore]`d by default so `cargo test` stays hermetic. Run it with:
//!
//! ```sh
//! make kind-e2e-class
//! ```

mod e2e_common;

use std::time::Instant;

use banlieue_api::banlieue::{Provider, ProviderClass};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::ServiceAccount;
use k8s_openapi::api::rbac::v1::ClusterRoleBinding;
use kube::Api;
use kube::api::{DeleteParams, Patch, PatchParams, PostParams};

use e2e_common::{
    E2E_CLASS, E2E_NAMESPACE, E2E_PINNED_CLASS, E2E_PROVIDER, E2E_SWAPPED_WORKLOAD, E2E_WORKLOAD,
    E2E_WORKLOAD_NAMESPACE, WATCH_PROPAGATION_BUDGET, client, create_workload_namespace,
    pinned_class, provider, setup, teardown, teardown_pinned, wait_for, wait_until_gone,
};

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
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e-class`"]
async fn changing_the_provider_class_prunes_the_previous_workload() {
    let client = client().await;
    setup(&client).await;
    teardown_pinned(&client).await;

    create_workload_namespace(&client).await;

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
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e-class`"]
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
