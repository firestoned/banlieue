// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `capi.rs` (ADR-0129).
//!
//! The fixtures follow what Cluster API v1.14 actually writes: an
//! InfraMachine as `external.GenerateTemplate` creates it (owned by its
//! MachineSet or control plane, labelled with the cluster name), the same
//! object after the Machine controller adopts it, and a bootstrap Secret as
//! CABPK's `storeBootstrapData` and k0smotron's `createBootstrapSecret` write
//! it.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::banlieue::VirtualMachine;
    use crate::infrastructure::{CloudHypervisorMachine, LibvirtMachine};
    use k8s_openapi::ByteString;
    use k8s_openapi::api::core::v1::Secret;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{
        Condition, ObjectMeta, OwnerReference, Time,
    };
    use kube::Resource;
    use serde_json::json;
    use std::collections::BTreeMap;

    const CLUSTER: &str = "wl-1";
    const MACHINE: &str = "wl-1-md-0-abcde";
    const CONFIG: &str = "wl-1-md-0-abcde";
    const SECRET: &str = "wl-1-md-0-abcde";
    const CLOUD_CONFIG: &str = "#cloud-config\nruncmd:\n  - echo join\n";

    fn owner(api_version: &str, kind: &str, name: &str, controller: bool) -> OwnerReference {
        OwnerReference {
            api_version: api_version.to_string(),
            kind: kind.to_string(),
            name: name.to_string(),
            uid: format!("uid-{name}"),
            controller: Some(controller),
            block_owner_deletion: None,
        }
    }

    fn meta(owners: Vec<OwnerReference>, labels: &[(&str, &str)]) -> ObjectMeta {
        ObjectMeta {
            name: Some(MACHINE.to_string()),
            namespace: Some("ns".to_string()),
            owner_references: (!owners.is_empty()).then_some(owners),
            labels: (!labels.is_empty()).then(|| {
                labels
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect()
            }),
            ..Default::default()
        }
    }

    /// An InfraMachine as a MachineSet creates it: MachineSet owner, label.
    fn as_machineset_created() -> ObjectMeta {
        meta(
            vec![owner(
                "cluster.x-k8s.io/v1beta2",
                "MachineSet",
                "wl-1-md-0",
                false,
            )],
            &[(CLUSTER_NAME_LABEL, CLUSTER)],
        )
    }

    /// The same InfraMachine after the Machine controller adopts it.
    fn as_adopted() -> ObjectMeta {
        meta(
            vec![owner("cluster.x-k8s.io/v1beta2", "Machine", MACHINE, true)],
            &[(CLUSTER_NAME_LABEL, CLUSTER)],
        )
    }

    fn condition(type_: &str, status: &str) -> Condition {
        Condition {
            type_: type_.to_string(),
            status: status.to_string(),
            reason: "R".to_string(),
            message: String::new(),
            observed_generation: None,
            last_transition_time: Time(k8s_openapi::jiff::Timestamp::UNIX_EPOCH),
        }
    }

    fn machine() -> MachineBootstrap {
        MachineBootstrap {
            cluster_name: CLUSTER.to_string(),
            data_secret_name: Some(SECRET.to_string()),
            config_ref: Some(CapiObjectRef {
                api_group: "bootstrap.cluster.x-k8s.io".to_string(),
                kind: "KubeadmConfig".to_string(),
                name: CONFIG.to_string(),
            }),
            infrastructure_ref: None,
        }
    }

    /// A bootstrap Secret exactly as CABPK writes it.
    fn cabpk_secret() -> Secret {
        let mut data = BTreeMap::new();
        data.insert(
            BOOTSTRAP_VALUE_KEY.to_string(),
            ByteString(CLOUD_CONFIG.as_bytes().to_vec()),
        );
        data.insert(
            BOOTSTRAP_FORMAT_KEY.to_string(),
            ByteString(FORMAT_CLOUD_CONFIG.as_bytes().to_vec()),
        );
        Secret {
            metadata: ObjectMeta {
                name: Some(SECRET.to_string()),
                namespace: Some("ns".to_string()),
                labels: Some(BTreeMap::from([(
                    CLUSTER_NAME_LABEL.to_string(),
                    CLUSTER.to_string(),
                )])),
                owner_references: Some(vec![owner(
                    "bootstrap.cluster.x-k8s.io/v1beta2",
                    "KubeadmConfig",
                    CONFIG,
                    true,
                )]),
                ..Default::default()
            },
            data: Some(data),
            type_: Some(BOOTSTRAP_SECRET_TYPE.to_string()),
            ..Default::default()
        }
    }

    fn rejected(secret: &Secret, m: &MachineBootstrap) -> &'static str {
        bootstrap_value(secret, m)
            .expect_err("must be refused")
            .reason
    }

    // ----------------------------------------------------------------------
    // api_group / is_capi_group
    // ----------------------------------------------------------------------

    #[test]
    fn api_group_splits_group_from_version() {
        assert_eq!(api_group("cluster.x-k8s.io/v1beta2"), "cluster.x-k8s.io");
        assert_eq!(api_group("v1"), "");
    }

    #[test]
    fn capi_groups_include_the_providers_subgroups() {
        assert!(is_capi_group("cluster.x-k8s.io"));
        assert!(is_capi_group("controlplane.cluster.x-k8s.io"));
        assert!(is_capi_group("bootstrap.cluster.x-k8s.io"));
        assert!(!is_capi_group("banlieue.io"));
        assert!(!is_capi_group("evilcluster.x-k8s.io"));
        assert!(!is_capi_group(""));
    }

    // ----------------------------------------------------------------------
    // is_capi_intended / owning_machine
    // ----------------------------------------------------------------------

    #[test]
    fn a_machine_as_capi_creates_it_is_intended_but_not_owned() {
        let m = as_machineset_created();
        assert!(is_capi_intended(&m));
        assert!(owning_machine(&m).is_none());
    }

    #[test]
    fn a_control_plane_owner_alone_makes_a_machine_intended() {
        let m = meta(
            vec![owner(
                "controlplane.cluster.x-k8s.io/v1beta2",
                "KubeadmControlPlane",
                "wl-1-cp",
                false,
            )],
            &[],
        );
        assert!(is_capi_intended(&m));
    }

    #[test]
    fn the_cluster_name_label_alone_makes_a_machine_intended() {
        assert!(is_capi_intended(&meta(
            vec![],
            &[(CLUSTER_NAME_LABEL, CLUSTER)]
        )));
    }

    #[test]
    fn an_adopted_machine_is_owned_by_its_machine() {
        let m = as_adopted();
        assert!(is_capi_intended(&m));
        assert_eq!(owning_machine(&m).map(|o| o.name.as_str()), Some(MACHINE));
    }

    #[test]
    fn a_virtualmachine_owned_machine_is_not_intended() {
        let m = meta(
            vec![owner(
                &VirtualMachine::api_version(&()),
                "VirtualMachine",
                "vm-1",
                true,
            )],
            &[],
        );
        assert!(!is_capi_intended(&m));
        assert!(owning_machine(&m).is_none());
    }

    #[test]
    fn a_virtualmachine_owned_machine_with_the_cluster_label_is_not_intended() {
        // banlieue copies a VirtualMachine's labels onto its machine, so a VM
        // labelled with a cluster's name (a load balancer for it, a kustomize
        // commonLabel) must not wait for bootstrap data no Machine sends.
        let m = meta(
            vec![owner(
                &VirtualMachine::api_version(&()),
                "VirtualMachine",
                "lb-1",
                true,
            )],
            &[(CLUSTER_NAME_LABEL, CLUSTER)],
        );
        assert!(!is_capi_intended(&m));
        assert!(!holds_creation(&m, &[]));
    }

    #[test]
    fn a_virtualmachine_reference_that_is_not_the_controller_does_not_exempt() {
        let m = meta(
            vec![owner(
                &VirtualMachine::api_version(&()),
                "VirtualMachine",
                "vm-1",
                false,
            )],
            &[(CLUSTER_NAME_LABEL, CLUSTER)],
        );
        assert!(is_capi_intended(&m));
    }

    #[test]
    fn a_virtualmachine_kind_in_another_group_does_not_exempt() {
        let m = meta(
            vec![owner("example.com/v1", "VirtualMachine", "vm-1", true)],
            &[(CLUSTER_NAME_LABEL, CLUSTER)],
        );
        assert!(is_capi_intended(&m));
    }

    #[test]
    fn only_the_controller_machine_reference_is_the_owning_machine() {
        // A stale, non-controller Machine reference listed first must not be
        // taken for the Machine that adopted this one.
        let m = meta(
            vec![
                owner("cluster.x-k8s.io/v1beta2", "Machine", "stale", false),
                owner("cluster.x-k8s.io/v1beta2", "Machine", MACHINE, true),
            ],
            &[(CLUSTER_NAME_LABEL, CLUSTER)],
        );
        assert_eq!(owning_machine(&m).map(|o| o.name.as_str()), Some(MACHINE));
    }

    #[test]
    fn a_machine_reference_that_is_not_the_controller_is_not_adoption() {
        let m = meta(
            vec![owner("cluster.x-k8s.io/v1beta2", "Machine", MACHINE, false)],
            &[(CLUSTER_NAME_LABEL, CLUSTER)],
        );
        assert!(owning_machine(&m).is_none());
        assert!(is_capi_intended(&m));
        assert!(holds_creation(
            &m,
            &[condition(BOOTSTRAP_DATA_READY, "True")]
        ));
    }

    #[test]
    fn a_standalone_machine_is_not_intended() {
        assert!(!is_capi_intended(&meta(vec![], &[])));
    }

    #[test]
    fn a_machine_kind_in_another_group_is_not_a_capi_machine() {
        let m = meta(vec![owner("example.com/v1", "Machine", "m", true)], &[]);
        assert!(owning_machine(&m).is_none());
        assert!(!is_capi_intended(&m));
    }

    // ----------------------------------------------------------------------
    // holds_creation / is_bootstrap_data_ready
    // ----------------------------------------------------------------------

    #[test]
    fn a_machine_as_capi_creates_it_is_held() {
        assert!(holds_creation(&as_machineset_created(), &[]));
    }

    #[test]
    fn an_adopted_machine_without_the_condition_is_held() {
        assert!(holds_creation(&as_adopted(), &[]));
    }

    #[test]
    fn an_adopted_machine_with_the_condition_false_is_held() {
        assert!(holds_creation(
            &as_adopted(),
            &[condition(BOOTSTRAP_DATA_READY, "False")]
        ));
    }

    #[test]
    fn an_adopted_machine_with_its_data_delivered_is_not_held() {
        assert!(!holds_creation(
            &as_adopted(),
            &[condition(BOOTSTRAP_DATA_READY, "True")]
        ));
    }

    #[test]
    fn a_true_condition_before_adoption_still_holds() {
        assert!(holds_creation(
            &as_machineset_created(),
            &[condition(BOOTSTRAP_DATA_READY, "True")]
        ));
    }

    #[test]
    fn standalone_and_virtualmachine_machines_are_never_held() {
        assert!(!holds_creation(&meta(vec![], &[]), &[]));
        let vm_owned = meta(
            vec![owner("banlieue.io/v1alpha1", "VirtualMachine", "vm", true)],
            &[],
        );
        assert!(!holds_creation(&vm_owned, &[]));
    }

    // ----------------------------------------------------------------------
    // without_controller_conditions
    // ----------------------------------------------------------------------

    #[test]
    fn providers_drop_only_the_controllers_condition() {
        let mut conditions = vec![
            condition("Ready", "True"),
            condition(BOOTSTRAP_DATA_READY, "True"),
            condition("GuestReady", "False"),
        ];
        retain_provider_conditions(&mut conditions);
        let types: Vec<_> = conditions.iter().map(|c| c.type_.as_str()).collect();
        assert_eq!(types, ["Ready", "GuestReady"]);
    }

    // ----------------------------------------------------------------------
    // MachineBootstrap::from_machine
    // ----------------------------------------------------------------------

    #[test]
    fn parses_a_v1beta2_machine() {
        let body = json!({
            "spec": {
                "clusterName": CLUSTER,
                "bootstrap": {
                    "configRef": {
                        "apiGroup": "bootstrap.cluster.x-k8s.io",
                        "kind": "KubeadmConfig",
                        "name": CONFIG
                    },
                    "dataSecretName": SECRET
                },
                "infrastructureRef": {
                    "apiGroup": "infrastructure.banlieue.io",
                    "kind": "CloudHypervisorMachine",
                    "name": MACHINE
                }
            }
        });
        let m = MachineBootstrap::from_machine(&body).expect("parses");
        assert_eq!(m.cluster_name, CLUSTER);
        assert_eq!(m.data_secret_name.as_deref(), Some(SECRET));
        assert_eq!(m.config_ref, machine().config_ref);
        assert_eq!(
            m.infrastructure_ref,
            Some(CapiObjectRef {
                api_group: "infrastructure.banlieue.io".to_string(),
                kind: "CloudHypervisorMachine".to_string(),
                name: MACHINE.to_string(),
            })
        );
    }

    #[test]
    fn a_machine_still_waiting_has_no_secret_name() {
        let body = json!({ "spec": { "clusterName": CLUSTER, "bootstrap": { "configRef": {
            "apiGroup": "bootstrap.cluster.x-k8s.io", "kind": "KubeadmConfig", "name": CONFIG } } } });
        let m = MachineBootstrap::from_machine(&body).expect("parses");
        assert_eq!(m.data_secret_name, None);
    }

    #[test]
    fn an_empty_secret_name_counts_as_unset() {
        let body =
            json!({ "spec": { "clusterName": CLUSTER, "bootstrap": { "dataSecretName": "" } } });
        let m = MachineBootstrap::from_machine(&body).expect("parses");
        assert_eq!(m.data_secret_name, None);
    }

    #[test]
    fn a_machine_without_a_cluster_name_does_not_parse() {
        assert!(MachineBootstrap::from_machine(&json!({ "spec": {} })).is_none());
    }

    #[test]
    fn an_incomplete_ref_is_no_ref() {
        let body = json!({ "spec": { "clusterName": CLUSTER, "bootstrap": {
            "configRef": { "kind": "KubeadmConfig" } } } });
        assert_eq!(
            MachineBootstrap::from_machine(&body)
                .expect("parses")
                .config_ref,
            None
        );
    }

    // ----------------------------------------------------------------------
    // machine_confirms_adoption: an ownerReference alone proves nothing
    // ----------------------------------------------------------------------

    const MACHINE_UID: &str = "uid-machine";

    fn this_infra_machine() -> CapiObjectRef {
        CapiObjectRef {
            api_group: "infrastructure.banlieue.io".to_string(),
            kind: "CloudHypervisorMachine".to_string(),
            name: MACHINE.to_string(),
        }
    }

    fn naming(infra: CapiObjectRef) -> MachineBootstrap {
        MachineBootstrap {
            infrastructure_ref: Some(infra),
            ..machine()
        }
    }

    #[test]
    fn the_machine_that_names_this_infra_machine_back_confirms_it() {
        let m = naming(this_infra_machine());
        assert!(machine_confirms_adoption(
            Some(MACHINE_UID),
            &m,
            MACHINE_UID,
            &this_infra_machine()
        ));
    }

    #[test]
    fn a_machine_naming_another_infra_machine_does_not_confirm() {
        let m = naming(CapiObjectRef {
            name: "someone-elses-machine".to_string(),
            ..this_infra_machine()
        });
        assert!(!machine_confirms_adoption(
            Some(MACHINE_UID),
            &m,
            MACHINE_UID,
            &this_infra_machine()
        ));
    }

    #[test]
    fn a_machine_naming_another_kind_or_group_does_not_confirm() {
        let other_kind = naming(CapiObjectRef {
            kind: "LibvirtMachine".to_string(),
            ..this_infra_machine()
        });
        let other_group = naming(CapiObjectRef {
            api_group: "infrastructure.cluster.x-k8s.io".to_string(),
            ..this_infra_machine()
        });
        for m in [other_kind, other_group] {
            assert!(!machine_confirms_adoption(
                Some(MACHINE_UID),
                &m,
                MACHINE_UID,
                &this_infra_machine()
            ));
        }
    }

    #[test]
    fn a_machine_naming_no_infra_machine_does_not_confirm() {
        assert!(!machine_confirms_adoption(
            Some(MACHINE_UID),
            &machine(),
            MACHINE_UID,
            &this_infra_machine()
        ));
    }

    #[test]
    fn a_machine_with_another_uid_does_not_confirm() {
        // Same name, different object: the owner reference names a Machine
        // that was deleted and recreated, or was never this one.
        let m = naming(this_infra_machine());
        assert!(!machine_confirms_adoption(
            Some("uid-other"),
            &m,
            MACHINE_UID,
            &this_infra_machine()
        ));
        assert!(!machine_confirms_adoption(
            None,
            &m,
            MACHINE_UID,
            &this_infra_machine()
        ));
    }

    // ----------------------------------------------------------------------
    // bootstrap_value: the five checks of ADR-0129 Decision 4
    // ----------------------------------------------------------------------

    #[test]
    fn a_cabpk_secret_is_copied_byte_for_byte() {
        assert_eq!(
            bootstrap_value(&cabpk_secret(), &machine()).expect("accepted"),
            CLOUD_CONFIG
        );
    }

    #[test]
    fn a_missing_format_means_cloud_config() {
        let mut s = cabpk_secret();
        s.data.as_mut().expect("data").remove(BOOTSTRAP_FORMAT_KEY);
        assert!(bootstrap_value(&s, &machine()).is_ok());
    }

    #[test]
    fn an_empty_format_means_cloud_config() {
        // CABPK renders cloud-config for an unset format, and a bootstrap
        // provider that writes the key empty means the same.
        let mut s = cabpk_secret();
        s.data
            .as_mut()
            .expect("data")
            .insert(BOOTSTRAP_FORMAT_KEY.to_string(), ByteString(Vec::new()));
        assert_eq!(bootstrap_value(&s, &machine()).as_deref(), Ok(CLOUD_CONFIG));
    }

    #[test]
    fn a_secret_of_another_type_is_refused() {
        let mut s = cabpk_secret();
        s.type_ = Some("Opaque".to_string());
        assert_eq!(
            rejected(&s, &machine()),
            bootstrap_reasons::BOOTSTRAP_SECRET_INVALID
        );
        s.type_ = None;
        assert_eq!(
            rejected(&s, &machine()),
            bootstrap_reasons::BOOTSTRAP_SECRET_INVALID
        );
    }

    #[test]
    fn a_secret_for_another_cluster_is_refused() {
        let mut s = cabpk_secret();
        s.metadata.labels = Some(BTreeMap::from([(
            CLUSTER_NAME_LABEL.to_string(),
            "other".to_string(),
        )]));
        assert_eq!(
            rejected(&s, &machine()),
            bootstrap_reasons::BOOTSTRAP_SECRET_INVALID
        );
        s.metadata.labels = None;
        assert_eq!(
            rejected(&s, &machine()),
            bootstrap_reasons::BOOTSTRAP_SECRET_INVALID
        );
    }

    #[test]
    fn a_secret_not_written_by_the_machines_config_is_refused() {
        let mut s = cabpk_secret();
        s.metadata.owner_references = Some(vec![owner(
            "bootstrap.cluster.x-k8s.io/v1beta2",
            "KubeadmConfig",
            "another-machines-config",
            true,
        )]);
        assert_eq!(
            rejected(&s, &machine()),
            bootstrap_reasons::BOOTSTRAP_SECRET_INVALID
        );
    }

    #[test]
    fn a_non_controller_owner_does_not_count() {
        let mut s = cabpk_secret();
        s.metadata.owner_references = Some(vec![owner(
            "bootstrap.cluster.x-k8s.io/v1beta2",
            "KubeadmConfig",
            CONFIG,
            false,
        )]);
        assert_eq!(
            rejected(&s, &machine()),
            bootstrap_reasons::BOOTSTRAP_SECRET_INVALID
        );
    }

    #[test]
    fn the_owner_must_match_the_config_refs_group_and_kind() {
        let mut s = cabpk_secret();
        s.metadata.owner_references = Some(vec![owner(
            "bootstrap.cluster.x-k8s.io/v1beta2",
            "K0sWorkerConfig",
            CONFIG,
            true,
        )]);
        assert_eq!(
            rejected(&s, &machine()),
            bootstrap_reasons::BOOTSTRAP_SECRET_INVALID
        );
    }

    #[test]
    fn a_k0smotron_secret_matches_its_k0s_worker_config() {
        let mut m = machine();
        m.config_ref = Some(CapiObjectRef {
            api_group: "bootstrap.cluster.x-k8s.io".to_string(),
            kind: "K0sWorkerConfig".to_string(),
            name: CONFIG.to_string(),
        });
        let mut s = cabpk_secret();
        s.metadata.owner_references = Some(vec![owner(
            "bootstrap.cluster.x-k8s.io/v1beta1",
            "K0sWorkerConfig",
            CONFIG,
            true,
        )]);
        assert!(bootstrap_value(&s, &m).is_ok());
    }

    #[test]
    fn without_a_config_ref_ownership_is_not_checked() {
        let mut m = machine();
        m.config_ref = None;
        let mut s = cabpk_secret();
        s.metadata.owner_references = None;
        assert!(bootstrap_value(&s, &m).is_ok());
    }

    #[test]
    fn an_empty_or_missing_value_is_refused() {
        let mut s = cabpk_secret();
        s.data
            .as_mut()
            .expect("data")
            .insert(BOOTSTRAP_VALUE_KEY.to_string(), ByteString(Vec::new()));
        assert_eq!(
            rejected(&s, &machine()),
            bootstrap_reasons::BOOTSTRAP_SECRET_INVALID
        );
        s.data.as_mut().expect("data").remove(BOOTSTRAP_VALUE_KEY);
        assert_eq!(
            rejected(&s, &machine()),
            bootstrap_reasons::BOOTSTRAP_SECRET_INVALID
        );
        s.data = None;
        assert_eq!(
            rejected(&s, &machine()),
            bootstrap_reasons::BOOTSTRAP_SECRET_INVALID
        );
    }

    #[test]
    fn a_value_that_is_not_utf8_is_refused() {
        let mut s = cabpk_secret();
        s.data.as_mut().expect("data").insert(
            BOOTSTRAP_VALUE_KEY.to_string(),
            ByteString(vec![0xff, 0xfe]),
        );
        assert_eq!(
            rejected(&s, &machine()),
            bootstrap_reasons::BOOTSTRAP_SECRET_INVALID
        );
    }

    #[test]
    fn every_format_but_cloud_config_is_unsupported() {
        for format in ["ignition", "powershell", "powershell-xml", "Cloud-Config"] {
            let mut s = cabpk_secret();
            s.data.as_mut().expect("data").insert(
                BOOTSTRAP_FORMAT_KEY.to_string(),
                ByteString(format.as_bytes().to_vec()),
            );
            let err = bootstrap_value(&s, &machine()).expect_err(format);
            assert_eq!(
                err.reason,
                bootstrap_reasons::UNSUPPORTED_BOOTSTRAP_DATA_FORMAT
            );
            assert!(err.message.contains(format), "{}", err.message);
        }
    }

    #[test]
    fn a_rejection_message_never_carries_the_value() {
        let mut s = cabpk_secret();
        s.type_ = Some("Opaque".to_string());
        let err = bootstrap_value(&s, &machine()).expect_err("refused");
        assert!(!err.message.contains("echo join"), "{}", err.message);
    }

    // ----------------------------------------------------------------------
    // BootstrapTarget
    // ----------------------------------------------------------------------

    #[test]
    fn the_target_trait_reads_a_cloud_hypervisor_machine() {
        let mut chm: CloudHypervisorMachine = serde_json::from_value(json!({
            "apiVersion": "infrastructure.banlieue.io/v1alpha1",
            "kind": "CloudHypervisorMachine",
            "metadata": { "name": MACHINE, "namespace": "ns" },
            "spec": {
                "providerRef": { "name": "host-1" },
                "cpus": { "boot": 2 },
                "memory": { "sizeMiB": 4096 },
                "storageClass": "default",
                "bootSource": { "kind": "image", "image": "ubuntu.raw" },
                "osDiskSizeGiB": 20,
                "nics": [],
                "userData": CLOUD_CONFIG
            },
            "status": { "conditions": [] }
        }))
        .expect("a valid CloudHypervisorMachine");
        assert_eq!(chm.user_data(), Some(CLOUD_CONFIG));
        assert_eq!(chm.provider_id(), None);
        assert!(chm.conditions().is_empty());
        chm.status = None;
        assert!(chm.conditions().is_empty());
    }

    // ----------------------------------------------------------------------
    // creation_started / holds_guest: one predicate for both sides
    // ----------------------------------------------------------------------

    fn cloud_hypervisor(meta: ObjectMeta) -> CloudHypervisorMachine {
        let mut m: CloudHypervisorMachine = serde_json::from_value(json!({
            "apiVersion": "infrastructure.banlieue.io/v1alpha1",
            "kind": "CloudHypervisorMachine",
            "metadata": { "name": MACHINE, "namespace": "ns" },
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
        .expect("a valid CloudHypervisorMachine");
        m.metadata = meta;
        m
    }

    fn libvirt(meta: ObjectMeta) -> LibvirtMachine {
        let mut m: LibvirtMachine = serde_json::from_value(json!({
            "apiVersion": "infrastructure.banlieue.io/v1alpha1",
            "kind": "LibvirtMachine",
            "metadata": { "name": MACHINE, "namespace": "ns" },
            "spec": {
                "providerRef": { "name": "host-1" },
                "pool": "default",
                "domainName": MACHINE,
                "bootSource": { "kind": "backingVolume", "volume": "ubuntu.qcow2" },
                "vcpus": 2,
                "memoryMiB": 4096,
                "firmware": "efi",
                "disks": [],
                "network": []
            }
        }))
        .expect("a valid LibvirtMachine");
        m.metadata = meta;
        m
    }

    #[test]
    fn a_cloud_hypervisor_machine_has_started_once_it_has_a_host_uid() {
        let mut m = cloud_hypervisor(as_adopted());
        assert!(!m.creation_started());
        m.status = Some(serde_json::from_value(json!({ "hostUid": 70001 })).expect("status"));
        assert!(m.creation_started());
    }

    #[test]
    fn a_cloud_hypervisor_machine_with_a_provider_id_has_started() {
        // `clusterctl move` or a restore keeps spec.providerID and loses the
        // status, host uid and BootstrapDataReady with it: the running
        // machine must not be held again, or delivered to again.
        let mut m = cloud_hypervisor(as_adopted());
        m.spec.provider_id = Some("cloudhypervisor://host-1/x".to_string());
        assert!(m.status.is_none());
        assert!(m.creation_started());
        assert!(!holds_guest(&m));
    }

    #[test]
    fn a_backend_without_a_hold_has_started_once_it_has_a_provider_id() {
        let mut m = libvirt(as_adopted());
        assert!(!m.creation_started());
        m.spec.provider_id = Some("libvirt://host-1/x".to_string());
        assert!(m.creation_started());
    }

    #[test]
    fn a_started_machine_is_never_held() {
        let mut m = cloud_hypervisor(as_adopted());
        assert!(holds_guest(&m));
        m.status = Some(serde_json::from_value(json!({ "hostUid": 70001 })).expect("status"));
        assert!(!holds_guest(&m));
    }

    #[test]
    fn a_delivered_machine_is_not_held() {
        let mut m = cloud_hypervisor(as_adopted());
        m.status = Some(
            serde_json::from_value(json!({ "conditions": [{
                "type": BOOTSTRAP_DATA_READY, "status": "True", "reason": "R", "message": "",
                "lastTransitionTime": "2026-11-15T00:00:00Z" }] }))
            .expect("status"),
        );
        assert!(!holds_guest(&m));
    }
}
