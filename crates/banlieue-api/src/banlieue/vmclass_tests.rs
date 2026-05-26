// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `vmclass.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    fn sample_disk(name: &str, size: u32) -> DiskSpec {
        DiskSpec {
            name: name.to_string(),
            size_gi_b: size,
            storage_class: "gold".to_string(),
            provisioning: DiskProvisioning::Thin,
        }
    }

    fn sample_nic(name: &str) -> NetworkInterfaceSpec {
        NetworkInterfaceSpec {
            name: name.to_string(),
            network_class: "prod".to_string(),
            ipam: IpamSpec::default(),
            mtu: None,
        }
    }

    fn minimal_vmclass_spec() -> VMClassSpec {
        VMClassSpec {
            hardware: HardwareSpec {
                cpus: 4,
                memory_mi_b: 8192,
                disks: vec![sample_disk("os", 40)],
            },
            network: NetworkSpec {
                interfaces: vec![sample_nic("eth0")],
            },
            firmware: Firmware::default(),
            features: Vec::new(),
        }
    }

    // ----------------------------------------------------------------------
    // HardwareSpec / DiskSpec — camelCase field naming
    // ----------------------------------------------------------------------

    #[test]
    fn hardware_spec_uses_camel_case_memory_field() {
        let h = HardwareSpec {
            cpus: 2,
            memory_mi_b: 4096,
            disks: Vec::new(),
        };
        let json = serde_json::to_value(&h).unwrap();
        // memoryMiB is the camelCase serialization of memory_mi_b
        assert_eq!(json["memoryMiB"], 4096);
        assert_eq!(json["cpus"], 2);
        let back: HardwareSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn hardware_spec_missing_required_fails() {
        let err = serde_json::from_str::<HardwareSpec>(r#"{"cpus":2}"#);
        assert!(err.is_err(), "memoryMiB and disks are required");
    }

    #[test]
    fn disk_spec_uses_camel_case_size_and_storage_class() {
        let d = DiskSpec {
            name: "os".to_string(),
            size_gi_b: 40,
            storage_class: "gold".to_string(),
            provisioning: DiskProvisioning::Thick,
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["sizeGiB"], 40);
        assert_eq!(json["storageClass"], "gold");
        assert_eq!(json["provisioning"], "thick");
        let back: DiskSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn disk_spec_provisioning_defaults_to_thin_when_omitted() {
        let json = serde_json::json!({
            "name": "os",
            "sizeGiB": 40,
            "storageClass": "gold"
        });
        let d: DiskSpec = serde_json::from_value(json).unwrap();
        assert_eq!(d.provisioning, DiskProvisioning::Thin);
    }

    #[test]
    fn disk_spec_missing_storage_class_fails() {
        let err = serde_json::from_str::<DiskSpec>(r#"{"name":"os","sizeGiB":40}"#);
        assert!(err.is_err());
    }

    // ----------------------------------------------------------------------
    // NetworkInterfaceSpec
    // ----------------------------------------------------------------------

    #[test]
    fn network_interface_spec_minimal_omits_mtu() {
        let n = sample_nic("eth0");
        let json = serde_json::to_value(&n).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("mtu"));
        assert_eq!(obj["networkClass"], "prod");
        assert_eq!(obj["ipam"]["source"], "dhcp");
        let back: NetworkInterfaceSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn network_interface_spec_with_mtu_round_trip() {
        let n = NetworkInterfaceSpec {
            name: "eth0".to_string(),
            network_class: "prod".to_string(),
            ipam: IpamSpec::default(),
            mtu: Some(9000),
        };
        let json = serde_json::to_value(&n).unwrap();
        assert_eq!(json["mtu"], 9000);
        let back: NetworkInterfaceSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn network_interface_spec_with_static_ipam_round_trip() {
        let n = NetworkInterfaceSpec {
            name: "eth0".to_string(),
            network_class: "prod".to_string(),
            ipam: IpamSpec {
                source: IpamSource::Static,
                static_: Some(StaticIpamConfig {
                    address: "10.0.0.5".to_string(),
                    prefix: 24,
                    gateway: Some("10.0.0.1".to_string()),
                    nameservers: Vec::new(),
                }),
                pool: None,
            },
            mtu: None,
        };
        let json = serde_json::to_value(&n).unwrap();
        assert_eq!(json["ipam"]["source"], "static");
        assert_eq!(json["ipam"]["static"]["address"], "10.0.0.5");
        let back: NetworkInterfaceSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn network_interface_spec_missing_ipam_fails() {
        let err = serde_json::from_str::<NetworkInterfaceSpec>(
            r#"{"name":"eth0","networkClass":"prod"}"#,
        );
        assert!(err.is_err());
    }

    // ----------------------------------------------------------------------
    // VMClassSpec — top-level
    // ----------------------------------------------------------------------

    #[test]
    fn vmclass_spec_firmware_default_is_efi_when_omitted() {
        let json = serde_json::json!({
            "hardware": {
                "cpus": 2,
                "memoryMiB": 2048,
                "disks": [{
                    "name": "os",
                    "sizeGiB": 20,
                    "storageClass": "gold"
                }]
            },
            "network": {
                "interfaces": [{
                    "name": "eth0",
                    "networkClass": "prod",
                    "ipam": { "source": "dhcp" }
                }]
            }
        });
        let s: VMClassSpec = serde_json::from_value(json).unwrap();
        assert_eq!(s.firmware, Firmware::Efi);
        assert!(s.features.is_empty());
    }

    #[test]
    fn vmclass_spec_features_round_trip() {
        let s = VMClassSpec {
            features: vec!["hotAddCPU".to_string(), "gpuPassthrough".to_string()],
            ..minimal_vmclass_spec()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json["features"],
            serde_json::json!(["hotAddCPU", "gpuPassthrough"])
        );
        let back: VMClassSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn vmclass_spec_full_round_trip() {
        let s = VMClassSpec {
            hardware: HardwareSpec {
                cpus: 8,
                memory_mi_b: 16384,
                disks: vec![
                    sample_disk("os", 40),
                    DiskSpec {
                        name: "data".to_string(),
                        size_gi_b: 500,
                        storage_class: "bulk".to_string(),
                        provisioning: DiskProvisioning::EagerZeroed,
                    },
                ],
            },
            network: NetworkSpec {
                interfaces: vec![sample_nic("eth0"), sample_nic("eth1")],
            },
            firmware: Firmware::EfiSecure,
            features: vec!["efiSecureBoot".to_string()],
        };
        let json = serde_json::to_value(&s).unwrap();
        let back: VMClassSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn vmclass_spec_missing_hardware_fails() {
        let err = serde_json::from_str::<VMClassSpec>(r#"{"network":{"interfaces":[]}}"#);
        assert!(err.is_err());
    }

    #[test]
    fn vmclass_spec_missing_network_fails() {
        let err = serde_json::from_str::<VMClassSpec>(
            r#"{"hardware":{"cpus":1,"memoryMiB":512,"disks":[]}}"#,
        );
        assert!(err.is_err());
    }

    // ----------------------------------------------------------------------
    // CRD generation
    // ----------------------------------------------------------------------

    #[test]
    fn vmclass_crd_metadata_matches_kube_attributes() {
        use kube::CustomResourceExt;
        let crd = VMClass::crd();
        assert_eq!(crd.spec.group, "banlieue.io");
        assert_eq!(crd.spec.names.kind, "VMClass");
        assert_eq!(crd.spec.names.plural, "vmclasses");
        // VMClass is cluster-scoped (no `namespaced` attribute on the macro).
        assert_eq!(crd.spec.scope, "Cluster");
        assert!(
            crd.spec
                .versions
                .iter()
                .any(|v| v.name == "v1alpha1" && v.served && v.storage)
        );
    }
}
