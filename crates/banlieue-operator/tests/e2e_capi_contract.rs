// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The Cluster API v1beta2 contract, against **real CAPI controllers**
//! (ADR-0096 Decision 5, in the style of ADR-0014).
//!
//! `make kind-e2e-capi` builds a fresh kind cluster, installs CAPI core with
//! `clusterctl init` (pinned release, cert-manager included), and installs
//! banlieue from a local clusterctl repository rendered from this tree with
//! `banlieue bootstrap operator --dry-run`. This suite then plays the part of
//! a vSphere provider, which is deliberately not running:
//!
//! 1. it creates a `Cluster` + `VSphereCluster` and a `Machine` +
//!    `VSphereMachine`;
//! 2. it sets the infrastructure fields the way a provider would
//!    (`status.initialization.provisioned`, `spec.providerID`,
//!    `status.addresses`);
//! 3. it asserts CAPI's controllers reflect them, and that deleting the
//!    `Machine` (and then the `Cluster`) deletes the infrastructure object.
//!
//! Every step CAPI takes on a banlieue object (owner references, labels,
//! deletion) is authorised only by the aggregate ClusterRole
//! `banlieue-capi-infrastructure`, so this also proves that role is enough.
//!
//! banlieue's own pods are not expected to run: the components are rendered
//! with an image tag that is not loaded into the cluster. Nothing here waits
//! on them, and nothing should: the contract under test is between CAPI and
//! the CRDs plus RBAC banlieue installs.
//!
//! Addresses are RFC 5737 documentation addresses. `#[ignore]`d so
//! `cargo test` stays hermetic; an unreachable cluster fails loudly.

mod e2e_common;

use banlieue_api::infrastructure::{VSphereCluster, VSphereMachine};
use k8s_openapi::api::core::v1::Namespace;
use k8s_openapi::api::rbac::v1::ClusterRole;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{
    ApiResource, DeleteParams, DynamicObject, GroupVersionKind, ListParams, Patch, PatchParams,
    PostParams,
};
use kube::{Api, Client};
use serde_json::{Value, json};

use e2e_common::{client, wait_for, wait_until_gone};

/// Namespace the suite works in, recreated on every run.
const NAMESPACE: &str = "banlieue-capi-e2e";

/// Shared name for the Cluster and its VSphereCluster.
const CLUSTER: &str = "capi-e2e";

/// Shared name for the Machine and its VSphereMachine.
const MACHINE: &str = "capi-e2e-md-0";

/// What the "provider" writes into `spec.providerID`.
const PROVIDER_ID: &str = "vsphere://4211a6f2-0000-4000-8000-000000000001";

/// RFC 5737 documentation addresses.
const INTERNAL_IP: &str = "192.0.2.21";
const EXTERNAL_IP: &str = "198.51.100.21";
const CONTROL_PLANE_HOST: &str = "192.0.2.10";
const CONTROL_PLANE_PORT: u16 = 6443;

const CAPI_GROUP: &str = "cluster.x-k8s.io";
const CAPI_VERSION: &str = "v1beta2";
const INFRA_GROUP: &str = "infrastructure.banlieue.io";
const INFRA_API_VERSION: &str = "infrastructure.banlieue.io/v1alpha1";
const AGGREGATE_LABEL: &str = "cluster.x-k8s.io/aggregate-to-manager";
const CAPI_AGGREGATE_ROLE: &str = "banlieue-capi-infrastructure";
const CLUSTER_NAME_LABEL: &str = "cluster.x-k8s.io/cluster-name";

fn capi_api(client: &Client, kind: &str, plural: &str) -> Api<DynamicObject> {
    let gvk = GroupVersionKind::gvk(CAPI_GROUP, CAPI_VERSION, kind);
    let resource = ApiResource::from_gvk_with_plural(&gvk, plural);
    Api::namespaced_with(client.clone(), NAMESPACE, &resource)
}

fn dynamic(kind: &str, body: Value) -> DynamicObject {
    let mut value = body;
    value["apiVersion"] = json!(format!("{CAPI_GROUP}/{CAPI_VERSION}"));
    value["kind"] = json!(kind);
    serde_json::from_value(value).expect("well-formed CAPI object")
}

/// A JSON pointer into a dynamic object's body.
fn field<'a>(obj: &'a DynamicObject, pointer: &str) -> Option<&'a Value> {
    obj.data.pointer(pointer)
}

/// Wait until the e2e Machine satisfies `check`.
async fn wait_for_machine(
    machines: &Api<DynamicObject>,
    what: &str,
    check: fn(&DynamicObject) -> bool,
) -> DynamicObject {
    wait_for(what, || {
        let api = machines.clone();
        async move {
            let machine = api.get(MACHINE).await.ok()?;
            check(&machine).then_some(machine)
        }
    })
    .await
}

/// Delete the namespace from a previous run and create it afresh.
async fn fresh_namespace(client: &Client) {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let _ = namespaces.delete(NAMESPACE, &DeleteParams::default()).await;
    wait_until_gone("the previous run's namespace to be deleted", || {
        let api = namespaces.clone();
        async move { matches!(api.get_opt(NAMESPACE).await, Ok(None)) }
    })
    .await;
    let ns = Namespace {
        metadata: ObjectMeta {
            name: Some(NAMESPACE.to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    namespaces
        .create(&PostParams::default(), &ns)
        .await
        .expect("creating the e2e namespace");
}

/// The aggregation actually happened: CAPI's aggregated manager role now
/// holds our infrastructure rules, and holds nothing in `banlieue.io`.
#[tokio::test]
#[ignore = "requires a clusterctl-initialised cluster; run `make kind-e2e-capi`"]
async fn capi_manager_aggregates_only_the_infrastructure_role() {
    let client = client().await;
    let cluster_roles: Api<ClusterRole> = Api::all(client.clone());

    let ours = cluster_roles
        .get_opt(CAPI_AGGREGATE_ROLE)
        .await
        .expect("reading ClusterRoles")
        .expect("clusterctl init must install banlieue-capi-infrastructure");
    assert_eq!(
        ours.metadata
            .labels
            .as_ref()
            .and_then(|l| l.get(AGGREGATE_LABEL))
            .map(String::as_str),
        Some("true")
    );

    let controller = cluster_roles
        .get_opt("banlieue-controller")
        .await
        .expect("reading ClusterRoles")
        .expect("clusterctl init must install banlieue-controller");
    assert!(
        !controller
            .metadata
            .labels
            .unwrap_or_default()
            .contains_key(AGGREGATE_LABEL),
        "banlieue-controller must not be aggregated into CAPI's manager"
    );

    // The role whose aggregationRule selects the label: CAPI core's.
    let aggregated = wait_for(
        "CAPI's aggregated manager role to include the infrastructure rules",
        || {
            let api = cluster_roles.clone();
            async move {
                let roles = api.list(&ListParams::default()).await.ok()?;
                roles.items.into_iter().find(|role| {
                    let selects_label = role.aggregation_rule.as_ref().is_some_and(|rule| {
                        rule.cluster_role_selectors.iter().flatten().any(|s| {
                            s.match_labels
                                .as_ref()
                                .and_then(|l| l.get(AGGREGATE_LABEL))
                                .is_some_and(|v| v == "true")
                        })
                    });
                    let has_infra = role
                        .rules
                        .iter()
                        .flatten()
                        .any(|r| r.api_groups.iter().flatten().any(|g| g == INFRA_GROUP));
                    selects_label && has_infra
                })
            }
        },
    )
    .await;

    for rule in aggregated.rules.iter().flatten() {
        assert!(
            !rule.api_groups.iter().flatten().any(|g| g == "banlieue.io"),
            "CAPI's manager must hold nothing in banlieue.io, got {rule:?}"
        );
    }
}

/// Cluster + VSphereCluster, Machine + VSphereMachine: CAPI reflects what the
/// "provider" reports, and deleting the CAPI object deletes ours.
#[tokio::test]
#[ignore = "requires a clusterctl-initialised cluster; run `make kind-e2e-capi`"]
async fn capi_drives_banlieue_infrastructure_objects_through_the_v1beta2_contract() {
    let client = client().await;
    fresh_namespace(&client).await;

    let clusters = capi_api(&client, "Cluster", "clusters");
    let machines = capi_api(&client, "Machine", "machines");
    let vsphere_clusters: Api<VSphereCluster> = Api::namespaced(client.clone(), NAMESPACE);
    let vsphere_machines: Api<VSphereMachine> = Api::namespaced(client.clone(), NAMESPACE);
    let pp = PostParams::default();

    // ── InfraCluster ────────────────────────────────────────────────────────
    let vsphere_cluster: VSphereCluster = serde_json::from_value(json!({
        "apiVersion": INFRA_API_VERSION,
        "kind": "VSphereCluster",
        "metadata": { "name": CLUSTER, "namespace": NAMESPACE },
        "spec": {
            "controlPlaneEndpoint": { "host": CONTROL_PLANE_HOST, "port": CONTROL_PLANE_PORT },
        },
    }))
    .expect("well-formed VSphereCluster");
    vsphere_clusters
        .create(&pp, &vsphere_cluster)
        .await
        .expect("creating the VSphereCluster");

    clusters
        .create(
            &pp,
            &dynamic(
                "Cluster",
                json!({
                    "metadata": { "name": CLUSTER, "namespace": NAMESPACE },
                    "spec": {
                        "infrastructureRef": {
                            "apiGroup": INFRA_GROUP,
                            "kind": "VSphereCluster",
                            "name": CLUSTER,
                        },
                    },
                }),
            ),
        )
        .await
        .expect("creating the CAPI Cluster");

    // CAPI adopts the InfraCluster: an owner reference to the Cluster (a plain
    // one: CAPI v1beta2 does not mark the Cluster as the controller here).
    wait_for("CAPI to take ownership of the VSphereCluster", || {
        let api = vsphere_clusters.clone();
        async move {
            let vc = api.get(CLUSTER).await.ok()?;
            vc.metadata
                .owner_references
                .unwrap_or_default()
                .into_iter()
                .find(|o| o.kind == "Cluster" && o.name == CLUSTER)
        }
    })
    .await;

    // The provider reports the InfraCluster provisioned.
    vsphere_clusters
        .patch_status(
            CLUSTER,
            &PatchParams::default(),
            &Patch::Merge(json!({ "status": { "initialization": { "provisioned": true } } })),
        )
        .await
        .expect("patching VSphereCluster status");

    let cluster = wait_for(
        "Cluster status.initialization.infrastructureProvisioned=true",
        || {
            let api = clusters.clone();
            async move {
                let c = api.get(CLUSTER).await.ok()?;
                (field(&c, "/status/initialization/infrastructureProvisioned")
                    == Some(&json!(true)))
                .then_some(c)
            }
        },
    )
    .await;
    assert_eq!(
        field(&cluster, "/spec/controlPlaneEndpoint/host"),
        Some(&json!(CONTROL_PLANE_HOST)),
        "CAPI copies the InfraCluster's controlPlaneEndpoint"
    );

    // ── InfraMachine ────────────────────────────────────────────────────────
    let vsphere_machine: VSphereMachine = serde_json::from_value(json!({
        "apiVersion": INFRA_API_VERSION,
        "kind": "VSphereMachine",
        "metadata": { "name": MACHINE, "namespace": NAMESPACE },
        "spec": {
            "providerRef": { "name": "capi-e2e-vc" },
            "template": "capi-e2e-template",
            "datacenter": "dc0",
            "cluster": "cluster0",
            "datastore": "ds0",
            "numCpus": 2,
            "memoryMiB": 2048,
            "firmware": "efi",
            "disks": [{ "name": "root", "sizeGiB": 20 }],
            "network": [{ "name": "eth0", "portGroup": "pg0", "ipam": {} }],
        },
    }))
    .expect("well-formed VSphereMachine");
    vsphere_machines
        .create(&pp, &vsphere_machine)
        .await
        .expect("creating the VSphereMachine");

    machines
        .create(
            &pp,
            &dynamic(
                "Machine",
                json!({
                    "metadata": { "name": MACHINE, "namespace": NAMESPACE },
                    "spec": {
                        "clusterName": CLUSTER,
                        "bootstrap": { "dataSecretName": "capi-e2e-bootstrap" },
                        "infrastructureRef": {
                            "apiGroup": INFRA_GROUP,
                            "kind": "VSphereMachine",
                            "name": MACHINE,
                        },
                    },
                }),
            ),
        )
        .await
        .expect("creating the CAPI Machine");

    // CAPI adopts the InfraMachine (owner reference + cluster-name label),
    // which needs `patch` on vspheremachines from the aggregate role.
    let adopted = wait_for("CAPI to take ownership of the VSphereMachine", || {
        let api = vsphere_machines.clone();
        async move {
            let vm = api.get(MACHINE).await.ok()?;
            let owned = vm.metadata.owner_references.as_ref().is_some_and(|refs| {
                refs.iter()
                    .any(|o| o.kind == "Machine" && o.name == MACHINE && o.controller == Some(true))
            });
            owned.then_some(vm)
        }
    })
    .await;
    assert_eq!(
        adopted
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get(CLUSTER_NAME_LABEL))
            .map(String::as_str),
        Some(CLUSTER),
        "CAPI labels the InfraMachine with its cluster"
    );

    // The provider reports the machine: providerID in spec, the rest in status.
    vsphere_machines
        .patch(
            MACHINE,
            &PatchParams::default(),
            &Patch::Merge(json!({ "spec": { "providerID": PROVIDER_ID } })),
        )
        .await
        .expect("patching VSphereMachine spec.providerID");
    vsphere_machines
        .patch_status(
            MACHINE,
            &PatchParams::default(),
            &Patch::Merge(json!({
                "status": {
                    "initialization": { "provisioned": true },
                    "addresses": [
                        { "type": "InternalIP", "address": INTERNAL_IP },
                        { "type": "ExternalIP", "address": EXTERNAL_IP },
                    ],
                },
            })),
        )
        .await
        .expect("patching VSphereMachine status");

    // One wait per contract field, so a timeout names the one CAPI missed.
    wait_for_machine(
        &machines,
        "Machine status.initialization.infrastructureProvisioned=true",
        |m| field(m, "/status/initialization/infrastructureProvisioned") == Some(&json!(true)),
    )
    .await;
    wait_for_machine(&machines, "Machine spec.providerID to be copied", |m| {
        field(m, "/spec/providerID") == Some(&json!(PROVIDER_ID))
    })
    .await;
    wait_for_machine(&machines, "Machine status.addresses to be copied", |m| {
        let addresses: Vec<&str> = field(m, "/status/addresses")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|a| a.get("address").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        addresses.contains(&INTERNAL_IP) && addresses.contains(&EXTERNAL_IP)
    })
    .await;

    // ── Deletion ────────────────────────────────────────────────────────────
    // Deleting the Machine deletes the VSphereMachine: CAPI issues the delete,
    // which needs `delete` on vspheremachines from the aggregate role.
    machines
        .delete(MACHINE, &DeleteParams::default())
        .await
        .expect("deleting the Machine");
    wait_until_gone("the VSphereMachine to be deleted with its Machine", || {
        let api = vsphere_machines.clone();
        async move { matches!(api.get_opt(MACHINE).await, Ok(None)) }
    })
    .await;
    wait_until_gone("the Machine to finish deleting", || {
        let api = machines.clone();
        async move { matches!(api.get_opt(MACHINE).await, Ok(None)) }
    })
    .await;

    // And the same for the Cluster and its VSphereCluster.
    clusters
        .delete(CLUSTER, &DeleteParams::default())
        .await
        .expect("deleting the Cluster");
    wait_until_gone("the VSphereCluster to be deleted with its Cluster", || {
        let api = vsphere_clusters.clone();
        async move { matches!(api.get_opt(CLUSTER).await, Ok(None)) }
    })
    .await;
    wait_until_gone("the Cluster to finish deleting", || {
        let api = clusters.clone();
        async move { matches!(api.get_opt(CLUSTER).await, Ok(None)) }
    })
    .await;
}
