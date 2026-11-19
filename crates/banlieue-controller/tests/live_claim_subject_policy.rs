// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The claim subject policy against a **real API server** (ADR-0047
//! Decision 9, amended 2026-10-13: a username prefix per issuer).
//!
//! The policy is CEL in a `ValidatingAdmissionPolicy`, so only an API server
//! can say what it accepts. Each case creates a claim with `dryRun=All` as an
//! impersonated user and asserts admission's verdict; nothing is persisted.
//!
//! ```sh
//! kubectl apply -f deploy/crds/ -f deploy/admission/virtualmachineclaim-subject-authorization.yaml
//! KUBECONFIG=~/.kube/config \
//!   cargo test -p banlieue-controller --test live_claim_subject_policy -- --ignored --nocapture
//! ```
//!
//! Needs the banlieue CRDs and that policy installed, and an identity that
//! may impersonate users and groups (cluster-admin). The test sets the
//! policy's ConfigMap to its own values and restores the original on the
//! way out, including on failure: run it against a scratch cluster.

use std::collections::BTreeMap;

mod common;

use banlieue_api::banlieue::VirtualMachineClaim;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::{Api, Patch, PatchParams, PostParams};
use serde_json::json;

use common::{admin, as_master};

const NAMESPACE: &str = "banlieue-system";
const POLICY_CONFIGMAP: &str = "banlieue-claim-subject-policy";
const CLAIM_TTL_SECS: u64 = 600;

const DEX: &str = "https://dex.example.com";
const GOOGLE: &str = "https://accounts.google.com";
const AUTH0: &str = "https://tenant.us.auth0.com/";
const UNLISTED: &str = "https://unlisted.example.com";

/// Whether admission accepts a claim created by `user` naming
/// (`issuer`, `id`). Dry run: nothing is stored.
async fn admitted(user: &str, issuer: &str, id: &str) -> bool {
    let claim: VirtualMachineClaim = serde_json::from_value(json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "VirtualMachineClaim",
        "metadata": { "generateName": "policy-test-", "namespace": NAMESPACE },
        "spec": {
            "poolRef": { "name": "policy-test-pool" },
            "subject": { "issuer": issuer, "id": id },
            "ttlSeconds": CLAIM_TTL_SECS,
        },
    }))
    .expect("claim fixture");
    let api: Api<VirtualMachineClaim> = Api::namespaced(as_master(user).await, NAMESPACE);
    let params = PostParams {
        dry_run: true,
        ..PostParams::default()
    };
    match api.create(&params, &claim).await {
        Ok(_) => true,
        Err(kube::Error::Api(e)) if e.code == 422 || e.code == 403 => {
            eprintln!("    denied: {}", e.message);
            false
        }
        Err(e) => panic!("unexpected error creating a claim as {user}: {e}"),
    }
}

/// Run `body` with the policy ConfigMap's data set to `data`, restoring the
/// original afterwards whatever happens.
async fn with_policy<F, Fut>(data: BTreeMap<&str, &str>, body: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    use futures::FutureExt as _;
    let cms: Api<ConfigMap> = Api::namespaced(admin().await, NAMESPACE);
    let original = cms
        .get(POLICY_CONFIGMAP)
        .await
        .expect("the policy ConfigMap: apply deploy/admission/ first");
    set_policy_data(&cms, json!(data)).await;
    let result = std::panic::AssertUnwindSafe(body()).catch_unwind().await;
    // Keys the test added but the original lacked are removed (null).
    let mut restore = json!(original.data.clone().unwrap_or_default());
    for key in data.keys() {
        if restore.get(*key).is_none() {
            restore[*key] = serde_json::Value::Null;
        }
    }
    set_policy_data(&cms, restore).await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

async fn set_policy_data(cms: &Api<ConfigMap>, data: serde_json::Value) {
    cms.patch(
        POLICY_CONFIGMAP,
        &PatchParams::default(),
        &Patch::Merge(json!({ "data": data })),
    )
    .await
    .expect("patch the policy ConfigMap");
}

fn multi_issuer_policy() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        (
            "issuers",
            "https://dex.example.com\nhttps://accounts.google.com\nhttps://tenant.us.auth0.com/\n",
        ),
        ("usernamePrefix", "github:\n"),
        // The third line is malformed (three words) and must be ignored.
        (
            "issuerPrefixes",
            "https://dex.example.com github:\nhttps://accounts.google.com google:\nnot a pair\n",
        ),
        ("brokers", ""),
    ])
}

#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs and the claim subject policy"]
async fn each_issuer_is_checked_against_its_own_prefix() {
    with_policy(multi_issuer_policy(), || async {
        assert!(
            admitted("github:octocat", DEX, "octocat").await,
            "Dex user, Dex issuer"
        );
        assert!(
            admitted("google:alice@example.com", GOOGLE, "alice@example.com").await,
            "Google user, Google issuer"
        );
    })
    .await;
}

#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs and the claim subject policy"]
async fn a_user_cannot_claim_under_another_issuer() {
    with_policy(multi_issuer_policy(), || async {
        assert!(
            !admitted("google:alice@example.com", DEX, "alice@example.com").await,
            "a Google identity must not record a Dex subject"
        );
        assert!(
            !admitted("github:octocat", GOOGLE, "octocat").await,
            "a Dex identity must not record a Google subject"
        );
    })
    .await;
}

#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs and the claim subject policy"]
async fn an_issuer_without_an_entry_falls_back_to_the_cluster_prefix() {
    with_policy(multi_issuer_policy(), || async {
        assert!(admitted("github:octocat", AUTH0, "octocat").await);
        assert!(!admitted("auth0:bob@example.com", AUTH0, "bob@example.com").await);
    })
    .await;
}

#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs and the claim subject policy"]
async fn an_unlisted_issuer_is_still_refused() {
    with_policy(multi_issuer_policy(), || async {
        assert!(!admitted("github:octocat", UNLISTED, "octocat").await);
    })
    .await;
}

/// Without `issuerPrefixes` the policy behaves exactly as before the
/// amendment: one prefix for every issuer.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs and the claim subject policy"]
async fn without_issuer_prefixes_one_prefix_applies_to_every_issuer() {
    let mut policy = multi_issuer_policy();
    policy.insert("issuerPrefixes", "");
    with_policy(policy, || async {
        assert!(admitted("github:octocat", GOOGLE, "octocat").await);
        assert!(!admitted("google:alice@example.com", GOOGLE, "alice@example.com").await);
    })
    .await;
}

/// ADR-0132 Decision 8 and ADR-0135 Decision 7. A selector claim has no `spec.poolRef`, and a CEL
/// comparison of an absent field errors, which this fail-closed policy turns
/// into a denial of EVERY update, the controller's finalizer included. So:
/// an update that leaves the pool fields alone is admitted, and changing,
/// clearing or swapping either pool field is refused.
///
/// Persists one claim (no controller is needed, and none may run: it would
/// bind the claim) and deletes it on the way out.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs and the claim subject policy"]
async fn a_selector_claim_can_be_updated_but_not_retargeted() {
    use kube::ResourceExt as _;
    use kube::api::DeleteParams;

    with_policy(multi_issuer_policy(), || async {
        let claim: VirtualMachineClaim = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "VirtualMachineClaim",
            "metadata": { "generateName": "policy-selector-", "namespace": NAMESPACE },
            "spec": {
                "poolSelector": {},
                "subject": { "issuer": DEX, "id": "octocat" },
                "ttlSeconds": CLAIM_TTL_SECS,
            },
        }))
        .expect("claim fixture");
        let user: Api<VirtualMachineClaim> =
            Api::namespaced(as_master("github:octocat").await, NAMESPACE);
        let created = user
            .create(&PostParams::default(), &claim)
            .await
            .expect("a selector claim by its own subject is admitted");
        let name = created.name_any();

        let api: Api<VirtualMachineClaim> = Api::namespaced(admin().await, NAMESPACE);
        let patch = |body: serde_json::Value| {
            let api = api.clone();
            let name = name.clone();
            async move {
                api.patch(&name, &PatchParams::default(), &Patch::Merge(body))
                    .await
            }
        };
        // What the controller does first: touch metadata, not the spec.
        let touched =
            patch(json!({ "metadata": { "labels": { "policy-test": "touched" } } })).await;
        let narrowed =
            patch(json!({ "spec": { "poolSelector": { "matchLabels": { "tier": "gold" } } } }))
                .await;
        let swapped =
            patch(json!({ "spec": { "poolSelector": null, "poolRef": { "name": "p" } } })).await;
        // ADR-0135 Decision 7: setting a strategy after creation is an edit.
        let strategised = patch(json!({ "spec": { "selection": { "strategy": "Spread" } } })).await;

        api.delete(&name, &DeleteParams::default())
            .await
            .expect("clean up the test claim");

        assert!(touched.is_ok(), "a metadata update was denied: {touched:?}");
        assert!(narrowed.is_err(), "spec.poolSelector must be immutable");
        assert!(
            swapped.is_err(),
            "a selector claim must not become a named one"
        );
        assert!(
            strategised.is_err(),
            "spec.selection must be immutable (ADR-0135)"
        );
    })
    .await;
}
