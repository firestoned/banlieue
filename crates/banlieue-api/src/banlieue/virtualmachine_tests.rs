// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `virtualmachine.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
    use kube::CustomResourceExt;
    use std::collections::BTreeMap;

    fn fixed_time() -> Time {
        let s = "\"2026-05-24T12:00:00Z\"";
        Time(serde_json::from_str(s).unwrap())
    }

    fn minimal_vm_spec() -> VirtualMachineSpec {
        VirtualMachineSpec {
            class_ref: LocalObjectReference {
                name: "db-prod-large".to_string(),
            },
            image_ref: LocalObjectReference {
                name: "ubuntu-22.04".to_string(),
            },
            placement: PlacementSpec::default(),
            desired_power_state: PowerState::PoweredOn,
            user_data: None,
            migration_policy: MigrationPolicy::default(),
            paused: false,
        }
    }

    // ----------------------------------------------------------------------
    // Defaults
    // ----------------------------------------------------------------------

    #[test]
    fn affinity_mode_default_is_required() {
        assert_eq!(AffinityMode::default(), AffinityMode::Required);
    }

    #[test]
    fn migration_policy_default_is_automatic() {
        assert_eq!(MigrationPolicy::default(), MigrationPolicy::Automatic);
    }

    #[test]
    fn vm_spec_default_power_state_is_powered_on() {
        // The function `default_power_on` is private, but we can assert the
        // observable default behavior by deserializing a spec that omits it.
        let json = serde_json::json!({
            "classRef": { "name": "c" },
            "imageRef": { "name": "i" }
        });
        let s: VirtualMachineSpec = serde_json::from_value(json).unwrap();
        assert_eq!(s.desired_power_state, PowerState::PoweredOn);
    }

    #[test]
    fn vm_user_data_default_key_is_user_dash_data() {
        // `default_userdata_key` is private; verify via deserialization.
        let json = serde_json::json!({ "secretRef": { "name": "ud" } });
        let ud: UserDataSpec = serde_json::from_value(json).unwrap();
        assert_eq!(ud.key, "user-data");
    }

    #[test]
    fn placement_spec_default_omits_all_optional_fields() {
        let p = PlacementSpec::default();
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json, serde_json::json!({}));
    }

    // ----------------------------------------------------------------------
    // Enum round-trips
    // ----------------------------------------------------------------------

    #[test]
    fn affinity_mode_all_variants_round_trip() {
        for (variant, expected) in [
            (AffinityMode::Required, "required"),
            (AffinityMode::Preferred, "preferred"),
        ] {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json, serde_json::json!(expected));
            let back: AffinityMode = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn affinity_mode_rejects_unknown_variant() {
        let err = serde_json::from_str::<AffinityMode>(r#""forbidden""#);
        assert!(err.is_err());
    }

    #[test]
    fn migration_policy_all_variants_round_trip() {
        for (variant, expected) in [
            (MigrationPolicy::Automatic, "automatic"),
            (MigrationPolicy::Manual, "manual"),
            (MigrationPolicy::Never, "never"),
        ] {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json, serde_json::json!(expected));
            let back: MigrationPolicy = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn migration_policy_rejects_unknown_variant() {
        let err = serde_json::from_str::<MigrationPolicy>(r#""always""#);
        assert!(err.is_err());
    }

    // ----------------------------------------------------------------------
    // Spec serialization shape
    // ----------------------------------------------------------------------

    #[test]
    fn vm_spec_minimal_skips_paused_and_user_data() {
        let s = minimal_vm_spec();
        let json = serde_json::to_value(&s).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("paused"));
        assert!(!obj.contains_key("userData"));
        assert!(obj.contains_key("classRef"));
        assert!(obj.contains_key("imageRef"));
    }

    #[test]
    fn vm_spec_paused_true_round_trip() {
        let s = VirtualMachineSpec {
            paused: true,
            ..minimal_vm_spec()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["paused"], true);
        let back: VirtualMachineSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn vm_spec_with_user_data_round_trip() {
        let s = VirtualMachineSpec {
            user_data: Some(UserDataSpec {
                secret_ref: LocalObjectReference {
                    name: "cloud-init".to_string(),
                },
                key: "user-data".to_string(),
            }),
            ..minimal_vm_spec()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["userData"]["secretRef"]["name"], "cloud-init");
        assert_eq!(json["userData"]["key"], "user-data");
        let back: VirtualMachineSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn vm_spec_with_custom_userdata_key_round_trip() {
        let s = VirtualMachineSpec {
            user_data: Some(UserDataSpec {
                secret_ref: LocalObjectReference {
                    name: "ignition".to_string(),
                },
                key: "ignition.json".to_string(),
            }),
            ..minimal_vm_spec()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["userData"]["key"], "ignition.json");
        let back: VirtualMachineSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back.user_data.unwrap().key, "ignition.json");
    }

    #[test]
    fn vm_spec_missing_class_ref_fails() {
        let err = serde_json::from_str::<VirtualMachineSpec>(r#"{"imageRef":{"name":"i"}}"#);
        assert!(err.is_err());
    }

    #[test]
    fn vm_spec_missing_image_ref_fails() {
        let err = serde_json::from_str::<VirtualMachineSpec>(r#"{"classRef":{"name":"c"}}"#);
        assert!(err.is_err());
    }

    // ----------------------------------------------------------------------
    // PlacementSpec / AntiAffinityRule
    // ----------------------------------------------------------------------

    #[test]
    fn placement_spec_with_selectors_and_anti_affinity_round_trip() {
        let mut labels = BTreeMap::new();
        labels.insert("env".to_string(), "prod".to_string());
        let p = PlacementSpec {
            provider_selector: Some(LabelSelector {
                match_labels: labels.clone(),
                match_expressions: Vec::new(),
            }),
            failure_domain_selector: Some(LabelSelector {
                match_labels: labels,
                match_expressions: Vec::new(),
            }),
            anti_affinity: vec![AntiAffinityRule {
                topology_key: "rack".to_string(),
                label_selector: LabelSelector::default(),
                mode: AffinityMode::Preferred,
            }],
        };
        let json = serde_json::to_value(&p).unwrap();
        let back: PlacementSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn anti_affinity_rule_missing_topology_key_fails() {
        let err = serde_json::from_str::<AntiAffinityRule>(r#"{"labelSelector":{}}"#);
        assert!(err.is_err());
    }

    // ----------------------------------------------------------------------
    // Status: ScheduledPlacement / ResolvedResource
    // ----------------------------------------------------------------------

    #[test]
    fn resolved_resource_round_trip() {
        let r = ResolvedResource {
            class_name: "gold".to_string(),
            backend_id: "ds-fast-01".to_string(),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "className": "gold", "backendId": "ds-fast-01" })
        );
        let back: ResolvedResource = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn scheduled_placement_minimal_round_trip() {
        let s = ScheduledPlacement {
            provider_name: "vsphere-dc1".to_string(),
            provider_class: "vsphere".to_string(),
            failure_domain: "dc1".to_string(),
            resolved_storage: Vec::new(),
            resolved_networks: Vec::new(),
            scheduled_at: None,
        };
        let json = serde_json::to_value(&s).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("resolvedStorage"));
        assert!(!obj.contains_key("resolvedNetworks"));
        assert!(!obj.contains_key("scheduledAt"));
        let back: ScheduledPlacement = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn scheduled_placement_with_resolved_resources_and_time_round_trip() {
        let s = ScheduledPlacement {
            provider_name: "vsphere-dc1".to_string(),
            provider_class: "vsphere".to_string(),
            failure_domain: "dc1".to_string(),
            resolved_storage: vec![ResolvedResource {
                class_name: "gold".to_string(),
                backend_id: "ds-fast-01".to_string(),
            }],
            resolved_networks: vec![ResolvedResource {
                class_name: "prod".to_string(),
                backend_id: "vmnet-prod".to_string(),
            }],
            scheduled_at: Some(fixed_time()),
        };
        let json = serde_json::to_value(&s).unwrap();
        let back: ScheduledPlacement = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn virtual_machine_status_default_round_trip() {
        let s = VirtualMachineStatus::default();
        let json = serde_json::to_value(&s).unwrap();
        // Only `initialization` (empty default) survives serialization.
        assert!(json.as_object().unwrap().contains_key("initialization"));
        let back: VirtualMachineStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn virtual_machine_status_with_addresses_and_power_state_round_trip() {
        let s = VirtualMachineStatus {
            scheduled: Some(ScheduledPlacement {
                provider_name: "p".to_string(),
                provider_class: "vsphere".to_string(),
                failure_domain: "fd1".to_string(),
                resolved_storage: Vec::new(),
                resolved_networks: Vec::new(),
                scheduled_at: None,
            }),
            infrastructure_ref: Some(TypedObjectReference {
                api_group: "infrastructure.banlieue.io".to_string(),
                kind: "VSphereMachine".to_string(),
                name: "vm-1".to_string(),
                namespace: Some("default".to_string()),
            }),
            initialization: InitializationStatus {
                provisioned: Some(true),
            },
            addresses: vec![MachineAddress {
                address_type: MachineAddressType::InternalIP,
                address: "10.0.0.10".to_string(),
            }],
            observed_power_state: Some(PowerState::PoweredOn),
            conditions: Vec::new(),
            observed_generation: Some(5),
        };
        let json = serde_json::to_value(&s).unwrap();
        let back: VirtualMachineStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    // ----------------------------------------------------------------------
    // CRD generation
    // ----------------------------------------------------------------------

    #[test]
    fn virtual_machine_crd_metadata_matches_kube_attributes() {
        let crd = VirtualMachine::crd();
        assert_eq!(crd.spec.group, "banlieue.io");
        assert_eq!(crd.spec.names.kind, "VirtualMachine");
        assert_eq!(crd.spec.names.plural, "virtualmachines");
        assert_eq!(crd.spec.scope, "Namespaced");
        assert!(
            crd.spec
                .versions
                .iter()
                .any(|v| v.name == "v1alpha1" && v.served && v.storage)
        );
    }
}
