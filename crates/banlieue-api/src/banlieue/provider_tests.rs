// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `provider.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
    use kube::CustomResourceExt;
    use std::collections::BTreeMap;

    fn sample_storage_class(name: &str) -> StorageClassMapping {
        let mut target = BTreeMap::new();
        target.insert("datastore".to_string(), "ds-fast-01".to_string());
        StorageClassMapping {
            name: name.to_string(),
            target,
        }
    }

    fn sample_network_class(name: &str) -> NetworkClassMapping {
        let mut target = BTreeMap::new();
        target.insert("portGroup".to_string(), "vmnet-prod".to_string());
        NetworkClassMapping {
            name: name.to_string(),
            target,
        }
    }

    // ----------------------------------------------------------------------
    // ProviderCapabilities::is_empty()
    // ----------------------------------------------------------------------

    #[test]
    fn provider_capabilities_default_is_empty() {
        let c = ProviderCapabilities::default();
        assert!(c.is_empty());
    }

    #[test]
    fn provider_capabilities_with_storage_class_is_not_empty() {
        let c = ProviderCapabilities {
            storage_classes: vec![sample_storage_class("gold")],
            network_classes: Vec::new(),
            features: Vec::new(),
        };
        assert!(!c.is_empty());
    }

    #[test]
    fn provider_capabilities_with_network_class_is_not_empty() {
        let c = ProviderCapabilities {
            storage_classes: Vec::new(),
            network_classes: vec![sample_network_class("prod")],
            features: Vec::new(),
        };
        assert!(!c.is_empty());
    }

    #[test]
    fn provider_capabilities_with_only_features_is_not_empty() {
        let c = ProviderCapabilities {
            storage_classes: Vec::new(),
            network_classes: Vec::new(),
            features: vec!["hotAddCPU".to_string()],
        };
        assert!(!c.is_empty());
    }

    #[test]
    fn provider_capabilities_with_everything_is_not_empty() {
        let c = ProviderCapabilities {
            storage_classes: vec![sample_storage_class("gold")],
            network_classes: vec![sample_network_class("prod")],
            features: vec!["efiSecureBoot".to_string()],
        };
        assert!(!c.is_empty());
    }

    // ----------------------------------------------------------------------
    // Serialization shape
    // ----------------------------------------------------------------------

    #[test]
    fn provider_connection_minimal_round_trip() {
        let c = ProviderConnection {
            endpoint: "https://vc.example.com/sdk".to_string(),
            credentials_ref: LocalObjectReference {
                name: "vc-creds".to_string(),
            },
            insecure_skip_tls_verify: false,
            ca_bundle: None,
        };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "endpoint": "https://vc.example.com/sdk",
                "credentialsRef": { "name": "vc-creds" }
            })
        );
        let back: ProviderConnection = serde_json::from_value(json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn provider_connection_with_optional_ca_and_insecure_round_trip() {
        let c = ProviderConnection {
            endpoint: "https://pve:8006".to_string(),
            credentials_ref: LocalObjectReference {
                name: "pve-creds".to_string(),
            },
            insecure_skip_tls_verify: true,
            ca_bundle: Some(CABundleSource {
                inline: Some("-----BEGIN CERT-----\n...".to_string()),
                ..Default::default()
            }),
        };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["insecureSkipTLSVerify"], true);
        assert_eq!(json["caBundle"]["inline"], "-----BEGIN CERT-----\n...");
        let back: ProviderConnection = serde_json::from_value(json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn provider_connection_missing_endpoint_fails() {
        let err = serde_json::from_str::<ProviderConnection>(r#"{"credentialsRef":{"name":"x"}}"#);
        assert!(err.is_err());
    }

    #[test]
    fn provider_connection_missing_credentials_ref_fails() {
        let err = serde_json::from_str::<ProviderConnection>(r#"{"endpoint":"https://x"}"#);
        assert!(err.is_err());
    }

    #[test]
    fn provider_spec_minimal_serializes_without_paused_or_capabilities() {
        let s = ProviderSpec {
            provider_class_ref: LocalObjectReference {
                name: "vsphere".to_string(),
            },
            connection: ProviderConnection {
                endpoint: "https://vc.example.com/sdk".to_string(),
                credentials_ref: LocalObjectReference {
                    name: "vc-creds".to_string(),
                },
                insecure_skip_tls_verify: false,
                ca_bundle: None,
            },
            capabilities: ProviderCapabilities::default(),
            paused: false,
        };
        let json = serde_json::to_value(&s).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("paused"), "paused=false must be skipped");
        assert!(
            !obj.contains_key("capabilities"),
            "empty capabilities must be skipped"
        );
        assert!(obj.contains_key("providerClassRef"));
    }

    #[test]
    fn provider_spec_paused_true_round_trip() {
        let s = ProviderSpec {
            provider_class_ref: LocalObjectReference {
                name: "libvirt".to_string(),
            },
            connection: ProviderConnection {
                endpoint: "qemu+ssh://host/system".to_string(),
                credentials_ref: LocalObjectReference {
                    name: "ssh-key".to_string(),
                },
                insecure_skip_tls_verify: false,
                ca_bundle: None,
            },
            capabilities: ProviderCapabilities::default(),
            paused: true,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["paused"], true);
        let back: ProviderSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn provider_spec_full_round_trip() {
        let s = ProviderSpec {
            provider_class_ref: LocalObjectReference {
                name: "vsphere".to_string(),
            },
            connection: ProviderConnection {
                endpoint: "https://vc/sdk".to_string(),
                credentials_ref: LocalObjectReference {
                    name: "vc".to_string(),
                },
                insecure_skip_tls_verify: false,
                ca_bundle: None,
            },
            capabilities: ProviderCapabilities {
                storage_classes: vec![sample_storage_class("gold")],
                network_classes: vec![sample_network_class("prod")],
                features: vec!["hotAddCPU".to_string()],
            },
            paused: false,
        };
        let json = serde_json::to_value(&s).unwrap();
        let back: ProviderSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    // ----------------------------------------------------------------------
    // ProviderStatus / FailureDomain / FailureDomainAttributes
    // ----------------------------------------------------------------------

    #[test]
    fn provider_status_default_round_trip() {
        let s = ProviderStatus::default();
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json, serde_json::json!({}));
        let back: ProviderStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn provider_status_with_failure_domain_and_condition_round_trip() {
        let mut labels = BTreeMap::new();
        labels.insert("dc".to_string(), "dc1".to_string());
        let s = ProviderStatus {
            failure_domains: vec![FailureDomain {
                name: "vsphere-dc1".to_string(),
                labels,
                attributes: FailureDomainAttributes {
                    available_storage_classes: vec!["gold".to_string()],
                    available_network_classes: vec!["prod".to_string()],
                    features: vec!["hotAddCPU".to_string()],
                    raw: BTreeMap::new(),
                },
            }],
            conditions: vec![Condition {
                last_transition_time: parse_time("2026-05-24T00:00:00Z"),
                message: "ok".to_string(),
                observed_generation: Some(1),
                reason: "ReconcileSucceeded".to_string(),
                status: "True".to_string(),
                type_: "Ready".to_string(),
            }],
            observed_generation: Some(1),
        };
        let json = serde_json::to_value(&s).unwrap();
        let back: ProviderStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn failure_domain_default_attributes_round_trip() {
        let fd = FailureDomain {
            name: "fd1".to_string(),
            labels: BTreeMap::new(),
            attributes: FailureDomainAttributes::default(),
        };
        let json = serde_json::to_value(&fd).unwrap();
        let back: FailureDomain = serde_json::from_value(json).unwrap();
        assert_eq!(back, fd);
    }

    #[test]
    fn failure_domain_missing_name_fails() {
        let err = serde_json::from_str::<FailureDomain>(r#"{}"#);
        assert!(err.is_err());
    }

    // ----------------------------------------------------------------------
    // StorageClassMapping / NetworkClassMapping
    // ----------------------------------------------------------------------

    #[test]
    fn storage_class_mapping_round_trip() {
        let s = sample_storage_class("gold");
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "name": "gold",
                "target": { "datastore": "ds-fast-01" }
            })
        );
        let back: StorageClassMapping = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn storage_class_mapping_missing_name_fails() {
        let err = serde_json::from_str::<StorageClassMapping>(r#"{"target":{}}"#);
        assert!(err.is_err());
    }

    #[test]
    fn network_class_mapping_round_trip() {
        let s = sample_network_class("prod");
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["name"], "prod");
        assert_eq!(json["target"]["portGroup"], "vmnet-prod");
        let back: NetworkClassMapping = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    // ----------------------------------------------------------------------
    // CRD generation (`Provider::crd()`)
    // ----------------------------------------------------------------------

    #[test]
    fn provider_crd_metadata_matches_kube_attributes() {
        let crd = Provider::crd();
        assert_eq!(crd.spec.group, "banlieue.io");
        assert_eq!(crd.spec.names.kind, "Provider");
        assert_eq!(crd.spec.names.plural, "providers");
        assert_eq!(
            crd.spec.scope, "Namespaced",
            "Provider must be namespace-scoped"
        );
        assert!(
            crd.spec
                .versions
                .iter()
                .any(|v| v.name == "v1alpha1" && v.served && v.storage)
        );
    }

    // Parse an RFC3339 timestamp into the meta/v1 `Time` newtype. Goes through
    // serde so we don't need a direct dependency on `jiff` (k8s-openapi 0.27
    // switched its internal time representation from chrono to jiff).
    fn parse_time(rfc3339: &str) -> Time {
        let quoted = format!("\"{rfc3339}\"");
        serde_json::from_str(&quoted).unwrap()
    }
}
