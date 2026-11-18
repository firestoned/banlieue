// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The ADR-0133 admission policies against a **real API server**.
//!
//! Two policies, because a shared provider has two doors:
//!
//! * `machine-provider-ref.yaml`: only `banlieue-controller` sets
//!   `spec.providerRef.namespace` on a `VSphereMachine` (or a template). A
//!   machine naming a provider in the provider namespace is a placement that
//!   skipped scheduling, so a tenant must not be able to write one.
//! * `provider-namespace-selector-authorization.yaml`: setting
//!   `Provider.spec.namespaceSelector` needs `update` on the virtual
//!   subresource `providers/namespaceselector`, which only platform admins hold.
//!
//! The first cases write with `dryRun=All` as an impersonated identity in
//! `system:masters`, so the verdict is admission's alone. The selector policy
//! is an *authorization* check, so its callers are impersonated **without**
//! that group and given exactly the RBAC the case needs; the test creates and
//! deletes those bindings. Nothing but one fixture ClusterRole pair is stored.
//!
//! ```sh
//! kubectl apply --server-side -f deploy/crds/
//! kubectl create namespace banlieue-system
//! kubectl apply -f deploy/admission/machine-provider-ref.yaml
//! kubectl apply -f deploy/admission/provider-namespace-selector-authorization.yaml
//! cargo test -p banlieue-controller --test live_provider_ref_policy -- --ignored --nocapture
//! ```
//!
//! Needs an identity that may impersonate and manage RBAC (cluster-admin). Run
//! it against a scratch cluster.

mod common;

use banlieue_api::banlieue::Provider;
use banlieue_api::infrastructure::{VSphereMachine, VSphereMachineTemplate};
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding};
use kube::Client;
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use serde_json::{Value, json};

use common::{admin, as_master, impersonating, verdict};

const PLATFORM_NAMESPACE: &str = "banlieue-system";
const TENANT_NAMESPACE: &str = "banlieue-policy-test";
const CONTROLLER: &str = "system:serviceaccount:banlieue-system:banlieue-controller";
/// The shared vSphere provider's identity: the operator names each after its Provider.
const PROVIDER: &str = "system:serviceaccount:banlieue-system:banlieue-provider-vsphere-vcenter";
const TENANT: &str = "octocat";
const FIXTURE: &str = "provider-ref-policy-test";

/// A principal that may create and update Providers but holds no virtual grant.
const DELEGATE: &str = "policy-test-delegate";
/// A principal that also holds `providers/namespaceselector` `update`.
const PLATFORM_ADMIN: &str = "policy-test-platform-admin";
const DELEGATE_ROLE: &str = "policy-test-provider-writer";

fn machine_spec(provider_ref: &Value) -> Value {
    json!({
        "providerRef": provider_ref,
        "template": "tmpl",
        "datacenter": "dc",
        "cluster": "cl",
        "datastore": "ds",
        "numCpus": 1,
        "memoryMiB": 1024,
        "firmware": "efi",
        "disks": [],
        "network": []
    })
}

fn machine(name: &str, provider_ref: &Value) -> VSphereMachine {
    serde_json::from_value(json!({
        "apiVersion": "infrastructure.banlieue.io/v1alpha1",
        "kind": "VSphereMachine",
        "metadata": { "name": name, "namespace": TENANT_NAMESPACE },
        "spec": machine_spec(provider_ref)
    }))
    .expect("machine fixture")
}

fn template(name: &str, provider_ref: &Value) -> VSphereMachineTemplate {
    serde_json::from_value(json!({
        "apiVersion": "infrastructure.banlieue.io/v1alpha1",
        "kind": "VSphereMachineTemplate",
        "metadata": { "name": name, "namespace": TENANT_NAMESPACE },
        "spec": { "template": { "spec": machine_spec(provider_ref) } }
    }))
    .expect("template fixture")
}

fn foreign() -> Value {
    json!({ "name": "vcenter", "namespace": PLATFORM_NAMESPACE })
}

fn local() -> Value {
    json!({ "name": "vcenter" })
}

async fn may_create_machine(user: &str, provider_ref: Value) -> bool {
    let api: Api<VSphereMachine> = Api::namespaced(as_master(user).await, TENANT_NAMESPACE);
    let params = PostParams {
        dry_run: true,
        ..PostParams::default()
    };
    verdict(
        api.create(&params, &machine("provider-ref-create", &provider_ref))
            .await,
        &format!("{user} creates a machine with providerRef {provider_ref}"),
    )
}

async fn may_create_template(user: &str, provider_ref: Value) -> bool {
    let api: Api<VSphereMachineTemplate> = Api::namespaced(as_master(user).await, TENANT_NAMESPACE);
    let params = PostParams {
        dry_run: true,
        ..PostParams::default()
    };
    verdict(
        api.create(&params, &template("provider-ref-template", &provider_ref))
            .await,
        &format!("{user} creates a template with providerRef {provider_ref}"),
    )
}

async fn may_patch_machine(user: &str, patch: Value) -> bool {
    let api: Api<VSphereMachine> = Api::namespaced(as_master(user).await, TENANT_NAMESPACE);
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
#[ignore = "needs a real API server with the CRDs and the provider-ref policy"]
async fn only_the_controller_writes_the_provider_ref_namespace() {
    let ns: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(admin().await);
    let _ = ns
        .create(
            &PostParams::default(),
            &serde_json::from_value(json!({
                "apiVersion": "v1", "kind": "Namespace",
                "metadata": { "name": TENANT_NAMESPACE }
            }))
            .unwrap(),
        )
        .await;
    let machines: Api<VSphereMachine> = Api::namespaced(admin().await, TENANT_NAMESPACE);
    let _ = machines.delete(FIXTURE, &DeleteParams::default()).await;

    // The fixture is a machine already placed on the shared provider, as only the controller can make one.
    Api::<VSphereMachine>::namespaced(as_master(CONTROLLER).await, TENANT_NAMESPACE)
        .create(&PostParams::default(), &machine(FIXTURE, &foreign()))
        .await
        .expect("the controller creates a machine naming the shared provider: is the policy matching it?");

    // Creation: the attack is a tenant naming the platform provider.
    assert!(
        may_create_machine(CONTROLLER, foreign()).await,
        "the controller places a VM on the shared provider"
    );
    assert!(
        !may_create_machine(TENANT, foreign()).await,
        "a tenant cannot name the shared provider directly"
    );
    assert!(
        !may_create_machine(PROVIDER, foreign()).await,
        "nor can the provider"
    );
    assert!(
        may_create_machine(TENANT, local()).await,
        "a bare providerRef is not this policy's: it resolves in the tenant's own namespace"
    );

    // The same door through a CAPI machine template.
    assert!(may_create_template(CONTROLLER, foreign()).await);
    assert!(
        !may_create_template(TENANT, foreign()).await,
        "a template would carry the forged reference into every machine"
    );
    assert!(may_create_template(TENANT, local()).await);

    // Update: re-pointing an existing machine, adding, or removing the namespace.
    let repoint = json!({ "spec": { "providerRef": { "namespace": "elsewhere" } } });
    assert!(
        !may_patch_machine(TENANT, repoint.clone()).await,
        "a tenant cannot re-point a machine"
    );
    assert!(
        !may_patch_machine(PROVIDER, repoint.clone()).await,
        "nor can the provider"
    );
    assert!(
        may_patch_machine(CONTROLLER, repoint).await,
        "the controller can"
    );
    assert!(
        !may_patch_machine(
            TENANT,
            json!({ "spec": { "providerRef": { "namespace": null } } })
        )
        .await,
        "a tenant cannot strip the namespace either"
    );

    // What the provider does write is untouched: its finalizer and (separately) status.
    assert!(
        may_patch_machine(
            PROVIDER,
            json!({ "metadata": { "finalizers": ["banlieue.io/test"] } })
        )
        .await,
        "the provider still manages its finalizer"
    );
    assert!(
        may_patch_machine(TENANT, json!({ "spec": { "numCpus": 2 } })).await,
        "an unrelated spec edit is not this policy's"
    );

    machines
        .delete(FIXTURE, &DeleteParams::default())
        .await
        .expect("delete the fixture");
}

fn provider(name: &str, selector: Option<Value>) -> Provider {
    let mut spec = json!({
        "providerClassRef": { "name": "vsphere" },
        "connection": { "endpoint": "https://vcenter.example.com/sdk" }
    });
    if let Some(selector) = selector {
        spec["namespaceSelector"] = selector;
    }
    serde_json::from_value(json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "Provider",
        "metadata": { "name": name, "namespace": TENANT_NAMESPACE },
        "spec": spec
    }))
    .expect("provider fixture")
}

/// Grant `user` create/update on Providers and, for the admin, the virtual subresource.
async fn grant(admin: &Client, with_selector_grant: bool) {
    let roles: Api<ClusterRole> = Api::all(admin.clone());
    let bindings: Api<ClusterRoleBinding> = Api::all(admin.clone());
    let mut rules = vec![json!({
        "apiGroups": ["banlieue.io"], "resources": ["providers"],
        "verbs": ["create", "update", "patch", "get"]
    })];
    if with_selector_grant {
        rules.push(json!({
            "apiGroups": ["banlieue.io"], "resources": ["providers/namespaceselector"],
            "verbs": ["update"]
        }));
    }
    let (role, user) = if with_selector_grant {
        (format!("{DELEGATE_ROLE}-admin"), PLATFORM_ADMIN)
    } else {
        (DELEGATE_ROLE.to_string(), DELEGATE)
    };
    let _ = roles
        .create(
            &PostParams::default(),
            &serde_json::from_value(json!({
                "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole",
                "metadata": { "name": role }, "rules": rules
            }))
            .unwrap(),
        )
        .await;
    let _ = bindings
        .create(
            &PostParams::default(),
            &serde_json::from_value(json!({
                "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRoleBinding",
                "metadata": { "name": role },
                "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": role },
                "subjects": [{ "apiGroup": "rbac.authorization.k8s.io", "kind": "User", "name": user }]
            }))
            .unwrap(),
        )
        .await;
}

async fn revoke(admin: &Client) {
    for role in [DELEGATE_ROLE.to_string(), format!("{DELEGATE_ROLE}-admin")] {
        let _ = Api::<ClusterRoleBinding>::all(admin.clone())
            .delete(&role, &DeleteParams::default())
            .await;
        let _ = Api::<ClusterRole>::all(admin.clone())
            .delete(&role, &DeleteParams::default())
            .await;
    }
}

async fn may_create_provider(user: &str, selector: Option<Value>) -> bool {
    // No `system:masters`: this policy asks the authorizer about the caller.
    let api: Api<Provider> = Api::namespaced(
        impersonating(user, vec!["system:authenticated".to_string()]).await,
        TENANT_NAMESPACE,
    );
    let params = PostParams {
        dry_run: true,
        ..PostParams::default()
    };
    verdict(
        api.create(&params, &provider("selector-policy-test", selector.clone()))
            .await,
        &format!("{user} creates a Provider with selector {selector:?}"),
    )
}

#[tokio::test]
#[ignore = "needs a real API server with the CRDs and the namespace-selector policy"]
async fn only_platform_admins_set_a_provider_namespace_selector() {
    let admin = admin().await;
    revoke(&admin).await;
    grant(&admin, false).await;
    grant(&admin, true).await;

    let selector = Some(json!({ "matchLabels": { "banlieue.io/tenant": "true" } }));

    assert!(
        may_create_provider(DELEGATE, None).await,
        "a delegated role may still create a Provider that serves only its own namespace"
    );
    assert!(
        !may_create_provider(DELEGATE, selector.clone()).await,
        "but not one that chooses which namespaces it serves"
    );
    assert!(
        may_create_provider(PLATFORM_ADMIN, selector).await,
        "a platform admin, holding providers/namespaceselector update, may"
    );

    revoke(&admin).await;
}
