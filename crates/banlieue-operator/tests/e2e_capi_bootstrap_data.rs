// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Cluster API bootstrap data reaches banlieue InfraMachines (ADR-0129),
//! against **real Cluster API controllers and a real bootstrap provider**.
//!
//! `make kind-e2e-capi-bootstrap` builds a fresh kind cluster, runs
//! `clusterctl init` (CAPI core, kubeadm bootstrap and control plane, pinned
//! in the Makefile; banlieue from a local clusterctl repository rendered from
//! this tree), applies `deploy/admission/capi-bootstrap-data-authorization.yaml`,
//! and starts **banlieue-controller as a local binary under its own
//! ServiceAccount's identity** (`scripts/kind-capi-controller.sh`): it reads
//! Secrets only where a Role lets it, exactly as in production. Nothing on the
//! bootstrap side is faked: kubeadm's control plane and bootstrap providers
//! render the data, Cluster API adopts the machines and publishes
//! `dataSecretName`, and banlieue's controller copies it.
//!
//! No host provider runs, so no guest is ever created, and nothing may
//! report one: the cases also assert that no machine is provisioned.
//!
//! Each case works in a namespace of its own, recreated on every run.
//! Addresses are RFC 5737 documentation addresses. `#[ignore]`d so
//! `cargo test` stays hermetic; an unreachable cluster, a missing policy or a
//! controller that is not running fails loudly.

mod e2e_common;

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use banlieue_api::capi::{
    BOOTSTRAP_DATA_READY, BOOTSTRAP_FORMAT_KEY, BOOTSTRAP_SECRET_TYPE, BOOTSTRAP_VALUE_KEY,
    CLUSTER_NAME_LABEL, bootstrap_reasons, owning_machine,
};
use banlieue_api::infrastructure::{CloudHypervisorMachine, CloudHypervisorMachineTemplate};
use k8s_openapi::ByteString;
use k8s_openapi::api::admissionregistration::v1::ValidatingAdmissionPolicy;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::api::rbac::v1::{Role, RoleBinding};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::api::{
    ApiResource, DynamicObject, GroupVersionKind, ListParams, Patch, PatchParams, PostParams,
};
use kube::{Api, Client, ResourceExt};
use serde_json::{Value, json};

use e2e_common::{client, wait_for};

const CAPI_GROUP: &str = "cluster.x-k8s.io";
const CONTROL_PLANE_GROUP: &str = "controlplane.cluster.x-k8s.io";
const BOOTSTRAP_GROUP: &str = "bootstrap.cluster.x-k8s.io";
const CAPI_VERSION: &str = "v1beta2";
const INFRA_GROUP: &str = "infrastructure.banlieue.io";

/// The controller's ServiceAccount, as `banlieue bootstrap operator` installs it.
const CONTROLLER_SA: &str = "banlieue-controller";
const CONTROLLER_NAMESPACE: &str = "banlieue-system";
/// The label the controller's `--namespace-selector` matches in this suite.
const SERVED_LABEL: &str = "banlieue.io/tenant";

/// Both policies in `capi-bootstrap-data-authorization.yaml`.
const MACHINE_POLICY: &str = "banlieue-capi-machine-bootstrap-data-authorization";
const TEMPLATE_POLICY: &str = "banlieue-capi-machine-template-bootstrap-data-authorization";

/// The guide's operator procedure: any metadata change wakes the machine.
const POKE_ANNOTATION: &str = "banlieue.io/reconcile-requested";

/// The Kubernetes version the control plane asks for. Nothing ever runs it:
/// kubeadm's providers only render it into the bootstrap data.
const K8S_VERSION: &str = "v1.34.1";

/// RFC 5737 documentation address for the control-plane endpoint.
const CONTROL_PLANE_HOST: &str = "192.0.2.10";
const CONTROL_PLANE_PORT: u16 = 6443;

/// A Provider that does not exist: no host provider runs in this suite.
const ABSENT_PROVIDER: &str = "e2e-absent-host";

/// The user the admission case impersonates: Machine RBAC, no Secret access.
const MACHINE_AUTHOR: &str = "e2e-machine-author";

/// A control plane the worker-only cases name and never create.
const ABSENT_CONTROL_PLANE: &str = "e2e-absent-control-plane";

/// Another infrastructure provider's group, outside banlieue's policies.
const OTHER_INFRA_GROUP: &str = "infrastructure.cluster.x-k8s.io";

/// Cluster API's documented opt-out of a MachineSet's preflight checks,
/// which need a running control plane the worker-only cases do not have.
const SKIP_PREFLIGHT_ANNOTATION: &str = "machineset.cluster.x-k8s.io/skip-preflight-checks";

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn resource(group: &str, kind: &str, plural: &str) -> ApiResource {
    let gvk = GroupVersionKind::gvk(group, CAPI_VERSION, kind);
    ApiResource::from_gvk_with_plural(&gvk, plural)
}

fn machines(client: &Client, ns: &str) -> Api<DynamicObject> {
    Api::namespaced_with(
        client.clone(),
        ns,
        &resource(CAPI_GROUP, "Machine", "machines"),
    )
}

fn machine_deployments(client: &Client, ns: &str) -> Api<DynamicObject> {
    Api::namespaced_with(
        client.clone(),
        ns,
        &resource(CAPI_GROUP, "MachineDeployment", "machinedeployments"),
    )
}

/// A dynamic object of `kind` in `group`/v1beta2 from a body with metadata.
fn object(group: &str, kind: &str, body: Value) -> DynamicObject {
    let mut value = body;
    value["apiVersion"] = json!(format!("{group}/{CAPI_VERSION}"));
    value["kind"] = json!(kind);
    serde_json::from_value(value).expect("a well-formed object")
}

async fn create(api: &Api<DynamicObject>, obj: &DynamicObject) {
    api.create(&PostParams::default(), obj)
        .await
        .unwrap_or_else(|e| {
            panic!(
                "creating {} {}: {e}",
                obj.types.as_ref().map_or("", |t| t.kind.as_str()),
                obj.name_any()
            )
        });
}

/// Recreate `ns`, labelled so the controller serves it
/// (`scripts/kind-capi-controller.sh` runs it with this selector; ADR-0130).
async fn fresh_namespace(client: &Client, ns: &str) {
    e2e_common::fresh_namespace(client, ns, &[(SERVED_LABEL, "true")]).await;
}

/// The Role an administrator creates so the controller can read bootstrap
/// Secrets in a namespace other than its own (ADR-0129 Decision 9; the guide
/// ships the same manifest).
async fn grant_controller_secret_reads(client: &Client, ns: &str) {
    let role: Role = serde_json::from_value(json!({
        "metadata": { "name": "banlieue-controller-capi-bootstrap", "namespace": ns },
        "rules": [{ "apiGroups": [""], "resources": ["secrets"], "verbs": ["get"] }]
    }))
    .expect("a Role");
    let binding: RoleBinding = serde_json::from_value(json!({
        "metadata": { "name": "banlieue-controller-capi-bootstrap", "namespace": ns },
        "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "Role",
                     "name": "banlieue-controller-capi-bootstrap" },
        "subjects": [{ "kind": "ServiceAccount", "name": CONTROLLER_SA,
                       "namespace": CONTROLLER_NAMESPACE }]
    }))
    .expect("a RoleBinding");
    Api::<Role>::namespaced(client.clone(), ns)
        .create(&PostParams::default(), &role)
        .await
        .expect("creating the controller's Role");
    Api::<RoleBinding>::namespaced(client.clone(), ns)
        .create(&PostParams::default(), &binding)
        .await
        .expect("binding the controller's Role");
}

/// A CloudHypervisorMachine template naming a Provider that does not exist.
async fn create_machine_template(client: &Client, ns: &str, name: &str) {
    let template: CloudHypervisorMachineTemplate = serde_json::from_value(json!({
        "apiVersion": format!("{INFRA_GROUP}/v1alpha1"),
        "kind": "CloudHypervisorMachineTemplate",
        "metadata": { "name": name, "namespace": ns },
        "spec": { "template": { "spec": {
            "providerRef": { "name": ABSENT_PROVIDER },
            "cpus": { "boot": 2 },
            "memory": { "sizeMiB": 4096 },
            "storageClass": "default",
            "bootSource": { "kind": "image", "image": "ubuntu-24.04.raw" },
            "osDiskSizeGiB": 20,
            "nics": []
        } } }
    }))
    .expect("a CloudHypervisorMachineTemplate");
    Api::<CloudHypervisorMachineTemplate>::namespaced(client.clone(), ns)
        .create(&PostParams::default(), &template)
        .await
        .expect("creating the CloudHypervisorMachineTemplate");
}

/// A `Cluster` with a control-plane endpoint and a `KubeadmControlPlane`
/// reference: Cluster API's webhook requires a Cluster to name a control
/// plane, an `infrastructureRef` or a topology. No `infrastructureRef`: no
/// banlieue backend but vSphere has an InfraCluster. Cases that need no real
/// control plane name one that is never created ([`ABSENT_CONTROL_PLANE`]).
async fn create_cluster(client: &Client, ns: &str, name: &str, control_plane: &str) {
    let cluster = object(
        CAPI_GROUP,
        "Cluster",
        json!({
            "metadata": { "name": name, "namespace": ns },
            "spec": {
                "controlPlaneEndpoint": { "host": CONTROL_PLANE_HOST, "port": CONTROL_PLANE_PORT },
                "controlPlaneRef": {
                    "apiGroup": CONTROL_PLANE_GROUP, "kind": "KubeadmControlPlane", "name": control_plane
                }
            }
        }),
    );
    create(
        &Api::namespaced_with(
            client.clone(),
            ns,
            &resource(CAPI_GROUP, "Cluster", "clusters"),
        ),
        &cluster,
    )
    .await;
}

/// A CloudHypervisorMachine written by hand for a hand-written Machine,
/// labelled with its cluster so it is held from creation, with `owners` as
/// its ownerReferences.
async fn create_infra_machine(client: &Client, ns: &str, name: &str, cluster: &str, owners: Value) {
    let infra: CloudHypervisorMachine = serde_json::from_value(json!({
        "apiVersion": format!("{INFRA_GROUP}/v1alpha1"),
        "kind": "CloudHypervisorMachine",
        "metadata": { "name": name, "namespace": ns,
                      "labels": { CLUSTER_NAME_LABEL: cluster },
                      "ownerReferences": owners },
        "spec": {
            "providerRef": { "name": ABSENT_PROVIDER },
            "cpus": { "boot": 2 },
            "memory": { "sizeMiB": 4096 },
            "storageClass": "default",
            "bootSource": { "kind": "image", "image": "ubuntu-24.04.raw" },
            "osDiskSizeGiB": 20,
            "nics": []
        }
    }))
    .expect("a CloudHypervisorMachine");
    Api::<CloudHypervisorMachine>::namespaced(client.clone(), ns)
        .create(&PostParams::default(), &infra)
        .await
        .expect("creating the CloudHypervisorMachine");
}

/// A hand-written v1beta2 Machine naming `secret` directly and `infra` as
/// its CloudHypervisorMachine.
fn hand_written_machine(
    ns: &str,
    name: &str,
    cluster: &str,
    secret: &str,
    infra: &str,
) -> DynamicObject {
    object(
        CAPI_GROUP,
        "Machine",
        json!({
            "metadata": { "name": name, "namespace": ns },
            "spec": {
                "clusterName": cluster,
                "bootstrap": { "dataSecretName": secret },
                "infrastructureRef": {
                    "apiGroup": INFRA_GROUP, "kind": "CloudHypervisorMachine", "name": infra
                }
            }
        }),
    )
}

/// A bootstrap-data Secret as a bootstrap provider shapes it, written by the
/// test (for the cases where the Machine names one directly).
fn bootstrap_secret(ns: &str, name: &str, cluster: &str, type_: &str, value: &str) -> Secret {
    Secret {
        metadata: kube::api::ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ns.to_string()),
            labels: Some(BTreeMap::from([(
                CLUSTER_NAME_LABEL.to_string(),
                cluster.to_string(),
            )])),
            ..Default::default()
        },
        type_: Some(type_.to_string()),
        data: Some(BTreeMap::from([
            (
                BOOTSTRAP_VALUE_KEY.to_string(),
                ByteString(value.as_bytes().to_vec()),
            ),
            (
                BOOTSTRAP_FORMAT_KEY.to_string(),
                ByteString(b"cloud-config".to_vec()),
            ),
        ])),
        ..Default::default()
    }
}

async fn create_secret(client: &Client, secret: &Secret) {
    Api::<Secret>::namespaced(
        client.clone(),
        secret.namespace().as_deref().expect("namespace"),
    )
    .create(&PostParams::default(), secret)
    .await
    .expect("creating a Secret");
}

/// The Secret's `value`, as the controller must have copied it.
async fn secret_value(client: &Client, ns: &str, name: &str) -> String {
    let secret = Api::<Secret>::namespaced(client.clone(), ns)
        .get(name)
        .await
        .expect("reading the bootstrap Secret");
    let bytes = secret
        .data
        .and_then(|d| d.get(BOOTSTRAP_VALUE_KEY).cloned())
        .expect("the bootstrap Secret has a value");
    String::from_utf8(bytes.0).expect("UTF-8 bootstrap data")
}

fn bootstrap_condition(m: &CloudHypervisorMachine) -> Option<&Condition> {
    m.status
        .as_ref()?
        .conditions
        .iter()
        .find(|c| c.type_ == BOOTSTRAP_DATA_READY)
}

/// The delivery condition reports the generation that carries the data: the
/// `spec.userData` apply bumps it, and the condition is written after.
fn assert_condition_is_current(m: &CloudHypervisorMachine) {
    let condition = bootstrap_condition(m).expect("BootstrapDataReady");
    assert_eq!(
        condition.observed_generation, m.metadata.generation,
        "BootstrapDataReady observed a generation before the one carrying the data"
    );
}

fn has_reason(m: &CloudHypervisorMachine, status: &str, reason: &str) -> bool {
    bootstrap_condition(m).is_some_and(|c| c.status == status && c.reason == reason)
}

/// The one CloudHypervisorMachine in `ns`, once a Machine owns it.
async fn adopted_machine(client: &Client, ns: &str, what: &str) -> CloudHypervisorMachine {
    let api: Api<CloudHypervisorMachine> = Api::namespaced(client.clone(), ns);
    wait_for(what, || {
        let api = api.clone();
        async move {
            let list = api.list(&ListParams::default()).await.ok()?;
            list.items
                .into_iter()
                .find(|m| owning_machine(&m.metadata).is_some())
        }
    })
    .await
}

/// Wait until the named CloudHypervisorMachine satisfies `check`.
async fn wait_for_infra<F>(
    client: &Client,
    ns: &str,
    name: &str,
    what: &str,
    check: F,
) -> CloudHypervisorMachine
where
    F: Fn(&CloudHypervisorMachine) -> bool,
{
    let api: Api<CloudHypervisorMachine> = Api::namespaced(client.clone(), ns);
    let check = &check;
    wait_for(what, || {
        let api = api.clone();
        let name = name.to_string();
        async move {
            let m = api.get(&name).await.ok()?;
            check(&m).then_some(m)
        }
    })
    .await
}

/// The owning Machine's `spec.bootstrap.dataSecretName`, once CAPI sets it.
async fn data_secret_name(client: &Client, ns: &str, machine: &str) -> String {
    let api = machines(client, ns);
    wait_for("CAPI to publish the Machine's dataSecretName", || {
        let api = api.clone();
        let machine = machine.to_string();
        async move {
            let m = api.get(&machine).await.ok()?;
            m.data
                .pointer("/spec/bootstrap/dataSecretName")?
                .as_str()
                .map(str::to_string)
        }
    })
    .await
}

/// Nothing reports a guest that no provider created.
async fn assert_nothing_provisioned(client: &Client, ns: &str, infra: &CloudHypervisorMachine) {
    let provisioned = infra
        .status
        .as_ref()
        .and_then(|s| s.initialization.provisioned);
    assert_ne!(
        provisioned,
        Some(true),
        "no provider runs, yet the machine reports provisioned"
    );
    let owner = owning_machine(&infra.metadata)
        .expect("adopted")
        .name
        .clone();
    let machine = machines(client, ns).get(&owner).await.expect("the Machine");
    assert_ne!(
        machine
            .data
            .pointer("/status/initialization/infrastructureProvisioned")
            .and_then(Value::as_bool),
        Some(true),
        "CAPI reports infrastructure provisioned for a guest that was never created"
    );
}

/// The policies this suite tests must be installed, or the admission case
/// would pass vacuously.
async fn assert_policy_installed(client: &Client) {
    let policies: Api<ValidatingAdmissionPolicy> = Api::all(client.clone());
    for name in [MACHINE_POLICY, TEMPLATE_POLICY] {
        assert!(
            policies
                .get_opt(name)
                .await
                .expect("reading policies")
                .is_some(),
            "{name} is not installed: apply deploy/admission/capi-bootstrap-data-authorization.yaml"
        );
    }
}

async fn poke(client: &Client, ns: &str, name: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after 1970")
        .as_nanos()
        .to_string();
    Api::<CloudHypervisorMachine>::namespaced(client.clone(), ns)
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(json!({ "metadata": { "annotations": { POKE_ANNOTATION: now } } })),
        )
        .await
        .expect("annotating the machine");
}

const CONTROLLER_HINT: &str =
    " (is banlieue-controller running? `scripts/kind-capi-controller.sh start`)";

// ---------------------------------------------------------------------------
// 1. A control plane's init data, rendered by kubeadm, reaches the machine
// ---------------------------------------------------------------------------

/// The ADR's main case, nothing faked: a `KubeadmControlPlane` over a
/// `CloudHypervisorMachineTemplate`. Its first Machine gets init data, which
/// kubeadm renders with no running control plane. The namespace has no Role
/// for the controller at first, so the machine must report
/// `BootstrapSecretForbidden` and carry no data; the guide's Role and its
/// "poke" procedure then deliver it, byte for byte.
#[tokio::test]
#[ignore = "requires `make kind-e2e-capi-bootstrap`"]
async fn kubeadm_init_data_reaches_the_cloud_hypervisor_machine() {
    const NS: &str = "banlieue-cbd-kcp";
    const CLUSTER: &str = "cbd-kcp";
    let client = client().await;
    fresh_namespace(&client, NS).await;

    create_machine_template(&client, NS, CLUSTER).await;
    let kcp = object(
        CONTROL_PLANE_GROUP,
        "KubeadmControlPlane",
        json!({
            "metadata": { "name": CLUSTER, "namespace": NS },
            "spec": {
                "replicas": 1,
                "version": K8S_VERSION,
                "machineTemplate": { "spec": { "infrastructureRef": {
                    "apiGroup": INFRA_GROUP,
                    "kind": "CloudHypervisorMachineTemplate",
                    "name": CLUSTER
                } } },
                // At least one property is required; this is the one
                // format banlieue delivers.
                "kubeadmConfigSpec": { "format": "cloud-config" }
            }
        }),
    );
    create(
        &Api::namespaced_with(
            client.clone(),
            NS,
            &resource(
                CONTROL_PLANE_GROUP,
                "KubeadmControlPlane",
                "kubeadmcontrolplanes",
            ),
        ),
        &kcp,
    )
    .await;
    create_cluster(&client, NS, CLUSTER, CLUSTER).await;

    let infra = adopted_machine(
        &client,
        NS,
        "the control plane's CloudHypervisorMachine to be adopted by its Machine",
    )
    .await;
    let name = infra.name_any();
    let owner = owning_machine(&infra.metadata)
        .expect("adopted")
        .name
        .clone();
    let secret = data_secret_name(&client, NS, &owner).await;

    // No Role in this namespace yet: the production identity cannot read it.
    let refused = wait_for_infra(
        &client,
        NS,
        &name,
        &format!("BootstrapSecretForbidden without a Role{CONTROLLER_HINT}"),
        |m| has_reason(m, "False", bootstrap_reasons::BOOTSTRAP_SECRET_FORBIDDEN),
    )
    .await;
    assert_eq!(
        refused.spec.user_data, None,
        "nothing copied without access"
    );

    grant_controller_secret_reads(&client, NS).await;
    poke(&client, NS, &name).await;

    let expected = secret_value(&client, NS, &secret).await;
    let delivered = wait_for_infra(
        &client,
        NS,
        &name,
        "the bootstrap data to be delivered after the Role is granted",
        |m| has_reason(m, "True", bootstrap_reasons::BOOTSTRAP_DATA_DELIVERED),
    )
    .await;
    assert_eq!(
        delivered.spec.user_data.as_deref(),
        Some(expected.as_str()),
        "spec.userData must be the Secret's value, byte for byte"
    );
    assert_condition_is_current(&delivered);
    assert_nothing_provisioned(&client, NS, &delivered).await;
}

// ---------------------------------------------------------------------------
// 2. A MachineDeployment naming its Secret directly; template propagation
// ---------------------------------------------------------------------------

/// A `MachineDeployment` whose template names a bootstrap Secret directly,
/// with an empty `format`. Also settles ADR-0129's open question about in-place propagation: after
/// delivery, a template label change reaches the existing machine (so Cluster
/// API did re-apply to it), and `spec.userData` must survive that.
#[tokio::test]
#[ignore = "requires `make kind-e2e-capi-bootstrap`"]
async fn a_machinedeployment_secret_is_delivered_and_survives_propagation() {
    const NS: &str = "banlieue-cbd-md";
    const CLUSTER: &str = "cbd-md";
    const SECRET: &str = "cbd-md-bootstrap";
    const PROBE_LABEL: &str = "e2e.banlieue.io/propagation-probe";
    const VALUE: &str = "#cloud-config\nruncmd:\n  - [sh, -c, 'echo cbd-md']\n";
    let client = client().await;
    fresh_namespace(&client, NS).await;
    grant_controller_secret_reads(&client, NS).await;

    // `format` written empty: read as cloud-config, as CABPK reads an unset
    // format (its `storeBootstrapData` never writes it empty itself).
    let mut secret = bootstrap_secret(NS, SECRET, CLUSTER, BOOTSTRAP_SECRET_TYPE, VALUE);
    secret
        .data
        .as_mut()
        .expect("data")
        .insert(BOOTSTRAP_FORMAT_KEY.to_string(), ByteString(Vec::new()));
    create_secret(&client, &secret).await;
    create_machine_template(&client, NS, CLUSTER).await;
    create_cluster(&client, NS, CLUSTER, ABSENT_CONTROL_PLANE).await;
    let md = object(
        CAPI_GROUP,
        "MachineDeployment",
        json!({
            // The Cluster's control plane does not exist here, and a
            // MachineSet's preflight checks would wait for it forever.
            "metadata": { "name": CLUSTER, "namespace": NS, "annotations": {
                SKIP_PREFLIGHT_ANNOTATION: "All"
            } },
            "spec": {
                "clusterName": CLUSTER,
                "replicas": 1,
                "selector": { "matchLabels": { "e2e.banlieue.io/pool": CLUSTER } },
                "template": {
                    "metadata": { "labels": { "e2e.banlieue.io/pool": CLUSTER } },
                    "spec": {
                        "clusterName": CLUSTER,
                        "bootstrap": { "dataSecretName": SECRET },
                        "infrastructureRef": {
                            "apiGroup": INFRA_GROUP,
                            "kind": "CloudHypervisorMachineTemplate",
                            "name": CLUSTER
                        }
                    }
                }
            }
        }),
    );
    create(&machine_deployments(&client, NS), &md).await;

    let infra = adopted_machine(&client, NS, "the MachineSet's machine to be adopted").await;
    let name = infra.name_any();
    let delivered = wait_for_infra(
        &client,
        NS,
        &name,
        &format!("the MachineDeployment's bootstrap data to be delivered{CONTROLLER_HINT}"),
        |m| has_reason(m, "True", bootstrap_reasons::BOOTSTRAP_DATA_DELIVERED),
    )
    .await;
    assert_eq!(delivered.spec.user_data.as_deref(), Some(VALUE));
    assert_condition_is_current(&delivered);

    machine_deployments(&client, NS)
        .patch(
            CLUSTER,
            &PatchParams::default(),
            &Patch::Merge(json!({ "spec": { "template": { "metadata": { "labels": {
                PROBE_LABEL: "1"
            } } } } })),
        )
        .await
        .expect("labelling the MachineDeployment's template");
    let propagated = wait_for_infra(
        &client,
        NS,
        &name,
        "Cluster API to propagate the template label to the existing machine",
        |m| m.labels().get(PROBE_LABEL).map(String::as_str) == Some("1"),
    )
    .await;
    assert_eq!(
        propagated.spec.user_data.as_deref(),
        Some(VALUE),
        "in-place propagation must not remove the delivered bootstrap data"
    );
    assert_nothing_provisioned(&client, NS, &propagated).await;
}

// ---------------------------------------------------------------------------
// 3. A Secret that is not bootstrap data is reported, never copied
// ---------------------------------------------------------------------------

/// A Machine naming an `Opaque` Secret (a Provider's credentials, say), with a
/// hand-written InfraMachine carrying the cluster label so it is held before
/// adoption. The controller can read the Secret, and still must not copy it.
#[tokio::test]
#[ignore = "requires `make kind-e2e-capi-bootstrap`"]
async fn a_secret_that_is_not_bootstrap_data_is_refused() {
    const NS: &str = "banlieue-cbd-bad";
    const CLUSTER: &str = "cbd-bad";
    const SECRET: &str = "not-bootstrap-data";
    const MACHINE: &str = "cbd-bad-0";
    let client = client().await;
    fresh_namespace(&client, NS).await;
    grant_controller_secret_reads(&client, NS).await;
    create_secret(
        &client,
        &bootstrap_secret(NS, SECRET, CLUSTER, "Opaque", "password: hunter2\n"),
    )
    .await;
    create_cluster(&client, NS, CLUSTER, ABSENT_CONTROL_PLANE).await;

    create_infra_machine(&client, NS, MACHINE, CLUSTER, json!([])).await;
    create(
        &machines(&client, NS),
        &hand_written_machine(NS, MACHINE, CLUSTER, SECRET, MACHINE),
    )
    .await;

    let refused = wait_for_infra(
        &client,
        NS,
        MACHINE,
        &format!("BootstrapSecretInvalid for an Opaque Secret{CONTROLLER_HINT}"),
        |m| has_reason(m, "False", bootstrap_reasons::BOOTSTRAP_SECRET_INVALID),
    )
    .await;
    assert_eq!(
        refused.spec.user_data, None,
        "an Opaque Secret is never copied"
    );
    let message = &bootstrap_condition(&refused).expect("condition").message;
    assert!(
        !message.contains("hunter2"),
        "the condition leaked the Secret: {message}"
    );
    assert_nothing_provisioned(&client, NS, &refused).await;
}

// ---------------------------------------------------------------------------
// 4. Admission: a principal may only name bootstrap data it can read
// ---------------------------------------------------------------------------

/// A client that impersonates [`MACHINE_AUTHOR`] with no groups, so its
/// RBAC (and therefore the policy's `authorizer`) is only what the case
/// grants it.
async fn as_machine_author() -> Client {
    let mut config = kube::Config::infer().await.expect("kubeconfig");
    config.auth_info.impersonate = Some(MACHINE_AUTHOR.to_string());
    Client::try_from(config).expect("an impersonating client")
}

/// HTTP status of a refused write, or `None` when admitted.
fn refusal(result: Result<DynamicObject, kube::Error>) -> Option<(u16, String)> {
    match result {
        Ok(_) => None,
        Err(kube::Error::Api(e)) => Some((e.code, e.message)),
        Err(e) => panic!("not an API answer: {e}"),
    }
}

const HTTP_FORBIDDEN: u16 = 403;

#[tokio::test]
#[ignore = "requires `make kind-e2e-capi-bootstrap`"]
async fn a_principal_may_only_name_bootstrap_data_it_can_read() {
    const NS: &str = "banlieue-cbd-adm";
    const CLUSTER: &str = "cbd-adm";
    const SECRET: &str = "cbd-adm-other-machine";
    const READABLE: &str = "cbd-adm-readable";
    const OTHER_CONFIG: &str = "cbd-adm-other-config";
    const READABLE_CONFIG: &str = "cbd-adm-readable-config";
    let admin = client().await;
    assert_policy_installed(&admin).await;
    fresh_namespace(&admin, NS).await;

    // Another Machine's bootstrap data, and one the author is allowed to read.
    for name in [SECRET, READABLE] {
        create_secret(
            &admin,
            &bootstrap_secret(NS, name, CLUSTER, BOOTSTRAP_SECRET_TYPE, "#cloud-config\n"),
        )
        .await;
    }
    let author_role: Role = serde_json::from_value(json!({
        "metadata": { "name": MACHINE_AUTHOR, "namespace": NS },
        "rules": [
            { "apiGroups": [CAPI_GROUP],
              "resources": ["machines", "machinesets", "machinedeployments"],
              "verbs": ["get", "list", "create", "update", "patch"] },
            { "apiGroups": [""], "resources": ["secrets"],
              "resourceNames": [READABLE], "verbs": ["get"] },
            { "apiGroups": [BOOTSTRAP_GROUP],
              "resources": ["kubeadmconfigs", "kubeadmconfigtemplates"],
              "resourceNames": [READABLE_CONFIG], "verbs": ["get"] }
        ]
    }))
    .expect("a Role");
    let author_binding: RoleBinding = serde_json::from_value(json!({
        "metadata": { "name": MACHINE_AUTHOR, "namespace": NS },
        "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": MACHINE_AUTHOR },
        "subjects": [{ "apiGroup": "rbac.authorization.k8s.io", "kind": "User", "name": MACHINE_AUTHOR }]
    }))
    .expect("a RoleBinding");
    Api::<Role>::namespaced(admin.clone(), NS)
        .create(&PostParams::default(), &author_role)
        .await
        .expect("creating the author's Role");
    Api::<RoleBinding>::namespaced(admin.clone(), NS)
        .create(&PostParams::default(), &author_binding)
        .await
        .expect("binding the author's Role");

    let secret_ref = |secret: &str| json!({ "dataSecretName": secret });
    let config_ref = |kind: &str, config: &str| json!({ "configRef": { "apiGroup": BOOTSTRAP_GROUP, "kind": kind, "name": config } });
    let banlieue_infra =
        |kind: &str, name: &str| json!({ "apiGroup": INFRA_GROUP, "kind": kind, "name": name });
    // Another infrastructure provider's InfraMachine: outside both policies.
    let other_infra = |kind: &str, name: &str| json!({ "apiGroup": OTHER_INFRA_GROUP, "kind": kind, "name": name });
    let machine_with = |name: &str, bootstrap: Value, infra: Value| {
        object(
            CAPI_GROUP,
            "Machine",
            json!({
                "metadata": { "name": name, "namespace": NS },
                "spec": {
                    "clusterName": CLUSTER,
                    "bootstrap": bootstrap,
                    "infrastructureRef": infra
                }
            }),
        )
    };
    let machine_naming = |name: &str, bootstrap: Value| {
        machine_with(
            name,
            bootstrap,
            banlieue_infra("CloudHypervisorMachine", name),
        )
    };
    let deployment_with = |name: &str, bootstrap: Value, infra: Value| {
        object(
            CAPI_GROUP,
            "MachineDeployment",
            json!({
                "metadata": { "name": name, "namespace": NS },
                "spec": {
                    "clusterName": CLUSTER,
                    "selector": { "matchLabels": { "pool": name } },
                    "template": {
                        "metadata": { "labels": { "pool": name } },
                        "spec": {
                            "clusterName": CLUSTER,
                            "bootstrap": bootstrap,
                            "infrastructureRef": infra
                        }
                    }
                }
            }),
        )
    };
    let dry_run = PostParams {
        dry_run: true,
        ..Default::default()
    };
    let author = as_machine_author().await;
    let author_machines = machines(&author, NS);

    // Naming another Machine's bootstrap Secret: refused.
    let (code, message) = refusal(
        author_machines
            .create(&dry_run, &machine_naming("m-other", secret_ref(SECRET)))
            .await,
    )
    .expect("a Machine naming a Secret its author cannot read must be refused");
    assert_eq!(code, HTTP_FORBIDDEN, "{message}");
    assert!(message.contains("not authorized to get"), "{message}");

    // The same through a MachineDeployment's template: refused.
    let author_deployments = machine_deployments(&author, NS);
    let md = deployment_with(
        "md-other",
        secret_ref(SECRET),
        banlieue_infra("CloudHypervisorMachineTemplate", "x"),
    );
    let (code, message) = refusal(author_deployments.create(&dry_run, &md).await)
        .expect("a MachineDeployment template naming an unreadable Secret must be refused");
    assert_eq!(code, HTTP_FORBIDDEN, "{message}");

    // A bootstrap config the author cannot read names its Secret indirectly
    // (Cluster API copies the config's dataSecretName): refused, on a Machine
    // and through a template.
    let (code, message) = refusal(
        author_machines
            .create(
                &dry_run,
                &machine_naming("m-other-config", config_ref("KubeadmConfig", OTHER_CONFIG)),
            )
            .await,
    )
    .expect("a Machine naming a bootstrap config its author cannot read must be refused");
    assert_eq!(code, HTTP_FORBIDDEN, "{message}");
    assert!(message.contains("spec.bootstrap.configRef"), "{message}");
    let md = deployment_with(
        "md-other-config",
        config_ref("KubeadmConfigTemplate", OTHER_CONFIG),
        banlieue_infra("CloudHypervisorMachineTemplate", "x"),
    );
    let (code, message) = refusal(author_deployments.create(&dry_run, &md).await).expect(
        "a template naming a bootstrap config template its author cannot read must be refused",
    );
    assert_eq!(code, HTTP_FORBIDDEN, "{message}");

    // A Secret or bootstrap config the author can read: admitted.
    assert_eq!(
        refusal(
            author_machines
                .create(
                    &dry_run,
                    &machine_naming("m-readable", secret_ref(READABLE))
                )
                .await
        ),
        None
    );
    assert_eq!(
        refusal(
            author_machines
                .create(
                    &dry_run,
                    &machine_naming("m-config", config_ref("KubeadmConfig", READABLE_CONFIG)),
                )
                .await
        ),
        None
    );
    assert_eq!(
        refusal(
            author_deployments
                .create(
                    &dry_run,
                    &deployment_with(
                        "md-config",
                        config_ref("KubeadmConfigTemplate", READABLE_CONFIG),
                        banlieue_infra("CloudHypervisorMachineTemplate", "x"),
                    ),
                )
                .await
        ),
        None
    );

    // Another infrastructure provider's Machines are not banlieue's business:
    // neither policy matches them, even naming an unreadable Secret.
    assert_eq!(
        refusal(
            author_machines
                .create(
                    &dry_run,
                    &machine_with(
                        "m-elsewhere",
                        secret_ref(SECRET),
                        other_infra("DockerMachine", "m-elsewhere"),
                    ),
                )
                .await
        ),
        None
    );
    assert_eq!(
        refusal(
            author_deployments
                .create(
                    &dry_run,
                    &deployment_with(
                        "md-elsewhere",
                        secret_ref(SECRET),
                        other_infra("DockerMachineTemplate", "x"),
                    ),
                )
                .await
        ),
        None
    );

    // A reference made under another provider was never checked: an update
    // that only switches infrastructureRef to banlieue is checked as if the
    // reference were new. Refused for data the author cannot read, admitted
    // for data it can (so the refusal is the policy's, not Cluster API's).
    let switch_to_banlieue = json!({ "spec": { "template": { "spec": {
        "infrastructureRef": banlieue_infra("CloudHypervisorMachineTemplate", "x")
    } } } });
    for (name, bootstrap, refused_on) in [
        ("md-switch", secret_ref(SECRET), Some("dataSecretName")),
        (
            "md-switch-config",
            config_ref("KubeadmConfigTemplate", OTHER_CONFIG),
            Some("configRef"),
        ),
        ("md-switch-readable", secret_ref(READABLE), None),
    ] {
        create(
            &author_deployments,
            &deployment_with(name, bootstrap, other_infra("DockerMachineTemplate", "x")),
        )
        .await;
        let switched = author_deployments
            .patch(
                name,
                &PatchParams {
                    dry_run: true,
                    ..Default::default()
                },
                &Patch::Merge(switch_to_banlieue.clone()),
            )
            .await;
        match refused_on {
            Some(field) => {
                let (code, message) = refusal(switched).unwrap_or_else(|| {
                    panic!("{name}: switching to banlieue must check the {field} it carries")
                });
                assert_eq!(code, HTTP_FORBIDDEN, "{message}");
                assert!(message.contains(field), "{message}");
            }
            None => assert_eq!(refusal(switched), None, "{name}"),
        }
    }

    // An update that leaves dataSecretName alone is admitted, whoever makes it:
    // the reference was authorized when it was made.
    create(
        &machines(&admin, NS),
        &machine_naming("m-admin", secret_ref(SECRET)),
    )
    .await;
    let relabel = author_machines
        .patch(
            "m-admin",
            &PatchParams {
                dry_run: true,
                ..Default::default()
            },
            &Patch::Merge(json!({ "metadata": { "labels": { "touched": "yes" } } })),
        )
        .await;
    assert_eq!(refusal(relabel), None);
}

// ---------------------------------------------------------------------------
// 5. An owner reference alone does not get another Machine's data
// ---------------------------------------------------------------------------

/// Someone who may create InfraMachines writes one whose ownerReference names
/// another Machine, with that Machine's real UID. The ownerReference is
/// ordinary metadata, and the Machine does not name this InfraMachine in its
/// `spec.infrastructureRef`, so the controller must report
/// `MachineReferenceMismatch` and copy nothing, while the Machine's own
/// InfraMachine still gets its data.
#[tokio::test]
#[ignore = "requires `make kind-e2e-capi-bootstrap`"]
async fn an_owner_reference_alone_does_not_get_another_machines_data() {
    const NS: &str = "banlieue-cbd-own";
    const CLUSTER: &str = "cbd-own";
    const SECRET: &str = "cbd-own-victim";
    const VICTIM: &str = "cbd-own-victim";
    const FORGED: &str = "cbd-own-forged";
    const VALUE: &str = "#cloud-config\nruncmd:\n  - echo victim-join-token\n";
    let client = client().await;
    fresh_namespace(&client, NS).await;
    grant_controller_secret_reads(&client, NS).await;
    create_secret(
        &client,
        &bootstrap_secret(NS, SECRET, CLUSTER, BOOTSTRAP_SECRET_TYPE, VALUE),
    )
    .await;
    create_cluster(&client, NS, CLUSTER, ABSENT_CONTROL_PLANE).await;
    create_infra_machine(&client, NS, VICTIM, CLUSTER, json!([])).await;
    create(
        &machines(&client, NS),
        &hand_written_machine(NS, VICTIM, CLUSTER, SECRET, VICTIM),
    )
    .await;

    // The Machine's own InfraMachine: delivered, so the Secret is valid and
    // readable, and only the forged reference stands between it and FORGED.
    let delivered = wait_for_infra(
        &client,
        NS,
        VICTIM,
        &format!("the Machine's own InfraMachine to receive its data{CONTROLLER_HINT}"),
        |m| has_reason(m, "True", bootstrap_reasons::BOOTSTRAP_DATA_DELIVERED),
    )
    .await;
    assert_eq!(delivered.spec.user_data.as_deref(), Some(VALUE));
    assert_condition_is_current(&delivered);

    let victim = machines(&client, NS)
        .get(VICTIM)
        .await
        .expect("the Machine");
    let uid = victim.metadata.uid.clone().expect("a Machine has a uid");
    create_infra_machine(
        &client,
        NS,
        FORGED,
        CLUSTER,
        json!([{ "apiVersion": format!("{CAPI_GROUP}/{CAPI_VERSION}"), "kind": "Machine",
                 "name": VICTIM, "uid": uid, "controller": true }]),
    )
    .await;

    let refused = wait_for_infra(
        &client,
        NS,
        FORGED,
        &format!("MachineReferenceMismatch on the forged InfraMachine{CONTROLLER_HINT}"),
        |m| has_reason(m, "False", bootstrap_reasons::MACHINE_REFERENCE_MISMATCH),
    )
    .await;
    assert_eq!(
        refused.spec.user_data, None,
        "a forged owner reference received another Machine's bootstrap data"
    );
    let message = &bootstrap_condition(&refused).expect("condition").message;
    assert!(
        !message.contains("victim-join-token"),
        "the condition leaked the Secret: {message}"
    );
    assert_ne!(
        refused
            .status
            .as_ref()
            .and_then(|s| s.initialization.provisioned),
        Some(true),
        "no provider runs, yet the forged machine reports provisioned"
    );
}

// ---------------------------------------------------------------------------
// 6. Cluster API refuses a bootstrap config another Machine controls
// ---------------------------------------------------------------------------

/// Cluster API's reason on a Machine whose bootstrap config it could not
/// reconcile, here because another Machine already controls that config.
const INTERNAL_ERROR_REASON: &str = "InternalError";

/// The admission policy authorizes who may name a bootstrap config; this case
/// pins what Cluster API itself does when a config is already adopted, as an
/// admin (whom the policy admits) names it from a second Machine: CAPI cannot
/// make the second Machine its controller, so it never copies the config's
/// `dataSecretName`, and nothing reaches the second Machine's InfraMachine.
/// A config not yet adopted (owned only by a MachineSet or control plane, or
/// by nobody) CAPI would take over; that path is the policy's to refuse, and
/// case 4 tests it.
#[tokio::test]
#[ignore = "requires `make kind-e2e-capi-bootstrap`"]
async fn an_adopted_bootstrap_config_is_not_shared_with_another_machine() {
    const NS: &str = "banlieue-cbd-cfg";
    const CLUSTER: &str = "cbd-cfg";
    const VICTIM: &str = "cbd-cfg-victim";
    const OTHER: &str = "cbd-cfg-other";
    let client = client().await;
    fresh_namespace(&client, NS).await;
    grant_controller_secret_reads(&client, NS).await;
    create_cluster(&client, NS, CLUSTER, ABSENT_CONTROL_PLANE).await;

    let configs = Api::<DynamicObject>::namespaced_with(
        client.clone(),
        NS,
        &resource(BOOTSTRAP_GROUP, "KubeadmConfig", "kubeadmconfigs"),
    );
    create(
        &configs,
        &object(
            BOOTSTRAP_GROUP,
            "KubeadmConfig",
            json!({ "metadata": { "name": VICTIM, "namespace": NS },
                    "spec": { "format": "cloud-config" } }),
        ),
    )
    .await;
    let machine_on = |name: &str| {
        object(
            CAPI_GROUP,
            "Machine",
            json!({
                "metadata": { "name": name, "namespace": NS },
                "spec": {
                    "clusterName": CLUSTER,
                    "bootstrap": { "configRef": {
                        "apiGroup": BOOTSTRAP_GROUP, "kind": "KubeadmConfig", "name": VICTIM } },
                    "infrastructureRef": {
                        "apiGroup": INFRA_GROUP, "kind": "CloudHypervisorMachine", "name": name
                    }
                }
            }),
        )
    };
    create_infra_machine(&client, NS, VICTIM, CLUSTER, json!([])).await;
    create(&machines(&client, NS), &machine_on(VICTIM)).await;

    // The victim Machine adopts its config first.
    let controller_of = |config: &DynamicObject| {
        config
            .metadata
            .owner_references
            .iter()
            .flatten()
            .find(|o| o.controller == Some(true))
            .map(|o| (o.kind.clone(), o.name.clone()))
    };
    wait_for("the victim Machine to adopt its bootstrap config", || {
        let configs = configs.clone();
        async move {
            let config = configs.get(VICTIM).await.ok()?;
            (controller_of(&config) == Some(("Machine".to_string(), VICTIM.to_string())))
                .then_some(())
        }
    })
    .await;

    create_infra_machine(&client, NS, OTHER, CLUSTER, json!([])).await;
    create(&machines(&client, NS), &machine_on(OTHER)).await;

    // Positive signal that CAPI reconciled, and refused, the second Machine.
    let api = machines(&client, NS);
    let refused = wait_for(
        "Cluster API to refuse the second Machine's bootstrap config (BootstrapConfigReady=Unknown, InternalError)",
        || {
            let api = api.clone();
            async move {
                let m = api.get(OTHER).await.ok()?;
                let refused = m
                    .data
                    .pointer("/status/conditions")?
                    .as_array()?
                    .iter()
                    .any(|c| {
                        c["type"] == "BootstrapConfigReady"
                            && c["status"] == "Unknown"
                            && c["reason"] == INTERNAL_ERROR_REASON
                    });
                refused.then_some(m)
            }
        },
    )
    .await;
    assert_eq!(
        refused.data.pointer("/spec/bootstrap/dataSecretName"),
        None,
        "Cluster API copied another Machine's bootstrap data to the second Machine"
    );
    let config = configs.get(VICTIM).await.expect("the victim's config");
    assert_eq!(
        controller_of(&config),
        Some(("Machine".to_string(), VICTIM.to_string())),
        "the second Machine took over the victim's bootstrap config"
    );
    let other = Api::<CloudHypervisorMachine>::namespaced(client.clone(), NS)
        .get(OTHER)
        .await
        .expect("the second InfraMachine");
    assert_eq!(
        other.spec.user_data, None,
        "the second InfraMachine received bootstrap data"
    );
}
