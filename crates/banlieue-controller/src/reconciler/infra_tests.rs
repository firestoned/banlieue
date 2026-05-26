// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::infra`].

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use banlieue_api::banlieue::{
        Architecture, DiskSpec, GuestAgent, HardwareSpec, ImagePerProviderStatus, ImageSource,
        ImageSourceKind, MigrationPolicy, NetworkInterfaceSpec, NetworkSpec, OsFamily,
        PlacementSpec, Provider, ProviderCapabilities, ProviderConnection, ProviderSpec,
        ResolvedResource, VMClass, VMClassSpec, VMImage, VMImageSpec, VMImageStatus,
        VirtualMachine, VirtualMachineSpec,
    };
    use banlieue_api::common::{
        DiskProvisioning, Firmware, IpamSource, IpamSpec, LocalObjectReference, PowerState,
    };
    use kube::core::ObjectMeta;

    use super::super::*;
    use crate::reconciler::scheduler::Decision;

    fn parent_provider() -> Provider {
        Provider {
            metadata: ObjectMeta {
                name: Some("vc1".into()),
                namespace: Some("banlieue-system".into()),
                ..Default::default()
            },
            spec: ProviderSpec {
                provider_class_ref: LocalObjectReference {
                    name: "vsphere".into(),
                },
                connection: ProviderConnection {
                    endpoint: "https://vcenter.example.com".into(),
                    credentials_ref: LocalObjectReference {
                        name: "vc1-creds".into(),
                    },
                    ca_bundle: None,
                    insecure_skip_tls_verify: false,
                },
                capabilities: ProviderCapabilities::default(),
                paused: false,
            },
            status: None,
        }
    }

    fn parent_vm() -> VirtualMachine {
        VirtualMachine {
            metadata: ObjectMeta {
                name: Some("db-01".into()),
                namespace: Some("banlieue-system".into()),
                uid: Some("uid-abc".into()),
                labels: Some(BTreeMap::from([("app".to_string(), "db-prod".to_string())])),
                generation: Some(2),
                ..Default::default()
            },
            spec: VirtualMachineSpec {
                class_ref: LocalObjectReference {
                    name: "db-prod-large".into(),
                },
                image_ref: LocalObjectReference {
                    name: "ubuntu-22.04-cloudinit".into(),
                },
                placement: PlacementSpec::default(),
                desired_power_state: PowerState::PoweredOn,
                user_data: None,
                migration_policy: MigrationPolicy::Automatic,
                paused: false,
            },
            status: None,
        }
    }

    fn parent_class() -> VMClass {
        VMClass {
            metadata: ObjectMeta {
                name: Some("db-prod-large".into()),
                ..Default::default()
            },
            spec: VMClassSpec {
                hardware: HardwareSpec {
                    cpus: 8,
                    memory_mi_b: 32_768,
                    disks: vec![DiskSpec {
                        name: "os".into(),
                        size_gi_b: 100,
                        storage_class: "gold".into(),
                        provisioning: DiskProvisioning::Thin,
                    }],
                },
                network: NetworkSpec {
                    interfaces: vec![NetworkInterfaceSpec {
                        name: "eth0".into(),
                        network_class: "prod".into(),
                        ipam: IpamSpec {
                            source: IpamSource::Dhcp,
                            static_: None,
                            pool: None,
                        },
                        mtu: None,
                    }],
                },
                firmware: Firmware::Efi,
                features: vec![],
            },
        }
    }

    fn parent_image() -> VMImage {
        VMImage {
            metadata: ObjectMeta {
                name: Some("ubuntu-22.04-cloudinit".into()),
                ..Default::default()
            },
            spec: VMImageSpec {
                os_family: OsFamily::Linux,
                os_distribution: "ubuntu".into(),
                os_version: "22.04".into(),
                architecture: Architecture::Amd64,
                guest_agent: GuestAgent::CloudInit,
                sources: vec![ImageSource {
                    provider_class: "vsphere".into(),
                    kind: ImageSourceKind::Template,
                    reference: "ubuntu-22.04-cloudinit".into(),
                    import_from: None,
                    checksum: None,
                }],
            },
            status: Some(VMImageStatus {
                per_provider: vec![ImagePerProviderStatus {
                    provider_name: "vc1".into(),
                    provider_namespace: "banlieue-system".into(),
                    ready: true,
                    resolved_ref: Some("[dc1] templates/ubuntu-22.04-cloudinit".into()),
                    reason: None,
                    message: None,
                }],
                conditions: vec![],
                observed_generation: None,
            }),
        }
    }

    fn decision_with_raw(raw: BTreeMap<String, String>) -> Decision {
        Decision {
            provider_name: "vc1".into(),
            provider_namespace: "banlieue-system".into(),
            provider_class: "vsphere".into(),
            failure_domain_name: "vc1-dc1-cluster-a".into(),
            resolved_storage: vec![ResolvedResource {
                class_name: "gold".into(),
                backend_id: "ds-fast-01".into(),
            }],
            resolved_networks: vec![ResolvedResource {
                class_name: "prod".into(),
                backend_id: "vmnet-prod".into(),
            }],
            failure_domain_raw: raw,
            failure_domain_labels: BTreeMap::new(),
        }
    }

    // ----------------------------------------------------------------------

    #[test]
    fn happy_path_populates_every_required_vsphere_field() {
        let raw = BTreeMap::from([
            ("datacenter".to_string(), "dc1".to_string()),
            ("cluster".to_string(), "cluster-a".to_string()),
        ]);
        let m = build_vsphere_machine(
            &parent_vm(),
            &parent_class(),
            &parent_image(),
            &decision_with_raw(raw),
            &parent_provider(),
        )
        .expect("ok");

        assert_eq!(m.metadata.name.as_deref(), Some("db-01"));
        assert_eq!(m.metadata.namespace.as_deref(), Some("banlieue-system"));
        assert_eq!(m.spec.datacenter, "dc1");
        assert_eq!(m.spec.cluster, "cluster-a");
        assert_eq!(m.spec.datastore, "ds-fast-01");
        assert_eq!(m.spec.template, "[dc1] templates/ubuntu-22.04-cloudinit");
        assert_eq!(m.spec.num_cpus, 8);
        assert_eq!(m.spec.memory_mi_b, 32_768);
        assert_eq!(m.spec.network.len(), 1);
        assert_eq!(m.spec.network[0].port_group, "vmnet-prod");
        assert_eq!(m.spec.disks.len(), 1);
        assert_eq!(m.spec.disks[0].name, "os");
        assert_eq!(m.spec.disks[0].size_gi_b, 100);
    }

    #[test]
    fn owner_reference_is_controller_and_blocks_owner_deletion() {
        let raw = BTreeMap::from([
            ("datacenter".to_string(), "dc1".to_string()),
            ("cluster".to_string(), "cluster-a".to_string()),
        ]);
        let m = build_vsphere_machine(
            &parent_vm(),
            &parent_class(),
            &parent_image(),
            &decision_with_raw(raw),
            &parent_provider(),
        )
        .unwrap();
        let owners = m.metadata.owner_references.expect("set");
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].kind, "VirtualMachine");
        assert_eq!(owners[0].name, "db-01");
        assert_eq!(owners[0].uid, "uid-abc");
        assert_eq!(owners[0].controller, Some(true));
        assert_eq!(owners[0].block_owner_deletion, Some(true));
    }

    #[test]
    fn missing_datacenter_raw_attribute_errors() {
        let raw = BTreeMap::from([("cluster".to_string(), "cluster-a".to_string())]);
        let err = build_vsphere_machine(
            &parent_vm(),
            &parent_class(),
            &parent_image(),
            &decision_with_raw(raw),
            &parent_provider(),
        )
        .unwrap_err();
        match err {
            InfraBuildError::MissingFdRaw(fd, attr) => {
                assert_eq!(fd, "vc1-dc1-cluster-a");
                assert_eq!(attr, "datacenter");
            }
            other => panic!("expected MissingFdRaw, got {other:?}"),
        }
    }

    #[test]
    fn missing_cluster_raw_attribute_errors() {
        let raw = BTreeMap::from([("datacenter".to_string(), "dc1".to_string())]);
        let err = build_vsphere_machine(
            &parent_vm(),
            &parent_class(),
            &parent_image(),
            &decision_with_raw(raw),
            &parent_provider(),
        )
        .unwrap_err();
        assert!(matches!(err, InfraBuildError::MissingFdRaw(_, "cluster")));
    }

    #[test]
    fn missing_image_resolved_ref_errors() {
        let raw = BTreeMap::from([
            ("datacenter".to_string(), "dc1".to_string()),
            ("cluster".to_string(), "cluster-a".to_string()),
        ]);
        let mut img = parent_image();
        if let Some(s) = img.status.as_mut() {
            s.per_provider[0].resolved_ref = None;
        }
        let err = build_vsphere_machine(
            &parent_vm(),
            &parent_class(),
            &img,
            &decision_with_raw(raw),
            &parent_provider(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            InfraBuildError::MissingResolvedImageRef { .. }
        ));
    }

    #[test]
    fn propagates_app_label_and_adds_owned_by_label() {
        let raw = BTreeMap::from([
            ("datacenter".to_string(), "dc1".to_string()),
            ("cluster".to_string(), "cluster-a".to_string()),
        ]);
        let m = build_vsphere_machine(
            &parent_vm(),
            &parent_class(),
            &parent_image(),
            &decision_with_raw(raw),
            &parent_provider(),
        )
        .unwrap();
        let labels = m.metadata.labels.expect("labels");
        assert_eq!(labels.get("app").map(String::as_str), Some("db-prod"));
        assert_eq!(
            labels.get("banlieue.io/owned-by").map(String::as_str),
            Some("db-01")
        );
    }
}
