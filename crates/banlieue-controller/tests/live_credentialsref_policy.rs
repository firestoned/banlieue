// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The credentialsRef authorization policy (CHAIN-001, ADR-0157) against a **real API server**.
//!
//! A Provider's connection decides which Secret its workload reads and where it presents it. The
//! policy makes whoever sets or changes that connection prove they may `get` the Secret, or
//! `reference` it (a custom verb that reads nothing, ADR-0157 Decision 2). A write that leaves
//! the connection alone (a label, a capability) is not checked (Decision 1).
//!
//! Every caller is impersonated **without** `system:masters`, holding exactly the RBAC its case
//! needs: write Providers in a scratch namespace, plus `get` on the Secret, or `reference` on it,
//! or neither. Writes are `dryRun=All`; one fixture Provider and the RBAC are stored and removed.
//!
//! ```sh
//! kubectl apply --server-side -f deploy/crds/
//! kubectl apply -f deploy/admission/provider-credentialsref-authorization.yaml
//! cargo test -p banlieue-controller --test live_credentialsref_policy -- --ignored --nocapture
//! ```
//!
//! Needs an identity that may impersonate and manage RBAC (cluster-admin). Run it against a
//! scratch cluster.

mod common;

use banlieue_api::banlieue::Provider;
use k8s_openapi::api::core::v1::Namespace;
use k8s_openapi::api::rbac::v1::{ClusterRole, RoleBinding};
use kube::Client;
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use serde_json::{Value, json};

use common::{admin, impersonating};

const NAMESPACE: &str = "banlieue-credentialsref-test";
/// The Secret every case names, and one nobody here is granted anything on.
const SECRET: &str = "vcenter-creds";
const OTHER_SECRET: &str = "other-creds";
const FIXTURE: &str = "credentialsref-fixture";

/// May `get` the Secret: the case the policy was written for.
const READER: &str = "credentialsref-test-reader";
/// May `reference` the Secret, not read it: a controller that names a credential it never holds.
const REFERRER: &str = "credentialsref-test-referrer";
/// May write Providers, nothing on the Secret.
const WRITER: &str = "credentialsref-test-writer";

const REFUSAL: &str = "neither get nor reference";

fn provider(name: &str, credentials: Option<&str>) -> Provider {
    let mut connection = json!({
        "endpoint": "https://vcenter.example.com/sdk",
        "caBundle": { "configMapRef": { "name": "vcenter-ca" } }
    });
    if let Some(c) = credentials {
        connection["credentialsRef"] = json!({ "name": c });
    }
    serde_json::from_value(json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "Provider",
        "metadata": { "name": name, "namespace": NAMESPACE },
        "spec": { "providerClassRef": { "name": "vsphere" }, "connection": connection }
    }))
    .expect("provider fixture")
}

fn role_name(user: &str) -> String {
    format!("{user}-role")
}

/// A ClusterRole bound in the scratch namespace only: write Providers, and `secret_verbs` on
/// exactly [`SECRET`].
async fn grant(admin: &Client, user: &str, secret_verbs: &[&str]) {
    let mut rules = vec![json!({
        "apiGroups": ["banlieue.io"], "resources": ["providers"],
        "verbs": ["create", "update", "patch", "get"]
    })];
    if !secret_verbs.is_empty() {
        rules.push(json!({
            "apiGroups": [""], "resources": ["secrets"],
            "resourceNames": [SECRET], "verbs": secret_verbs
        }));
    }
    let role = role_name(user);
    Api::<ClusterRole>::all(admin.clone())
        .create(
            &PostParams::default(),
            &serde_json::from_value(json!({
                "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole",
                "metadata": { "name": role }, "rules": rules
            }))
            .unwrap(),
        )
        .await
        .expect("create the case's ClusterRole");
    Api::<RoleBinding>::namespaced(admin.clone(), NAMESPACE)
        .create(
            &PostParams::default(),
            &serde_json::from_value(json!({
                "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
                "metadata": { "name": role, "namespace": NAMESPACE },
                "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": role },
                "subjects": [{ "apiGroup": "rbac.authorization.k8s.io", "kind": "User", "name": user }]
            }))
            .unwrap(),
        )
        .await
        .expect("bind the case's ClusterRole");
}

async fn clean(admin: &Client) {
    for user in [READER, REFERRER, WRITER] {
        let _ = Api::<RoleBinding>::namespaced(admin.clone(), NAMESPACE)
            .delete(&role_name(user), &DeleteParams::default())
            .await;
        let _ = Api::<ClusterRole>::all(admin.clone())
            .delete(&role_name(user), &DeleteParams::default())
            .await;
    }
    let _ = Api::<Provider>::namespaced(admin.clone(), NAMESPACE)
        .delete(FIXTURE, &DeleteParams::default())
        .await;
}

async fn as_user(user: &str) -> Api<Provider> {
    // No `system:masters`: the policy asks the authorizer about the caller.
    Api::namespaced(
        impersonating(user, vec!["system:authenticated".to_string()]).await,
        NAMESPACE,
    )
}

/// `Ok` when admitted, the API server's message when refused (403 or 422). Anything else panics,
/// so an unreachable cluster never reads as a refusal.
fn outcome<T>(r: Result<T, kube::Error>, what: &str) -> Result<(), String> {
    match r {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(e)) if e.code == 403 || e.code == 422 => {
            eprintln!("    {what}: refused: {}", e.message);
            Err(e.message)
        }
        Err(e) => panic!("{what}: unexpected error: {e}"),
    }
}

async fn create(user: &str, credentials: Option<&str>) -> Result<(), String> {
    let params = PostParams {
        dry_run: true,
        ..PostParams::default()
    };
    outcome(
        as_user(user)
            .await
            .create(&params, &provider("credentialsref-create", credentials))
            .await,
        &format!("{user} creates a Provider naming {credentials:?}"),
    )
}

async fn patch(user: &str, patch: &Value) -> Result<(), String> {
    let params = PatchParams {
        dry_run: true,
        ..PatchParams::default()
    };
    outcome(
        as_user(user)
            .await
            .patch(FIXTURE, &params, &Patch::Merge(patch))
            .await,
        &format!("{user} patches {patch}"),
    )
}

fn refused(r: Result<(), String>, what: &str) {
    match r {
        Ok(()) => panic!("{what}: admitted"),
        Err(m) => assert!(
            m.contains(REFUSAL),
            "{what}: refused, but not by this policy: {m}"
        ),
    }
}

fn admitted(r: Result<(), String>, what: &str) {
    if let Err(m) = r {
        panic!("{what}: refused: {m}");
    }
}

#[tokio::test]
#[ignore = "needs a real API server with the CRDs and the credentialsRef policy"]
async fn naming_a_credential_needs_get_or_reference_and_only_when_the_connection_changes() {
    let admin = admin().await;
    let _ = Api::<Namespace>::all(admin.clone())
        .create(
            &PostParams::default(),
            &serde_json::from_value(json!({
                "apiVersion": "v1", "kind": "Namespace", "metadata": { "name": NAMESPACE }
            }))
            .unwrap(),
        )
        .await;
    clean(&admin).await;
    grant(&admin, READER, &["get"]).await;
    grant(&admin, REFERRER, &["reference"]).await;
    grant(&admin, WRITER, &[]).await;
    // The fixture an administrator made, already naming the credential.
    Api::<Provider>::namespaced(admin.clone(), NAMESPACE)
        .create(&PostParams::default(), &provider(FIXTURE, Some(SECRET)))
        .await
        .expect("an administrator creates the fixture Provider");

    // ----- create: every connection is checked ------------------------------------------
    admitted(create(READER, Some(SECRET)).await, "a reader names it");
    admitted(
        create(REFERRER, Some(SECRET)).await,
        "a referrer names it without reading it (Decision 2)",
    );
    refused(
        create(WRITER, Some(SECRET)).await,
        "a writer with neither names it",
    );
    refused(
        create(REFERRER, Some(OTHER_SECRET)).await,
        "a referrer names a Secret it was not granted",
    );
    admitted(
        create(WRITER, None).await,
        "a Provider naming no credential is not this policy's",
    );

    // ----- update leaving the connection alone: not checked (Decision 1) -----------------
    admitted(
        patch(
            WRITER,
            &json!({ "metadata": { "labels": { "topology.example.com/zone": "az1" } } }),
        )
        .await,
        "a label by a writer with neither",
    );
    admitted(
        patch(
            WRITER,
            &json!({ "spec": { "capabilities": { "features": ["nestedVirtualization"] } } }),
        )
        .await,
        "a capability by a writer with neither",
    );

    // ----- update changing the connection: checked, whatever field moves ---------------
    for (what, change) in [
        (
            "re-points the endpoint",
            json!({ "spec": { "connection": { "endpoint": "https://attacker.example.com/sdk" } } }),
        ),
        (
            "swaps the CA",
            json!({ "spec": { "connection": { "caBundle": { "configMapRef": { "name": "other-ca" } } } } }),
        ),
        (
            "turns off verification",
            json!({ "spec": { "connection": { "insecureSkipTLSVerify": true } } }),
        ),
    ] {
        refused(
            patch(WRITER, &change).await,
            &format!("a writer with neither {what}, credential unchanged"),
        );
        admitted(
            patch(REFERRER, &change).await,
            &format!("a referrer {what}"),
        );
        admitted(patch(READER, &change).await, &format!("a reader {what}"));
    }
    let repoint =
        json!({ "spec": { "connection": { "credentialsRef": { "name": OTHER_SECRET } } } });
    refused(
        patch(REFERRER, &repoint).await,
        "a referrer re-points it at a Secret it was not granted",
    );
    refused(
        patch(WRITER, &repoint).await,
        "a writer with neither re-points it",
    );

    clean(&admin).await;
}
