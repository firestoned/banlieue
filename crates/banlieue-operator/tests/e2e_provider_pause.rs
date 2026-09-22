// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Pause semantics: `spec.paused` on a `Provider`, and on the `ProviderClass`
//! every Provider of that class inherits it from (ADR-0014).
//!
//! Both cases assert an **absence**, which is the easiest assertion in the
//! suite to make vacuous: a completely broken operator creates no workload
//! either, and a bare "nothing was created" check passes just as happily. (It
//! did exactly that on the first e2e run, while every reconcile was 403ing —
//! bug-105.) Each test therefore unpauses afterwards and requires the workload
//! to appear, which is what establishes the operator was alive and willing.
//!
//! `#[ignore]`d by default so `cargo test` stays hermetic. Run it with:
//!
//! ```sh
//! make kind-e2e-pause
//! ```

mod e2e_common;

use banlieue_api::banlieue::{Provider, ProviderClass};
use k8s_openapi::api::apps::v1::Deployment;
use kube::Api;
use kube::api::{Patch, PatchParams, PostParams};

use e2e_common::{
    E2E_CLASS, E2E_NAMESPACE, E2E_PROVIDER, E2E_WORKLOAD, QUIESCE_WINDOW, client, provider, setup,
    teardown, wait_for,
};

#[tokio::test]
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e-pause`"]
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
    tokio::time::sleep(QUIESCE_WINDOW).await;

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

/// A paused *class* suspends every Provider of that class, not just one.
///
/// Paired with an unpause, so the absence assertion cannot pass vacuously
/// against a broken operator (bug-105).
#[tokio::test]
#[ignore = "requires a Kubernetes cluster; run `make kind-e2e-pause`"]
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

    tokio::time::sleep(QUIESCE_WINDOW).await;

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
