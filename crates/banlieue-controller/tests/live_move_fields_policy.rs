// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The move-fields policy against a **real API server** (ADR-0127 Decision 4).
//!
//! Only `banlieue-controller` may set or change a `CloudHypervisorMachine`'s `spec.identity`,
//! `spec.moveIn` and `spec.moveOut`. A host provider's Role allows `patch` on every machine in
//! its namespace, so without the policy a stolen host token could point another host's guest at
//! a peer it controls. Each case writes with `dryRun=All` as an impersonated identity, in
//! `system:masters` so the verdict is admission's alone; nothing but the one fixture machine is
//! stored.
//!
//! ```sh
//! kubectl apply --server-side -f deploy/crds/
//! kubectl create namespace banlieue-system
//! kubectl apply -f deploy/admission/cloudhypervisormachine-move-fields.yaml
//! cargo test -p banlieue-controller --test live_move_fields_policy -- --ignored --nocapture
//! ```
//!
//! Needs an identity that may impersonate (cluster-admin). Run it against a scratch cluster.

use banlieue_api::infrastructure::CloudHypervisorMachine;
use kube::Client;
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use serde_json::{Value, json};

const NAMESPACE: &str = "banlieue-system";
const PRIVILEGED_GROUP: &str = "system:masters";
const CONTROLLER: &str = "system:serviceaccount:banlieue-system:banlieue-controller";
/// A host provider's identity: the operator names each after its Provider.
const HOST_PROVIDER: &str = "system:serviceaccount:banlieue-system:banlieue-provider-hv01";
const SOMEONE: &str = "octocat";
const FIXTURE: &str = "move-fields-policy-test";

async fn client(user: Option<&str>) -> Client {
    let mut config = kube::Config::infer()
        .await
        .expect("kubeconfig: set KUBECONFIG");
    if let Some(user) = user {
        config.auth_info.impersonate = Some(user.to_string());
        config.auth_info.impersonate_groups = Some(vec![PRIVILEGED_GROUP.to_string()]);
    }
    Client::try_from(config).expect("client")
}

fn machine(name: &str, extra: &Value) -> CloudHypervisorMachine {
    let mut m = json!({
        "apiVersion": "infrastructure.banlieue.io/v1alpha1",
        "kind": "CloudHypervisorMachine",
        "metadata": { "name": name, "namespace": NAMESPACE },
        "spec": {
            "providerRef": { "name": "hv01" },
            "cpus": { "boot": 1 },
            "memory": { "sizeMiB": 512 },
            "storageClass": "default",
            "bootSource": { "kind": "image", "image": "img" },
            "osDiskSizeGiB": 10,
            "nics": []
        }
    });
    for (k, v) in extra.as_object().into_iter().flatten() {
        m["spec"][k] = v.clone();
    }
    serde_json::from_value(m).expect("machine fixture")
}

fn verdict(r: Result<CloudHypervisorMachine, kube::Error>, what: &str) -> bool {
    match r {
        Ok(_) => true,
        Err(kube::Error::Api(e)) if e.code == 422 || e.code == 403 => {
            eprintln!("    {what}: denied: {}", e.message);
            false
        }
        Err(e) => panic!("{what}: unexpected error: {e}"),
    }
}

/// Whether `user` may create a machine carrying `extra` in its spec. Dry run.
async fn may_create(user: &str, extra: Value) -> bool {
    let api: Api<CloudHypervisorMachine> = Api::namespaced(client(Some(user)).await, NAMESPACE);
    let params = PostParams {
        dry_run: true,
        ..PostParams::default()
    };
    verdict(
        api.create(&params, &machine("move-fields-create", &extra))
            .await,
        &format!("{user} creates with {extra}"),
    )
}

/// Whether `user` may merge-patch the fixture machine with `patch`. Dry run.
async fn may_patch(user: &str, patch: Value) -> bool {
    let api: Api<CloudHypervisorMachine> = Api::namespaced(client(Some(user)).await, NAMESPACE);
    let params = PatchParams {
        dry_run: true,
        ..PatchParams::default()
    };
    verdict(
        api.patch(FIXTURE, &params, &Patch::Merge(&patch)).await,
        &format!("{user} patches {patch}"),
    )
}

#[tokio::test]
#[ignore = "needs a real API server with the CRDs and the move-fields policy"]
async fn only_the_controller_writes_the_move_fields() {
    let admin: Api<CloudHypervisorMachine> = Api::namespaced(client(None).await, NAMESPACE);
    let _ = admin.delete(FIXTURE, &DeleteParams::default()).await;
    // The fixture is a machine already mid-move, as only the controller can make one.
    let controller: Api<CloudHypervisorMachine> =
        Api::namespaced(client(Some(CONTROLLER)).await, NAMESPACE);
    controller
        .create(
            &PostParams::default(),
            &machine(
                FIXTURE,
                &json!({
                    "identity": { "name": "web-0", "uid": "u-1" },
                    "moveOut": { "moveId": "m1" }
                }),
            ),
        )
        .await
        .expect("the controller creates a machine with move fields: is the policy matching it?");

    let moved_in =
        json!({ "identity": { "name": "web-0", "uid": "u-1" }, "moveIn": { "moveId": "m1" } });
    assert!(
        may_create(CONTROLLER, moved_in.clone()).await,
        "the controller creates a destination"
    );
    assert!(!may_create(SOMEONE, moved_in).await, "nobody else does");
    assert!(
        !may_create(HOST_PROVIDER, json!({ "moveOut": { "moveId": "m9" } })).await,
        "nor a host provider"
    );
    assert!(
        may_create(SOMEONE, json!({})).await,
        "a machine without move fields is not this policy's"
    );

    // The attack: a host token re-pointing another host's outgoing guest.
    let repoint = json!({ "spec": { "moveOut": {
        "peer": { "certificate": "-----BEGIN CERTIFICATE-----", "endpoint": "203.0.113.9:7443" } } } });
    assert!(
        !may_patch(HOST_PROVIDER, repoint.clone()).await,
        "a host provider cannot set a peer"
    );
    assert!(
        !may_patch(SOMEONE, repoint.clone()).await,
        "nor anyone else"
    );
    assert!(
        may_patch(CONTROLLER, repoint).await,
        "the controller copies the peer across"
    );
    for (what, patch) in [
        (
            "commit",
            json!({ "spec": { "moveOut": { "cutover": true } } }),
        ),
        ("abort", json!({ "spec": { "moveOut": { "abort": true } } })),
        ("remove moveOut", json!({ "spec": { "moveOut": null } })),
        (
            "add moveIn",
            json!({ "spec": { "moveIn": { "moveId": "m2" } } }),
        ),
    ] {
        assert!(
            !may_patch(HOST_PROVIDER, patch.clone()).await,
            "a host provider cannot {what}"
        );
        assert!(
            may_patch(CONTROLLER, patch).await,
            "the controller can {what}"
        );
    }

    // What a host provider does write is untouched: metadata on its machines.
    assert!(
        may_patch(
            HOST_PROVIDER,
            json!({ "metadata": { "finalizers": ["banlieue.io/test"] } })
        )
        .await,
        "a host provider still manages its finalizers"
    );

    admin
        .delete(FIXTURE, &DeleteParams::default())
        .await
        .expect("delete the fixture");
}
