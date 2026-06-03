// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `common.rs`.
//!
//! Coverage strategy: every public type gets a positive (round-trip),
//! negative (rejects invalid input), and exception (missing required field
//! or unknown variant) test. The shared types are pure-data — there is no
//! behavior beyond serde and a couple of `Default` impls — so the tests are
//! intentionally exhaustive across enum variants and skip-serialization
//! conditions.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::collections::BTreeMap;

    // ----------------------------------------------------------------------
    // InitializationStatus
    // ----------------------------------------------------------------------

    #[test]
    fn initialization_status_default_is_none() {
        let s = InitializationStatus::default();
        assert_eq!(s.provisioned, None);
    }

    #[test]
    fn initialization_status_round_trip_with_value() {
        let s = InitializationStatus {
            provisioned: Some(true),
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json, serde_json::json!({ "provisioned": true }));
        let back: InitializationStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn initialization_status_omits_none_field() {
        let s = InitializationStatus::default();
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json, serde_json::json!({}));
    }

    #[test]
    fn initialization_status_accepts_empty_object() {
        let s: InitializationStatus = serde_json::from_str("{}").unwrap();
        assert_eq!(s.provisioned, None);
    }

    #[test]
    fn initialization_status_rejects_wrong_type() {
        let err = serde_json::from_str::<InitializationStatus>(r#"{"provisioned":"yes"}"#);
        assert!(err.is_err(), "expected error on string for bool field");
    }

    // ----------------------------------------------------------------------
    // ApiEndpoint
    // ----------------------------------------------------------------------

    #[test]
    fn api_endpoint_round_trip() {
        let e = ApiEndpoint {
            host: "10.0.0.10".to_string(),
            port: 6443,
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "host": "10.0.0.10", "port": 6443 })
        );
        let back: ApiEndpoint = serde_json::from_value(json).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn api_endpoint_requires_both_fields() {
        assert!(serde_json::from_str::<ApiEndpoint>(r#"{"host":"h"}"#).is_err());
        assert!(serde_json::from_str::<ApiEndpoint>(r#"{"port":6443}"#).is_err());
    }

    #[test]
    fn api_endpoint_rejects_string_port() {
        assert!(serde_json::from_str::<ApiEndpoint>(r#"{"host":"h","port":"6443"}"#).is_err());
    }

    // ----------------------------------------------------------------------
    // ClusterFailureDomain
    // ----------------------------------------------------------------------

    #[test]
    fn cluster_failure_domain_default_is_empty() {
        let fd = ClusterFailureDomain::default();
        assert_eq!(fd.name, "");
        assert_eq!(fd.control_plane, None);
        assert!(fd.attributes.is_empty());
    }

    #[test]
    fn cluster_failure_domain_omits_empty_optionals() {
        let fd = ClusterFailureDomain {
            name: "prod-vsphere-dc0-c0".to_string(),
            control_plane: None,
            attributes: BTreeMap::new(),
        };
        let json = serde_json::to_value(&fd).unwrap();
        // Only `name` survives; `controlPlane` and empty `attributes` are skipped.
        assert_eq!(json, serde_json::json!({ "name": "prod-vsphere-dc0-c0" }));
    }

    #[test]
    fn cluster_failure_domain_round_trip_full() {
        let mut attributes = BTreeMap::new();
        attributes.insert("datacenter".to_string(), "DC0".to_string());
        attributes.insert("cluster".to_string(), "C0".to_string());
        let fd = ClusterFailureDomain {
            name: "prod-vsphere-dc0-c0".to_string(),
            control_plane: Some(true),
            attributes,
        };
        let json = serde_json::to_value(&fd).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "name": "prod-vsphere-dc0-c0",
                "controlPlane": true,
                "attributes": { "datacenter": "DC0", "cluster": "C0" }
            })
        );
        let back: ClusterFailureDomain = serde_json::from_value(json).unwrap();
        assert_eq!(back, fd);
    }

    #[test]
    fn cluster_failure_domain_uses_camel_case_control_plane() {
        // Guard against the field serializing as snake_case `control_plane`.
        let fd = ClusterFailureDomain {
            name: "fd".to_string(),
            control_plane: Some(false),
            attributes: BTreeMap::new(),
        };
        let s = serde_json::to_string(&fd).unwrap();
        assert!(s.contains("\"controlPlane\""), "got: {s}");
        assert!(!s.contains("control_plane"), "got: {s}");
    }

    // ----------------------------------------------------------------------
    // MachineAddress / MachineAddressType
    // ----------------------------------------------------------------------

    #[test]
    fn machine_address_round_trip_all_types() {
        let cases = [
            (MachineAddressType::Hostname, "Hostname"),
            (MachineAddressType::ExternalIP, "ExternalIP"),
            (MachineAddressType::InternalIP, "InternalIP"),
            (MachineAddressType::ExternalDNS, "ExternalDNS"),
            (MachineAddressType::InternalDNS, "InternalDNS"),
        ];
        for (variant, expected) in cases {
            let ma = MachineAddress {
                address_type: variant.clone(),
                address: "10.0.0.1".to_string(),
            };
            let json = serde_json::to_value(&ma).unwrap();
            assert_eq!(
                json,
                serde_json::json!({ "type": expected, "address": "10.0.0.1" })
            );
            let back: MachineAddress = serde_json::from_value(json).unwrap();
            assert_eq!(back, ma);
        }
    }

    #[test]
    fn machine_address_rejects_unknown_type_variant() {
        let err =
            serde_json::from_str::<MachineAddress>(r#"{"type":"Loopback","address":"127.0.0.1"}"#);
        assert!(err.is_err(), "expected error on unknown address type");
    }

    #[test]
    fn machine_address_missing_required_field_fails() {
        let err = serde_json::from_str::<MachineAddress>(r#"{"type":"Hostname"}"#);
        assert!(err.is_err(), "expected error when `address` is missing");
    }

    // ----------------------------------------------------------------------
    // LocalObjectReference
    // ----------------------------------------------------------------------

    #[test]
    fn local_object_reference_default_is_empty_name() {
        let r = LocalObjectReference::default();
        assert!(r.name.is_empty());
    }

    #[test]
    fn local_object_reference_round_trip() {
        let r = LocalObjectReference {
            name: "my-secret".to_string(),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json, serde_json::json!({ "name": "my-secret" }));
        let back: LocalObjectReference = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn local_object_reference_missing_name_fails() {
        let err = serde_json::from_str::<LocalObjectReference>("{}");
        assert!(err.is_err());
    }

    // ----------------------------------------------------------------------
    // TypedObjectReference
    // ----------------------------------------------------------------------

    #[test]
    fn typed_object_reference_with_namespace_round_trip() {
        let r = TypedObjectReference {
            api_group: "ipam.cluster.x-k8s.io".to_string(),
            kind: "IPAddressClaim".to_string(),
            name: "pool-a".to_string(),
            namespace: Some("ipam".to_string()),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "apiGroup": "ipam.cluster.x-k8s.io",
                "kind": "IPAddressClaim",
                "name": "pool-a",
                "namespace": "ipam"
            })
        );
        let back: TypedObjectReference = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn typed_object_reference_without_namespace_omits_field() {
        let r = TypedObjectReference {
            api_group: "g".to_string(),
            kind: "K".to_string(),
            name: "n".to_string(),
            namespace: None,
        };
        let json = serde_json::to_value(&r).unwrap();
        assert!(!json.as_object().unwrap().contains_key("namespace"));
    }

    #[test]
    fn typed_object_reference_missing_required_fails() {
        let err = serde_json::from_str::<TypedObjectReference>(r#"{"apiGroup":"g","kind":"K"}"#);
        assert!(err.is_err());
    }

    // ----------------------------------------------------------------------
    // LabelSelector / LabelSelectorRequirement / LabelSelectorOperator
    // ----------------------------------------------------------------------

    #[test]
    fn label_selector_default_is_empty() {
        let s = LabelSelector::default();
        assert!(s.match_labels.is_empty());
        assert!(s.match_expressions.is_empty());
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json, serde_json::json!({}));
    }

    #[test]
    fn label_selector_with_match_labels_serializes() {
        let mut labels = BTreeMap::new();
        labels.insert("env".to_string(), "prod".to_string());
        let s = LabelSelector {
            match_labels: labels,
            match_expressions: Vec::new(),
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "matchLabels": { "env": "prod" } })
        );
    }

    #[test]
    fn label_selector_with_match_expressions_serializes() {
        let s = LabelSelector {
            match_labels: BTreeMap::new(),
            match_expressions: vec![LabelSelectorRequirement {
                key: "tier".to_string(),
                operator: LabelSelectorOperator::In,
                values: vec!["gold".to_string(), "silver".to_string()],
            }],
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "matchExpressions": [
                    { "key": "tier", "operator": "In", "values": ["gold", "silver"] }
                ]
            })
        );
    }

    #[test]
    fn label_selector_operator_all_variants_round_trip() {
        let cases = [
            (LabelSelectorOperator::In, "In"),
            (LabelSelectorOperator::NotIn, "NotIn"),
            (LabelSelectorOperator::Exists, "Exists"),
            (LabelSelectorOperator::DoesNotExist, "DoesNotExist"),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json, serde_json::json!(expected));
            let back: LabelSelectorOperator = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn label_selector_operator_rejects_unknown_variant() {
        let err = serde_json::from_str::<LabelSelectorOperator>(r#""StartsWith""#);
        assert!(err.is_err());
    }

    #[test]
    fn label_selector_requirement_omits_empty_values() {
        let r = LabelSelectorRequirement {
            key: "k".to_string(),
            operator: LabelSelectorOperator::Exists,
            values: Vec::new(),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert!(!json.as_object().unwrap().contains_key("values"));
    }

    // ----------------------------------------------------------------------
    // DiskProvisioning
    // ----------------------------------------------------------------------

    #[test]
    fn disk_provisioning_default_is_thin() {
        assert_eq!(DiskProvisioning::default(), DiskProvisioning::Thin);
    }

    #[test]
    fn disk_provisioning_all_variants_round_trip() {
        let cases = [
            (DiskProvisioning::Thin, "thin"),
            (DiskProvisioning::Thick, "thick"),
            (DiskProvisioning::EagerZeroed, "eagerZeroed"),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json, serde_json::json!(expected));
            let back: DiskProvisioning = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn disk_provisioning_rejects_unknown_variant() {
        let err = serde_json::from_str::<DiskProvisioning>(r#""sparse""#);
        assert!(err.is_err());
    }

    #[test]
    fn disk_provisioning_rejects_pascal_case_input() {
        let err = serde_json::from_str::<DiskProvisioning>(r#""Thin""#);
        assert!(err.is_err(), "camelCase rename should reject PascalCase");
    }

    // ----------------------------------------------------------------------
    // Firmware (kebab-case)
    // ----------------------------------------------------------------------

    #[test]
    fn firmware_default_is_efi() {
        assert_eq!(Firmware::default(), Firmware::Efi);
    }

    #[test]
    fn firmware_all_variants_use_kebab_case() {
        let cases = [
            (Firmware::Bios, "bios"),
            (Firmware::Efi, "efi"),
            (Firmware::EfiSecure, "efi-secure"),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json, serde_json::json!(expected));
            let back: Firmware = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn firmware_rejects_pascal_case_input() {
        let err = serde_json::from_str::<Firmware>(r#""Efi""#);
        assert!(err.is_err(), "kebab-case rename should reject PascalCase");
    }

    // ----------------------------------------------------------------------
    // PowerState (PoweredOn / PoweredOff / Suspended — explicit string forms
    // chosen to avoid YAML 1.1 implicit-boolean parsing of bare `On`/`Off`).
    // ----------------------------------------------------------------------

    #[test]
    fn power_state_default_is_powered_on() {
        assert_eq!(PowerState::default(), PowerState::PoweredOn);
    }

    #[test]
    fn power_state_variants_serialize_unambiguously() {
        let cases = [
            (PowerState::PoweredOn, "PoweredOn"),
            (PowerState::PoweredOff, "PoweredOff"),
            (PowerState::Suspended, "Suspended"),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json, serde_json::json!(expected));
            let back: PowerState = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn power_state_rejects_lowercase_input() {
        let err = serde_json::from_str::<PowerState>(r#""poweredon""#);
        assert!(err.is_err());
    }

    #[test]
    fn power_state_rejects_legacy_short_form() {
        // `On`/`Off` are no longer accepted — they're the strings that caused
        // CRD schema validation to fail on the kube apiserver (Go YAML 1.1
        // boolean coercion).
        assert!(serde_json::from_str::<PowerState>(r#""On""#).is_err());
        assert!(serde_json::from_str::<PowerState>(r#""Off""#).is_err());
    }

    // ----------------------------------------------------------------------
    // IpamSpec (flat struct + IpamSource discriminator)
    // ----------------------------------------------------------------------

    #[test]
    fn ipam_source_default_is_dhcp() {
        assert_eq!(IpamSource::default(), IpamSource::Dhcp);
    }

    #[test]
    fn ipam_spec_default_is_dhcp_with_no_sub_configs() {
        let s = IpamSpec::default();
        assert_eq!(s.source, IpamSource::Dhcp);
        assert!(s.static_.is_none());
        assert!(s.pool.is_none());
    }

    #[test]
    fn ipam_source_all_variants_round_trip() {
        for (variant, expected) in [
            (IpamSource::Dhcp, "dhcp"),
            (IpamSource::Static, "static"),
            (IpamSource::Pool, "pool"),
        ] {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json, serde_json::json!(expected));
            let back: IpamSource = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn ipam_source_rejects_unknown_variant() {
        let err = serde_json::from_str::<IpamSource>(r#""slaac""#);
        assert!(err.is_err());
    }

    #[test]
    fn ipam_spec_dhcp_round_trip_omits_sub_configs() {
        let s = IpamSpec {
            source: IpamSource::Dhcp,
            static_: None,
            pool: None,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json, serde_json::json!({ "source": "dhcp" }));
        let back: IpamSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn ipam_spec_static_minimal_round_trip() {
        let s = IpamSpec {
            source: IpamSource::Static,
            static_: Some(StaticIpamConfig {
                address: "10.0.0.5".to_string(),
                prefix: 24,
                gateway: None,
                nameservers: Vec::new(),
            }),
            pool: None,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "source": "static",
                "static": { "address": "10.0.0.5", "prefix": 24 }
            })
        );
        let back: IpamSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn ipam_spec_static_with_gateway_and_dns_round_trip() {
        let s = IpamSpec {
            source: IpamSource::Static,
            static_: Some(StaticIpamConfig {
                address: "10.0.0.5".to_string(),
                prefix: 24,
                gateway: Some("10.0.0.1".to_string()),
                nameservers: vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()],
            }),
            pool: None,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["source"], "static");
        assert_eq!(json["static"]["gateway"], "10.0.0.1");
        assert_eq!(
            json["static"]["nameservers"],
            serde_json::json!(["1.1.1.1", "8.8.8.8"])
        );
        let back: IpamSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn ipam_spec_pool_round_trip() {
        let s = IpamSpec {
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
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "source": "pool",
                "pool": {
                    "poolRef": {
                        "apiGroup": "ipam.cluster.x-k8s.io",
                        "kind": "IPAddressClaim",
                        "name": "pool-a"
                    }
                }
            })
        );
        let back: IpamSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn ipam_spec_rejects_unknown_source() {
        let err = serde_json::from_str::<IpamSpec>(r#"{"source":"slaac"}"#);
        assert!(err.is_err());
    }

    #[test]
    fn ipam_spec_static_config_missing_address_fails() {
        let err = serde_json::from_str::<StaticIpamConfig>(r#"{"prefix":24}"#);
        assert!(err.is_err());
    }

    #[test]
    fn ipam_spec_pool_config_missing_pool_ref_fails() {
        let err = serde_json::from_str::<PoolIpamConfig>(r#"{}"#);
        assert!(err.is_err());
    }

    #[test]
    fn ipam_spec_missing_source_field_fails() {
        // `source` is the only required field on IpamSpec — both `static`
        // and `pool` are optional sibling configs. Cross-field validation
        // ("source=Static implies static is set") is intentionally NOT
        // schema-enforced; it's the controller's job (and a future CEL rule).
        let err =
            serde_json::from_str::<IpamSpec>(r#"{"static":{"address":"10.0.0.5","prefix":24}}"#);
        assert!(err.is_err());
    }

    // ----------------------------------------------------------------------
    // condition_reasons / condition_types constants
    // ----------------------------------------------------------------------

    #[test]
    fn condition_reasons_have_expected_stable_strings() {
        assert_eq!(condition_reasons::VM_CREATED, "VMCreated");
        assert_eq!(condition_reasons::VM_RUNNING, "VMRunning");
        assert_eq!(condition_reasons::VM_STOPPED, "VMStopped");
        assert_eq!(condition_reasons::CLONING, "Cloning");
        assert_eq!(condition_reasons::POWERED_ON, "PoweredOn");
        assert_eq!(condition_reasons::POWERED_OFF, "PoweredOff");
        assert_eq!(condition_reasons::SCHEDULED, "Scheduled");
        assert_eq!(condition_reasons::SCHEDULING_FAILED, "SchedulingFailed");
        assert_eq!(condition_reasons::PLACEMENT_DRIFT, "PlacementDrift");
        assert_eq!(condition_reasons::PLACEMENT_VALID, "PlacementValid");
        assert_eq!(condition_reasons::MIGRATING, "Migrating");
        assert_eq!(condition_reasons::IMAGE_PENDING, "ImagePending");
        assert_eq!(condition_reasons::IMAGE_READY, "ImageReady");
        assert_eq!(condition_reasons::IMAGE_IMPORT_FAILED, "ImageImportFailed");
        assert_eq!(condition_reasons::IPAM_PENDING, "IPAMPending");
        assert_eq!(condition_reasons::IPAM_BOUND, "IPAMBound");
    }

    #[test]
    fn condition_types_have_expected_stable_strings() {
        assert_eq!(condition_types::READY, "Ready");
        assert_eq!(condition_types::INFRASTRUCTURE_READY, "InfrastructureReady");
        assert_eq!(condition_types::SCHEDULED, "Scheduled");
        assert_eq!(condition_types::PLACEMENT_VALID, "PlacementValid");
        assert_eq!(condition_types::MIGRATING, "Migrating");
        assert_eq!(condition_types::POWER_STATE, "PowerState");
        assert_eq!(condition_types::IMAGE_READY, "ImageReady");
        assert_eq!(condition_types::PROVIDER_REACHABLE, "ProviderReachable");
    }

    // ----------------------------------------------------------------------
    // KeySelector
    // ----------------------------------------------------------------------

    #[test]
    fn key_selector_omits_none_key() {
        let s = KeySelector {
            name: "trust".to_string(),
            key: None,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json, serde_json::json!({ "name": "trust" }));
        let back: KeySelector = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn key_selector_round_trip_with_key() {
        let s = KeySelector {
            name: "trust".to_string(),
            key: Some("tls.crt".to_string()),
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "name": "trust", "key": "tls.crt" })
        );
        let back: KeySelector = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn key_selector_requires_name() {
        let err = serde_json::from_str::<KeySelector>("{}");
        assert!(err.is_err(), "name is required");
    }

    #[test]
    fn key_selector_key_or_uses_default_when_absent() {
        let s = KeySelector {
            name: "trust".to_string(),
            key: None,
        };
        assert_eq!(s.key_or(DEFAULT_CA_BUNDLE_KEY), "ca.crt");
    }

    #[test]
    fn key_selector_key_or_prefers_explicit_key() {
        let s = KeySelector {
            name: "trust".to_string(),
            key: Some("custom.pem".to_string()),
        };
        assert_eq!(s.key_or(DEFAULT_CA_BUNDLE_KEY), "custom.pem");
    }

    #[test]
    fn default_ca_bundle_key_is_ca_crt() {
        assert_eq!(DEFAULT_CA_BUNDLE_KEY, "ca.crt");
    }

    // ----------------------------------------------------------------------
    // CABundleSource
    // ----------------------------------------------------------------------

    #[test]
    fn ca_bundle_source_inline_round_trip() {
        let s = CABundleSource {
            inline: Some("-----BEGIN CERTIFICATE-----\n...".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "inline": "-----BEGIN CERTIFICATE-----\n..." })
        );
        let back: CABundleSource = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn ca_bundle_source_config_map_ref_round_trip() {
        let s = CABundleSource {
            config_map_ref: Some(KeySelector {
                name: "corp-trust".to_string(),
                key: None,
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "configMapRef": { "name": "corp-trust" } })
        );
        let back: CABundleSource = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn ca_bundle_source_secret_ref_round_trip() {
        let s = CABundleSource {
            secret_ref: Some(KeySelector {
                name: "private-ca".to_string(),
                key: Some("bundle.pem".to_string()),
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "secretRef": { "name": "private-ca", "key": "bundle.pem" } })
        );
        let back: CABundleSource = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn ca_bundle_source_default_is_empty_and_invalid() {
        let s = CABundleSource::default();
        assert_eq!(s.source_count(), 0);
        assert!(s.validate().is_err(), "zero sources must be rejected");
    }

    #[test]
    fn ca_bundle_source_validate_accepts_exactly_one() {
        for s in [
            CABundleSource {
                inline: Some("pem".to_string()),
                ..Default::default()
            },
            CABundleSource {
                config_map_ref: Some(KeySelector {
                    name: "cm".to_string(),
                    key: None,
                }),
                ..Default::default()
            },
            CABundleSource {
                secret_ref: Some(KeySelector {
                    name: "sec".to_string(),
                    key: None,
                }),
                ..Default::default()
            },
        ] {
            assert_eq!(s.source_count(), 1);
            assert!(s.validate().is_ok());
        }
    }

    #[test]
    fn ca_bundle_source_validate_rejects_more_than_one() {
        let s = CABundleSource {
            inline: Some("pem".to_string()),
            secret_ref: Some(KeySelector {
                name: "sec".to_string(),
                key: None,
            }),
            ..Default::default()
        };
        assert_eq!(s.source_count(), 2);
        let err = s.validate().unwrap_err();
        assert!(err.contains("more than one"), "got: {err}");
    }

    #[test]
    fn ca_bundle_source_all_three_set_is_invalid() {
        let s = CABundleSource {
            inline: Some("pem".to_string()),
            config_map_ref: Some(KeySelector {
                name: "cm".to_string(),
                key: None,
            }),
            secret_ref: Some(KeySelector {
                name: "sec".to_string(),
                key: None,
            }),
        };
        assert_eq!(s.source_count(), 3);
        assert!(s.validate().is_err());
    }
}
