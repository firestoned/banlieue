// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `vsphere_machine.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    fn sample_disk(name: &str, size: u32) -> VSphereDiskSpec {
        VSphereDiskSpec {
            name: name.to_string(),
            size_gi_b: size,
            provisioning: DiskProvisioning::Thin,
        }
    }

    fn sample_nic(name: &str) -> VSphereNicSpec {
        VSphereNicSpec {
            name: name.to_string(),
            port_group: "vmnet-prod".to_string(),
            mac_address: None,
            ipam: IpamSpec::default(),
        }
    }

    fn minimal_spec() -> VSphereMachineSpec {
        VSphereMachineSpec {
            provider_id: None,
            failure_domain: None,
            provider_ref: LocalObjectReference {
                name: "vsphere-dc1".to_string(),
            },
            template: "ubuntu-22.04-cloudinit".to_string(),
            datacenter: "dc1".to_string(),
            cluster: "cluster-a".to_string(),
            datastore: "ds-fast-01".to_string(),
            folder: None,
            resource_pool: None,
            num_cpus: 4,
            memory_mi_b: 8192,
            firmware: Firmware::Efi,
            disks: vec![sample_disk("os", 40)],
            network: vec![sample_nic("eth0")],
        }
    }

    // ----------------------------------------------------------------------
    // VSphereDiskSpec / VSphereNicSpec
    // ----------------------------------------------------------------------

    #[test]
    fn vsphere_disk_spec_provisioning_defaults_to_thin() {
        let json = serde_json::json!({ "name": "os", "sizeGiB": 40 });
        let d: VSphereDiskSpec = serde_json::from_value(json).unwrap();
        assert_eq!(d.provisioning, DiskProvisioning::Thin);
    }

    #[test]
    fn vsphere_disk_spec_eager_zeroed_round_trip() {
        let d = VSphereDiskSpec {
            name: "data".to_string(),
            size_gi_b: 500,
            provisioning: DiskProvisioning::EagerZeroed,
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["provisioning"], "eagerZeroed");
        let back: VSphereDiskSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn vsphere_disk_spec_missing_size_fails() {
        let err = serde_json::from_str::<VSphereDiskSpec>(r#"{"name":"os"}"#);
        assert!(err.is_err());
    }

    #[test]
    fn vsphere_nic_spec_minimal_omits_mac_address() {
        let n = sample_nic("eth0");
        let json = serde_json::to_value(&n).unwrap();
        assert!(!json.as_object().unwrap().contains_key("macAddress"));
        let back: VSphereNicSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn vsphere_nic_spec_with_mac_address_round_trip() {
        let n = VSphereNicSpec {
            name: "eth0".to_string(),
            port_group: "dvs-prod".to_string(),
            mac_address: Some("00:50:56:00:00:01".to_string()),
            ipam: IpamSpec::default(),
        };
        let json = serde_json::to_value(&n).unwrap();
        assert_eq!(json["macAddress"], "00:50:56:00:00:01");
        let back: VSphereNicSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn vsphere_nic_spec_with_pool_ipam_round_trip() {
        let n = VSphereNicSpec {
            name: "eth0".to_string(),
            port_group: "vmnet-prod".to_string(),
            mac_address: None,
            ipam: IpamSpec {
                source: IpamSource::Pool,
                static_: None,
                pool: Some(PoolIpamConfig {
                    pool_ref: TypedObjectReference {
                        api_group: "ipam.cluster.x-k8s.io".to_string(),
                        kind: "IPAddressClaim".to_string(),
                        name: "pool-a".to_string(),
                        namespace: None,
                    },
                }),
            },
        };
        let json = serde_json::to_value(&n).unwrap();
        assert_eq!(json["ipam"]["source"], "pool");
        assert_eq!(json["ipam"]["pool"]["poolRef"]["name"], "pool-a");
        let back: VSphereNicSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, n);
    }

    // ----------------------------------------------------------------------
    // VSphereMachineSpec — providerID rename + optional fields
    // ----------------------------------------------------------------------

    #[test]
    fn vsphere_machine_spec_provider_id_uses_uppercase_id() {
        let s = VSphereMachineSpec {
            provider_id: Some("vsphere://uuid-1234".to_string()),
            ..minimal_spec()
        };
        let json = serde_json::to_value(&s).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("providerID"), "must rename to providerID");
        assert!(!obj.contains_key("providerId"));
        assert_eq!(obj["providerID"], "vsphere://uuid-1234");
        let back: VSphereMachineSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn vsphere_machine_spec_minimal_omits_all_optional_fields() {
        let s = minimal_spec();
        let json = serde_json::to_value(&s).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("providerID"));
        assert!(!obj.contains_key("failureDomain"));
        assert!(!obj.contains_key("folder"));
        assert!(!obj.contains_key("resourcePool"));
    }

    #[test]
    fn vsphere_machine_spec_with_folder_and_resource_pool_round_trip() {
        let s = VSphereMachineSpec {
            folder: Some("banlieue/prod".to_string()),
            resource_pool: Some("prod-pool".to_string()),
            failure_domain: Some("dc1".to_string()),
            ..minimal_spec()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["folder"], "banlieue/prod");
        assert_eq!(json["resourcePool"], "prod-pool");
        assert_eq!(json["failureDomain"], "dc1");
        let back: VSphereMachineSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn vsphere_machine_spec_memory_uses_camel_case_mi_b() {
        let s = minimal_spec();
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["memoryMiB"], 8192);
        assert_eq!(json["numCpus"], 4);
    }

    #[test]
    fn vsphere_machine_spec_missing_template_fails() {
        let err = serde_json::from_str::<VSphereMachineSpec>(
            r#"{"providerRef":{"name":"p"},"datacenter":"dc","cluster":"c","datastore":"ds","numCpus":1,"memoryMiB":1024,"firmware":"efi","disks":[],"network":[]}"#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn vsphere_machine_spec_missing_provider_ref_fails() {
        let err = serde_json::from_str::<VSphereMachineSpec>(
            r#"{"template":"t","datacenter":"dc","cluster":"c","datastore":"ds","numCpus":1,"memoryMiB":1024,"firmware":"efi","disks":[],"network":[]}"#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn vsphere_machine_spec_full_round_trip() {
        let s = VSphereMachineSpec {
            provider_id: Some("vsphere://abc".to_string()),
            failure_domain: Some("dc1".to_string()),
            folder: Some("f".to_string()),
            resource_pool: Some("rp".to_string()),
            firmware: Firmware::EfiSecure,
            disks: vec![sample_disk("os", 40), sample_disk("data", 200)],
            network: vec![sample_nic("eth0"), sample_nic("eth1")],
            ..minimal_spec()
        };
        let json = serde_json::to_value(&s).unwrap();
        let back: VSphereMachineSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    // ----------------------------------------------------------------------
    // VSphereMachineStatus
    // ----------------------------------------------------------------------

    #[test]
    fn vsphere_machine_status_default_round_trip() {
        let s = VSphereMachineStatus::default();
        let json = serde_json::to_value(&s).unwrap();
        // initialization is non-optional default; others skipped.
        assert!(json.as_object().unwrap().contains_key("initialization"));
        let back: VSphereMachineStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn vsphere_machine_status_full_round_trip() {
        let s = VSphereMachineStatus {
            initialization: InitializationStatus {
                provisioned: Some(true),
            },
            failure_domain: Some("dc1".to_string()),
            addresses: vec![MachineAddress {
                address_type: MachineAddressType::InternalIP,
                address: "10.0.0.10".to_string(),
            }],
            vm_ref: Some("vm-1234".to_string()),
            instance_uuid: Some("uuid-1234".to_string()),
            conditions: Vec::new(),
            observed_generation: Some(2),
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["vmRef"], "vm-1234");
        assert_eq!(json["instanceUuid"], "uuid-1234");
        let back: VSphereMachineStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    // ----------------------------------------------------------------------
    // VSphereMachineTemplate
    // ----------------------------------------------------------------------

    #[test]
    fn vsphere_machine_template_resource_round_trip() {
        let r = VSphereMachineTemplateResource {
            spec: minimal_spec(),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.as_object().unwrap().contains_key("spec"));
        let back: VSphereMachineTemplateResource = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn vsphere_machine_template_spec_round_trip() {
        let s = VSphereMachineTemplateSpec {
            template: VSphereMachineTemplateResource {
                spec: minimal_spec(),
            },
        };
        let json = serde_json::to_value(&s).unwrap();
        let back: VSphereMachineTemplateSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    // ----------------------------------------------------------------------
    // CRD generation
    // ----------------------------------------------------------------------

    #[test]
    fn vsphere_machine_crd_metadata_matches_kube_attributes() {
        use kube::CustomResourceExt;
        let crd = VSphereMachine::crd();
        assert_eq!(crd.spec.group, "infrastructure.banlieue.io");
        assert_eq!(crd.spec.names.kind, "VSphereMachine");
        assert_eq!(crd.spec.names.plural, "vspheremachines");
        assert_eq!(crd.spec.scope, "Namespaced");
        assert!(
            crd.spec
                .versions
                .iter()
                .any(|v| v.name == "v1alpha1" && v.served && v.storage)
        );
    }

    #[test]
    fn vsphere_machine_template_crd_metadata_matches_kube_attributes() {
        use kube::CustomResourceExt;
        let crd = VSphereMachineTemplate::crd();
        assert_eq!(crd.spec.group, "infrastructure.banlieue.io");
        assert_eq!(crd.spec.names.kind, "VSphereMachineTemplate");
        assert_eq!(crd.spec.names.plural, "vspheremachinetemplates");
        assert_eq!(crd.spec.scope, "Namespaced");
    }
}
