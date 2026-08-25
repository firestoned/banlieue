// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::infra`].

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use banlieue_api::banlieue::{
        Architecture, DiskOverride, DiskSpec, GuestAgent, HardwareOverride, HardwareSpec,
        ImagePerProviderStatus, ImageSource, ImageSourceKind, MigrationPolicy,
        NetworkInterfaceOverride, NetworkInterfaceSpec, NetworkSpec, OsFamily, PlacementSpec,
        Provider, ProviderCapabilities, ProviderConnection, ProviderSpec, ResolvedResource,
        SubnetShape, VMClass, VMClassSpec, VMImage, VMImageSpec, VMImageStatus, VirtualMachine,
        VirtualMachineSpec, ZoneImageStatus,
    };
    use banlieue_api::common::{
        DiskProvisioning, Firmware, IpamShape, IpamSource, LocalObjectReference, PowerState,
        StaticIpamConfig,
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
                use_content_library: false,
                failure_domain_name_overrides: Vec::new(),
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
                network_overrides: Vec::new(),
                hardware_override: None,
                folder: None,
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
                        ipam: IpamShape::default(),
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
                cloud_config: None,
                template: None,
                iso_overlay: None,
            },
            status: Some(VMImageStatus {
                per_provider: vec![ImagePerProviderStatus {
                    provider_name: "vc1".into(),
                    provider_namespace: "banlieue-system".into(),
                    ready: true,
                    resolved_ref: Some("[dc1] templates/ubuntu-22.04-cloudinit".into()),
                    reason: None,
                    message: None,
                    zones: vec![],
                }],
                build_artifact: None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
        )
        .unwrap();
        let labels = m.metadata.labels.expect("labels");
        assert_eq!(labels.get("app").map(String::as_str), Some("db-prod"));
        assert_eq!(
            labels.get("banlieue.io/owned-by").map(String::as_str),
            Some("db-01")
        );
    }

    // ----------------------------------------------------------------------
    // network_overrides -> VSphereNicSpec.ipam (ADR-0024)
    // ----------------------------------------------------------------------

    fn static_override(name: &str, address: &str) -> NetworkInterfaceOverride {
        NetworkInterfaceOverride {
            name: name.to_string(),
            static_: StaticIpamConfig {
                address: address.to_string(),
                prefix: 24,
                gateway: Some("10.0.0.1".to_string()),
                nameservers: vec!["10.0.1.53".to_string()],
                domain: Some("k8s.example.internal".to_string()),
            },
        }
    }

    #[test]
    fn merge_ipam_override_none_returns_class_ipam_unchanged() {
        let class_ipam = IpamShape::default();
        let merged = merge_ipam_override(&class_ipam, None, None);
        assert_eq!(merged.source(), IpamSource::Dhcp);
        assert!(merged.static_.is_none());
        assert!(merged.pool.is_none());
    }

    #[test]
    fn merge_ipam_override_some_replaces_class_ipam_with_static() {
        let class_ipam = IpamShape::default();
        let over = static_override("eth0", "10.0.0.90");
        let merged = merge_ipam_override(&class_ipam, Some(&over.static_), None);
        assert_eq!(merged.source(), IpamSource::Static);
        assert_eq!(merged.static_.as_ref().unwrap().address, "10.0.0.90");
        assert!(merged.pool.is_none());
    }

    // ----------------------------------------------------------------------
    // merge_ipam_override + zone_subnet (ADR-0032)
    // ----------------------------------------------------------------------

    fn override_missing_subnet_fields(address: &str) -> StaticIpamConfig {
        // Only address/prefix set — the exact shape a static-addressing VM
        // needs to supply once gateway/nameservers/domain are zone-derived.
        StaticIpamConfig {
            address: address.to_string(),
            prefix: 24,
            gateway: None,
            nameservers: Vec::new(),
            domain: None,
        }
    }

    fn override_missing_subnet_fields_named(name: &str, address: &str) -> NetworkInterfaceOverride {
        NetworkInterfaceOverride {
            name: name.to_string(),
            static_: override_missing_subnet_fields(address),
        }
    }

    #[test]
    fn merge_ipam_override_fills_gateway_nameservers_domain_from_the_zone_subnet() {
        let class_ipam = IpamShape::default();
        let over = override_missing_subnet_fields("10.0.0.104");
        let zone_subnet = SubnetShape {
            gateway: Some("10.0.0.1".to_string()),
            nameservers: vec!["10.0.0.53".to_string(), "10.0.0.54".to_string()],
            domain: Some("k8s.example.internal".to_string()),
        };
        let merged = merge_ipam_override(&class_ipam, Some(&over), Some(&zone_subnet));
        let static_ = merged.static_.expect("static IPAM present");
        assert_eq!(static_.address, "10.0.0.104");
        assert_eq!(static_.prefix, 24, "prefix always comes from the override");
        assert_eq!(static_.gateway.as_deref(), Some("10.0.0.1"));
        assert_eq!(
            static_.nameservers,
            vec!["10.0.0.53".to_string(), "10.0.0.54".to_string()]
        );
        assert_eq!(static_.domain.as_deref(), Some("k8s.example.internal"));
    }

    #[test]
    fn merge_ipam_override_never_overwrites_an_explicit_per_vm_value() {
        // The VM's own gateway/nameservers/domain win outright, field by
        // field, even when a zone subnet is also available — explicit over
        // implicit.
        let class_ipam = IpamShape::default();
        let over = static_override("eth0", "10.0.0.90"); // full StaticIpamConfig, all fields set
        let zone_subnet = SubnetShape {
            gateway: Some("10.9.9.1".to_string()),
            nameservers: vec!["10.9.9.53".to_string()],
            domain: Some("zone.internal".to_string()),
        };
        let merged = merge_ipam_override(&class_ipam, Some(&over.static_), Some(&zone_subnet));
        let static_ = merged.static_.expect("static IPAM present");
        assert_eq!(static_.gateway.as_deref(), Some("10.0.0.1"));
        assert_eq!(static_.nameservers, vec!["10.0.1.53".to_string()]);
        assert_eq!(static_.domain.as_deref(), Some("k8s.example.internal"));
    }

    #[test]
    fn merge_ipam_override_with_no_zone_subnet_leaves_gaps_empty() {
        // No Provider-side subnet declared for this class/zone at all —
        // falls back to today's behavior (gaps stay empty), not an error.
        let class_ipam = IpamShape::default();
        let over = override_missing_subnet_fields("10.0.0.90");
        let merged = merge_ipam_override(&class_ipam, Some(&over), None);
        let static_ = merged.static_.expect("static IPAM present");
        assert_eq!(static_.gateway, None);
        assert!(static_.nameservers.is_empty());
        assert_eq!(static_.domain, None);
    }

    #[test]
    fn build_vsphere_machine_leaves_ipam_untouched_when_no_override_matches() {
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
            None,
        )
        .unwrap();
        assert_eq!(m.spec.network[0].ipam.source(), IpamSource::Dhcp);
    }

    #[test]
    fn build_vsphere_machine_applies_matching_network_override() {
        let raw = BTreeMap::from([
            ("datacenter".to_string(), "dc1".to_string()),
            ("cluster".to_string(), "cluster-a".to_string()),
        ]);
        let mut vm = parent_vm();
        vm.spec.network_overrides = vec![static_override("eth0", "10.0.0.90")];
        let m = build_vsphere_machine(
            &vm,
            &parent_class(),
            &parent_image(),
            &decision_with_raw(raw),
            &parent_provider(),
            None,
        )
        .unwrap();
        assert_eq!(m.spec.network[0].ipam.source(), IpamSource::Static);
        assert_eq!(
            m.spec.network[0].ipam.static_.as_ref().unwrap().address,
            "10.0.0.90"
        );
    }

    #[test]
    fn build_vsphere_machine_fills_gateway_from_the_providers_per_zone_subnet() {
        // End-to-end version of merge_ipam_override_fills_gateway_..._from_the_zone_subnet:
        // a Provider declaring a per-zone subnet for the "prod" networkClass
        // (ADR-0032) fills in what the VM's own override left unset, with
        // no VMClass change and no extra VirtualMachine field.
        let raw = BTreeMap::from([
            ("datacenter".to_string(), "dc1".to_string()),
            ("cluster".to_string(), "cluster-a".to_string()),
        ]);
        let mut provider = parent_provider();
        provider.spec.capabilities.network_classes =
            vec![banlieue_api::banlieue::NetworkClassMapping {
                name: "prod".to_string(),
                subnet: Some(SubnetShape {
                    gateway: Some("10.0.0.1".to_string()),
                    nameservers: vec!["10.0.0.53".to_string()],
                    domain: Some("dc1.internal".to_string()),
                }),
                ..Default::default()
            }];
        let mut vm = parent_vm();
        vm.spec.network_overrides = vec![override_missing_subnet_fields_named("eth0", "10.0.0.90")];
        let m = build_vsphere_machine(
            &vm,
            &parent_class(),
            &parent_image(),
            &decision_with_raw(raw),
            &provider,
            None,
        )
        .unwrap();
        let static_ = m.spec.network[0].ipam.static_.as_ref().unwrap();
        assert_eq!(static_.address, "10.0.0.90");
        assert_eq!(static_.gateway.as_deref(), Some("10.0.0.1"));
        assert_eq!(static_.nameservers, vec!["10.0.0.53".to_string()]);
        assert_eq!(static_.domain.as_deref(), Some("dc1.internal"));
    }

    #[test]
    fn build_vsphere_machine_ignores_override_for_a_different_interface_name() {
        let raw = BTreeMap::from([
            ("datacenter".to_string(), "dc1".to_string()),
            ("cluster".to_string(), "cluster-a".to_string()),
        ]);
        let mut vm = parent_vm();
        vm.spec.network_overrides = vec![static_override("eth9-does-not-exist", "10.0.0.90")];
        let m = build_vsphere_machine(
            &vm,
            &parent_class(),
            &parent_image(),
            &decision_with_raw(raw),
            &parent_provider(),
            None,
        )
        .unwrap();
        assert_eq!(m.spec.network[0].ipam.source(), IpamSource::Dhcp);
    }

    // ----------------------------------------------------------------------
    // hardware_override (cpus / memoryMiB / disk sizes)
    // ----------------------------------------------------------------------

    #[test]
    fn build_vsphere_machine_uses_class_hardware_when_no_override() {
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
            None,
        )
        .unwrap();
        assert_eq!(m.spec.num_cpus, 8);
        assert_eq!(m.spec.memory_mi_b, 32_768);
        assert_eq!(m.spec.disks[0].size_gi_b, 100);
    }

    #[test]
    fn build_vsphere_machine_applies_cpus_and_memory_override() {
        let raw = BTreeMap::from([
            ("datacenter".to_string(), "dc1".to_string()),
            ("cluster".to_string(), "cluster-a".to_string()),
        ]);
        let mut vm = parent_vm();
        vm.spec.hardware_override = Some(HardwareOverride {
            cpus: Some(16),
            memory_mi_b: Some(65_536),
            disk_overrides: Vec::new(),
        });
        let m = build_vsphere_machine(
            &vm,
            &parent_class(),
            &parent_image(),
            &decision_with_raw(raw),
            &parent_provider(),
            None,
        )
        .unwrap();
        assert_eq!(m.spec.num_cpus, 16);
        assert_eq!(m.spec.memory_mi_b, 65_536);
        // Disk untouched — no disk_overrides entry.
        assert_eq!(m.spec.disks[0].size_gi_b, 100);
    }

    #[test]
    fn build_vsphere_machine_applies_partial_hardware_override() {
        // Only cpus set — memoryMiB and disks still inherit the class.
        let raw = BTreeMap::from([
            ("datacenter".to_string(), "dc1".to_string()),
            ("cluster".to_string(), "cluster-a".to_string()),
        ]);
        let mut vm = parent_vm();
        vm.spec.hardware_override = Some(HardwareOverride {
            cpus: Some(16),
            memory_mi_b: None,
            disk_overrides: Vec::new(),
        });
        let m = build_vsphere_machine(
            &vm,
            &parent_class(),
            &parent_image(),
            &decision_with_raw(raw),
            &parent_provider(),
            None,
        )
        .unwrap();
        assert_eq!(m.spec.num_cpus, 16);
        assert_eq!(m.spec.memory_mi_b, 32_768);
    }

    #[test]
    fn build_vsphere_machine_applies_disk_size_override_by_name() {
        let raw = BTreeMap::from([
            ("datacenter".to_string(), "dc1".to_string()),
            ("cluster".to_string(), "cluster-a".to_string()),
        ]);
        let mut vm = parent_vm();
        vm.spec.hardware_override = Some(HardwareOverride {
            cpus: None,
            memory_mi_b: None,
            disk_overrides: vec![DiskOverride {
                name: "os".to_string(),
                size_gi_b: 500,
            }],
        });
        let m = build_vsphere_machine(
            &vm,
            &parent_class(),
            &parent_image(),
            &decision_with_raw(raw),
            &parent_provider(),
            None,
        )
        .unwrap();
        assert_eq!(m.spec.disks[0].size_gi_b, 500);
    }

    #[test]
    fn build_vsphere_machine_ignores_disk_override_for_a_different_disk_name() {
        let raw = BTreeMap::from([
            ("datacenter".to_string(), "dc1".to_string()),
            ("cluster".to_string(), "cluster-a".to_string()),
        ]);
        let mut vm = parent_vm();
        vm.spec.hardware_override = Some(HardwareOverride {
            cpus: None,
            memory_mi_b: None,
            disk_overrides: vec![DiskOverride {
                name: "data-does-not-exist".to_string(),
                size_gi_b: 500,
            }],
        });
        let m = build_vsphere_machine(
            &vm,
            &parent_class(),
            &parent_image(),
            &decision_with_raw(raw),
            &parent_provider(),
            None,
        )
        .unwrap();
        assert_eq!(m.spec.disks[0].size_gi_b, 100);
    }

    #[test]
    fn build_vsphere_machine_threads_rendered_user_data() {
        // ADR-0025: the caller (virtualmachine.rs's reconcile()) resolves
        // and renders VirtualMachine.spec.userData's Secret *before*
        // calling build_vsphere_machine, which just inlines whatever
        // content it's given — it never reads a Secret itself.
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
            Some("#cloud-config\nhostname: db-01\n"),
        )
        .unwrap();
        assert_eq!(
            m.spec.user_data.as_deref(),
            Some("#cloud-config\nhostname: db-01\n")
        );
    }

    #[test]
    fn build_vsphere_machine_omits_user_data_when_none_rendered() {
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
            None,
        )
        .unwrap();
        assert!(m.spec.user_data.is_none());
    }

    #[test]
    fn build_vsphere_machine_defaults_to_powered_on() {
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
            None,
        )
        .unwrap();
        assert_eq!(m.spec.desired_power_state, PowerState::PoweredOn);
    }

    #[test]
    fn build_vsphere_machine_resolves_template_from_per_zone_ref() {
        // Url-kind images have per-zone resolved_ref (no top-level one).
        // resolved_ref is the bare template name; template_folder is the
        // per-zone folder it lives in — never a Job name (found live:
        // using the Job name here made every clone fail template lookup).
        let mut image = parent_image();
        image.status.as_mut().unwrap().per_provider = vec![ImagePerProviderStatus {
            provider_name: "vc1".into(),
            provider_namespace: "banlieue-system".into(),
            ready: true,
            resolved_ref: None,
            reason: None,
            message: None,
            zones: vec![ZoneImageStatus {
                name: "vc1-dc1-cluster-a".into(),
                ready: true,
                resolved_ref: Some("kairos-hadron".into()),
                template_folder: Some("templates/vc1-dc1-cluster-a".into()),
                reason: None,
                message: None,
            }],
        }];
        let raw = BTreeMap::from([
            ("datacenter".to_string(), "dc1".to_string()),
            ("cluster".to_string(), "cluster-a".to_string()),
        ]);
        let m = build_vsphere_machine(
            &parent_vm(),
            &parent_class(),
            &image,
            &decision_with_raw(raw),
            &parent_provider(),
            None,
        )
        .unwrap();
        assert_eq!(m.spec.template, "kairos-hadron");
        assert_eq!(
            m.spec.template_folder.as_deref(),
            Some("templates/vc1-dc1-cluster-a")
        );
    }

    #[test]
    fn build_vsphere_machine_threads_desired_power_state_from_virtual_machine_spec() {
        let raw = BTreeMap::from([
            ("datacenter".to_string(), "dc1".to_string()),
            ("cluster".to_string(), "cluster-a".to_string()),
        ]);
        let mut vm = parent_vm();
        vm.spec.desired_power_state = PowerState::PoweredOff;
        let m = build_vsphere_machine(
            &vm,
            &parent_class(),
            &parent_image(),
            &decision_with_raw(raw),
            &parent_provider(),
            None,
        )
        .unwrap();
        assert_eq!(m.spec.desired_power_state, PowerState::PoweredOff);
    }
}
