// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `bootstrap_data.rs` (ADR-0129).
//!
//! The reconciler is a thin async shell over the pure functions tested here:
//! [`gate`] decides from the InfraMachine alone, [`unconfirmed_adoption`]
//! whether the Machine its owner reference names really owns it, [`outcome`]
//! from the Machine and the Secret lookup, and [`infra_machine_for`] maps a
//! Machine event to the InfraMachine it names. The fixtures follow Cluster API v1.14's own
//! objects (see `banlieue_api::capi`).

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::capi::{
        BOOTSTRAP_DATA_READY, BOOTSTRAP_FORMAT_KEY, BOOTSTRAP_SECRET_TYPE, BOOTSTRAP_VALUE_KEY,
        CLUSTER_NAME_LABEL, CapiObjectRef, MachineBootstrap, bootstrap_reasons,
    };
    use banlieue_api::infrastructure::{CloudHypervisorMachine, LibvirtMachine};
    use k8s_openapi::ByteString;
    use k8s_openapi::api::core::v1::Secret;
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
    use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
    use serde_json::json;

    const NS: &str = "wl";
    const CLUSTER: &str = "wl-1";
    const MACHINE: &str = "wl-1-md-0-abcde";
    const SECRET: &str = "wl-1-md-0-abcde";
    const VALUE: &str = "#cloud-config\nruncmd:\n  - echo join\n";
    const MACHINE_UID: &str = "u2";

    fn chm(owners: serde_json::Value, labels: serde_json::Value) -> CloudHypervisorMachine {
        serde_json::from_value(json!({
            "apiVersion": "infrastructure.banlieue.io/v1alpha1",
            "kind": "CloudHypervisorMachine",
            "metadata": {
                "name": MACHINE, "namespace": NS, "generation": 3,
                "ownerReferences": owners, "labels": labels
            },
            "spec": {
                "providerRef": { "name": "host-1" },
                "cpus": { "boot": 2 },
                "memory": { "sizeMiB": 4096 },
                "storageClass": "default",
                "bootSource": { "kind": "image", "image": "ubuntu.raw" },
                "osDiskSizeGiB": 20,
                "nics": []
            }
        }))
        .expect("a valid CloudHypervisorMachine")
    }

    fn machineset_created() -> CloudHypervisorMachine {
        chm(
            json!([{ "apiVersion": "cluster.x-k8s.io/v1beta2", "kind": "MachineSet",
                     "name": "wl-1-md-0", "uid": "u1" }]),
            json!({ CLUSTER_NAME_LABEL: CLUSTER }),
        )
    }

    fn adopted() -> CloudHypervisorMachine {
        chm(
            json!([{ "apiVersion": "cluster.x-k8s.io/v1beta2", "kind": "Machine",
                     "name": MACHINE, "uid": MACHINE_UID, "controller": true }]),
            json!({ CLUSTER_NAME_LABEL: CLUSTER }),
        )
    }

    fn with_condition(mut m: CloudHypervisorMachine, status: &str) -> CloudHypervisorMachine {
        m.status = Some(
            serde_json::from_value(json!({ "conditions": [{
            "type": BOOTSTRAP_DATA_READY, "status": status, "reason": "R", "message": "",
            "lastTransitionTime": "2026-11-14T00:00:00Z"
        }] }))
            .expect("status"),
        );
        m
    }

    fn machine(secret: Option<&str>) -> MachineBootstrap {
        MachineBootstrap {
            cluster_name: CLUSTER.to_string(),
            data_secret_name: secret.map(str::to_string),
            config_ref: Some(CapiObjectRef {
                api_group: "bootstrap.cluster.x-k8s.io".to_string(),
                kind: "KubeadmConfig".to_string(),
                name: MACHINE.to_string(),
            }),
            infrastructure_ref: None,
        }
    }

    fn secret(format: &str) -> Secret {
        serde_json::from_value(json!({
            "metadata": {
                "name": SECRET, "namespace": NS,
                "labels": { CLUSTER_NAME_LABEL: CLUSTER },
                "ownerReferences": [{ "apiVersion": "bootstrap.cluster.x-k8s.io/v1beta2",
                    "kind": "KubeadmConfig", "name": MACHINE, "uid": "u3", "controller": true }]
            },
            "type": BOOTSTRAP_SECRET_TYPE,
            "data": {
                BOOTSTRAP_VALUE_KEY: ByteString(VALUE.as_bytes().to_vec()),
                BOOTSTRAP_FORMAT_KEY: ByteString(format.as_bytes().to_vec())
            }
        }))
        .expect("a valid Secret")
    }

    fn reported(o: &Outcome) -> (&'static str, Option<Duration>) {
        match o {
            Outcome::Report {
                reason, requeue, ..
            } => (reason, *requeue),
            Outcome::Deliver { .. } => panic!("expected a report, got a delivery"),
        }
    }

    // ----------------------------------------------------------------------
    // gate
    // ----------------------------------------------------------------------

    #[test]
    fn a_standalone_machine_is_skipped() {
        assert_eq!(gate(&chm(json!([]), json!({}))), Gate::Skip);
    }

    #[test]
    fn a_virtualmachine_owned_machine_is_skipped() {
        let m = chm(
            json!([{ "apiVersion": "banlieue.io/v1alpha1", "kind": "VirtualMachine",
                     "name": "vm", "uid": "u", "controller": true }]),
            json!({}),
        );
        assert_eq!(gate(&m), Gate::Skip);
    }

    #[test]
    fn a_machine_before_adoption_waits_for_its_machine() {
        assert_eq!(gate(&machineset_created()), Gate::NotAdopted);
    }

    #[test]
    fn an_adopted_machine_needs_its_machine_read() {
        assert_eq!(
            gate(&adopted()),
            Gate::ReadMachine {
                name: MACHINE.to_string(),
                uid: MACHINE_UID.to_string(),
            }
        );
    }

    #[test]
    fn a_false_condition_is_retried() {
        assert_eq!(
            gate(&with_condition(adopted(), "False")),
            Gate::ReadMachine {
                name: MACHINE.to_string(),
                uid: MACHINE_UID.to_string(),
            }
        );
    }

    #[test]
    fn a_delivered_machine_is_never_rewritten() {
        assert_eq!(gate(&with_condition(adopted(), "True")), Gate::Skip);
    }

    #[test]
    fn a_machine_the_provider_has_passed_is_never_delivered_to() {
        let mut m = adopted();
        m.status = Some(serde_json::from_value(json!({ "hostUid": 70001 })).expect("status"));
        assert_eq!(gate(&m), Gate::Skip);
    }

    /// The gate and the provider's hold share `creation_started`: a machine
    /// with a `providerID` but no status (after `clusterctl move` or a
    /// restore) is past the hold, so nothing is delivered to it again.
    #[test]
    fn a_machine_with_a_provider_id_and_no_status_is_never_delivered_to() {
        let mut m = adopted();
        m.spec.provider_id = Some("cloudhypervisor://host-1/u2".to_string());
        assert_eq!(gate(&m), Gate::Skip);
    }

    #[test]
    fn a_virtualmachine_with_the_cluster_label_is_skipped() {
        let m = chm(
            json!([{ "apiVersion": "banlieue.io/v1alpha1", "kind": "VirtualMachine",
                     "name": "lb", "uid": "u", "controller": true }]),
            json!({ CLUSTER_NAME_LABEL: CLUSTER }),
        );
        assert_eq!(gate(&m), Gate::Skip);
    }

    // ----------------------------------------------------------------------
    // unreadable_machine
    // ----------------------------------------------------------------------

    #[test]
    fn a_machine_without_a_cluster_name_is_not_reported_as_missing() {
        let o = unreadable_machine(MACHINE);
        let Outcome::Report {
            reason,
            message,
            requeue,
        } = o
        else {
            panic!("expected a report");
        };
        assert_eq!(reason, bootstrap_reasons::WAITING_FOR_BOOTSTRAP_DATA);
        assert!(message.contains("spec.clusterName"), "{message}");
        assert!(!message.contains("not found"), "{message}");
        assert_eq!(requeue, None, "a change to the Machine is a watch event");
    }

    // ----------------------------------------------------------------------
    // infra_ref_of / unconfirmed_adoption: the Machine must name this
    // InfraMachine back before its Secret is read
    // ----------------------------------------------------------------------

    #[test]
    fn an_infra_machine_is_named_as_a_machines_infrastructure_ref_names_it() {
        assert_eq!(
            infra_ref_of(&adopted()),
            CapiObjectRef {
                api_group: "infrastructure.banlieue.io".to_string(),
                kind: "CloudHypervisorMachine".to_string(),
                name: MACHINE.to_string(),
            }
        );
    }

    fn naming_back() -> MachineBootstrap {
        MachineBootstrap {
            infrastructure_ref: Some(infra_ref_of(&adopted())),
            ..machine(Some(SECRET))
        }
    }

    #[test]
    fn a_machine_that_names_this_infra_machine_back_lets_the_secret_be_read() {
        assert_eq!(
            unconfirmed_adoption(&adopted(), MACHINE_UID, Some(MACHINE_UID), &naming_back()),
            None
        );
    }

    #[test]
    fn a_machine_that_names_another_infra_machine_is_refused_without_a_retry() {
        let m = MachineBootstrap {
            infrastructure_ref: Some(CapiObjectRef {
                name: "another".to_string(),
                ..infra_ref_of(&adopted())
            }),
            ..machine(Some(SECRET))
        };
        let o =
            unconfirmed_adoption(&adopted(), MACHINE_UID, Some(MACHINE_UID), &m).expect("refused");
        assert_eq!(
            reported(&o),
            (bootstrap_reasons::MACHINE_REFERENCE_MISMATCH, None)
        );
    }

    #[test]
    fn a_machine_whose_uid_differs_from_the_owner_reference_is_refused() {
        let o = unconfirmed_adoption(&adopted(), MACHINE_UID, Some("u9"), &naming_back())
            .expect("refused");
        assert_eq!(
            reported(&o).0,
            bootstrap_reasons::MACHINE_REFERENCE_MISMATCH
        );
    }

    #[test]
    fn a_mismatch_message_never_names_the_secret() {
        let o = unconfirmed_adoption(&adopted(), MACHINE_UID, Some("u9"), &naming_back())
            .expect("refused");
        let Outcome::Report { message, .. } = o else {
            panic!("expected a report");
        };
        // The Secret is the Machine's, not this InfraMachine's: nothing about
        // it is reported to whoever wrote the owner reference.
        assert!(!message.contains("Secret"), "{message}");
    }

    // ----------------------------------------------------------------------
    // outcome
    // ----------------------------------------------------------------------

    #[test]
    fn a_missing_machine_waits_on_the_watch() {
        let o = outcome(None, None);
        assert_eq!(
            reported(&o),
            (bootstrap_reasons::WAITING_FOR_BOOTSTRAP_DATA, None)
        );
    }

    #[test]
    fn a_machine_without_a_secret_name_waits_on_the_watch() {
        let m = machine(None);
        let o = outcome(Some(&m), None);
        assert_eq!(
            reported(&o),
            (bootstrap_reasons::WAITING_FOR_BOOTSTRAP_DATA, None)
        );
    }

    #[test]
    fn a_secret_not_found_is_retried_soon() {
        let m = machine(Some(SECRET));
        let o = outcome(Some(&m), Some(SecretLookup::NotFound));
        assert_eq!(
            reported(&o),
            (
                bootstrap_reasons::WAITING_FOR_BOOTSTRAP_DATA,
                Some(SECRET_NOT_FOUND_REQUEUE)
            )
        );
    }

    #[test]
    fn a_secret_created_after_not_found_is_delivered() {
        let m = machine(Some(SECRET));
        let first = outcome(Some(&m), Some(SecretLookup::NotFound));
        assert_eq!(reported(&first).1, Some(SECRET_NOT_FOUND_REQUEUE));
        let next = outcome(
            Some(&m),
            Some(SecretLookup::Found(Box::new(secret("cloud-config")))),
        );
        assert_eq!(
            next,
            Outcome::Deliver {
                value: VALUE.to_string()
            }
        );
    }

    #[test]
    fn a_forbidden_secret_waits_for_a_person() {
        let m = machine(Some(SECRET));
        let o = outcome(Some(&m), Some(SecretLookup::Forbidden));
        assert_eq!(
            reported(&o),
            (
                bootstrap_reasons::BOOTSTRAP_SECRET_FORBIDDEN,
                Some(NEEDS_A_PERSON_REQUEUE)
            )
        );
    }

    #[test]
    fn a_corrected_format_is_delivered_on_the_next_pass() {
        let m = machine(Some(SECRET));
        let first = outcome(
            Some(&m),
            Some(SecretLookup::Found(Box::new(secret("ignition")))),
        );
        assert_eq!(
            reported(&first),
            (
                bootstrap_reasons::UNSUPPORTED_BOOTSTRAP_DATA_FORMAT,
                Some(NEEDS_A_PERSON_REQUEUE)
            )
        );
        let next = outcome(
            Some(&m),
            Some(SecretLookup::Found(Box::new(secret("cloud-config")))),
        );
        assert!(matches!(next, Outcome::Deliver { .. }));
    }

    #[test]
    fn an_invalid_secret_waits_for_a_person() {
        let m = machine(Some(SECRET));
        let mut s = secret("cloud-config");
        s.type_ = Some("Opaque".to_string());
        let o = outcome(Some(&m), Some(SecretLookup::Found(Box::new(s))));
        assert_eq!(
            reported(&o),
            (
                bootstrap_reasons::BOOTSTRAP_SECRET_INVALID,
                Some(NEEDS_A_PERSON_REQUEUE)
            )
        );
    }

    #[test]
    fn the_short_retry_is_shorter_than_the_long_one() {
        assert!(SECRET_NOT_FOUND_REQUEUE < NEEDS_A_PERSON_REQUEUE);
    }

    // ----------------------------------------------------------------------
    // infra_machine_for: the Machine watch's mapper
    // ----------------------------------------------------------------------

    fn capi_machine(infra_kind: &str, infra_group: &str) -> DynamicObject {
        let gvk = GroupVersionKind::gvk("cluster.x-k8s.io", "v1beta2", "Machine");
        let mut m = DynamicObject::new(MACHINE, &ApiResource::from_gvk(&gvk)).within(NS);
        m.data = json!({ "spec": {
            "clusterName": CLUSTER,
            "bootstrap": { "dataSecretName": SECRET },
            "infrastructureRef": { "apiGroup": infra_group, "kind": infra_kind, "name": "infra-1" }
        }});
        m
    }

    #[test]
    fn a_machine_maps_to_the_infra_machine_it_names() {
        let r = infra_machine_for::<CloudHypervisorMachine>(&capi_machine(
            "CloudHypervisorMachine",
            "infrastructure.banlieue.io",
        ))
        .expect("mapped");
        assert_eq!(r.name, "infra-1");
        assert_eq!(r.namespace.as_deref(), Some(NS));
    }

    #[test]
    fn a_machine_for_another_kind_maps_to_nothing() {
        assert!(
            infra_machine_for::<LibvirtMachine>(&capi_machine(
                "CloudHypervisorMachine",
                "infrastructure.banlieue.io"
            ))
            .is_none()
        );
    }

    #[test]
    fn a_machine_for_another_providers_kind_maps_to_nothing() {
        assert!(
            infra_machine_for::<CloudHypervisorMachine>(&capi_machine(
                "CloudHypervisorMachine",
                "infrastructure.example.com"
            ))
            .is_none()
        );
    }

    // ----------------------------------------------------------------------
    // patch bodies
    // ----------------------------------------------------------------------

    #[test]
    fn the_user_data_patch_names_only_user_data() {
        let p = user_data_patch::<CloudHypervisorMachine>(VALUE);
        assert_eq!(p["apiVersion"], "infrastructure.banlieue.io/v1alpha1");
        assert_eq!(p["kind"], "CloudHypervisorMachine");
        assert_eq!(p["spec"], json!({ "userData": VALUE }));
        assert!(p.get("status").is_none());
    }

    #[test]
    fn the_condition_patch_carries_only_the_controllers_condition() {
        let m = adopted();
        let c = next_condition(
            &m,
            "True",
            bootstrap_reasons::BOOTSTRAP_DATA_DELIVERED,
            "delivered",
        );
        let p = condition_patch::<CloudHypervisorMachine>(&c);
        let conditions = p["status"]["conditions"].as_array().expect("array");
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0]["type"], BOOTSTRAP_DATA_READY);
        assert_eq!(conditions[0]["observedGeneration"], 3);
        assert_eq!(
            p["status"].as_object().expect("object").len(),
            1,
            "nothing but conditions"
        );
    }

    #[test]
    fn an_unchanged_condition_keeps_its_transition_time() {
        let m = with_condition(adopted(), "False");
        let c = next_condition(&m, "False", "Other", "msg");
        let previous: &Condition = &m.status.as_ref().expect("status").conditions[0];
        assert_eq!(c.last_transition_time, previous.last_transition_time);
        let flipped = next_condition(&m, "True", "Other", "msg");
        assert_ne!(flipped.last_transition_time, previous.last_transition_time);
    }

    #[test]
    fn an_identical_condition_needs_no_write() {
        let m = with_condition(adopted(), "False");
        let current = m.status.as_ref().expect("status").conditions[0].clone();
        assert!(!condition_changed(&m, &current));
        let mut other = current.clone();
        other.message = "changed".to_string();
        assert!(condition_changed(&m, &other));
        assert!(condition_changed(&adopted(), &current));
    }

    // ----------------------------------------------------------------------
    // crd_serves_machines: what turns the reconciler on
    // ----------------------------------------------------------------------

    fn machine_crd(
        name: &str,
        version: &str,
        served: bool,
        established: &str,
    ) -> CustomResourceDefinition {
        serde_json::from_value(json!({
            "apiVersion": "apiextensions.k8s.io/v1",
            "kind": "CustomResourceDefinition",
            "metadata": { "name": name },
            "spec": {
                "group": "cluster.x-k8s.io",
                "names": { "kind": "Machine", "plural": "machines" },
                "scope": "Namespaced",
                "versions": [
                    { "name": "v1beta1", "served": true, "storage": false },
                    { "name": version, "served": served, "storage": true }
                ]
            },
            "status": { "conditions": [
                { "type": "Established", "status": established, "reason": "R", "message": "",
                  "lastTransitionTime": "2026-11-15T00:00:00Z" }
            ] }
        }))
        .expect("a valid CustomResourceDefinition")
    }

    #[test]
    fn an_established_crd_serving_v1beta2_turns_the_reconciler_on() {
        assert!(crd_serves_machines(&machine_crd(
            MACHINE_CRD,
            "v1beta2",
            true,
            "True"
        )));
    }

    #[test]
    fn a_crd_not_yet_established_leaves_it_off() {
        assert!(!crd_serves_machines(&machine_crd(
            MACHINE_CRD,
            "v1beta2",
            true,
            "False"
        )));
        let mut no_status = machine_crd(MACHINE_CRD, "v1beta2", true, "True");
        no_status.status = None;
        assert!(!crd_serves_machines(&no_status));
    }

    #[test]
    fn a_crd_without_a_served_v1beta2_leaves_it_off() {
        // An older Cluster API serves only v1beta1, which has no apiGroup on
        // its references.
        assert!(!crd_serves_machines(&machine_crd(
            MACHINE_CRD,
            "v1beta2",
            false,
            "True"
        )));
        assert!(!crd_serves_machines(&machine_crd(
            MACHINE_CRD,
            "v1alpha4",
            true,
            "True"
        )));
    }

    #[test]
    fn another_crd_never_turns_it_on() {
        assert!(!crd_serves_machines(&machine_crd(
            "machines.infrastructure.cluster.x-k8s.io",
            "v1beta2",
            true,
            "True"
        )));
    }
}
