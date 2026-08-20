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
            target: Some(target),
            ..Default::default()
        }
    }

    fn sample_network_class(name: &str) -> NetworkClassMapping {
        let mut target = BTreeMap::new();
        target.insert("portGroup".to_string(), "vmnet-prod".to_string());
        NetworkClassMapping {
            name: name.to_string(),
            target: Some(target),
            ..Default::default()
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
            use_content_library: false,
            failure_domain_name_overrides: Vec::new(),
        };
        let json = serde_json::to_value(&s).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("paused"), "paused=false must be skipped");
        assert!(
            !obj.contains_key("useContentLibrary"),
            "useContentLibrary=false must be skipped"
        );
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
            use_content_library: false,
            failure_domain_name_overrides: Vec::new(),
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
            use_content_library: true,
            failure_domain_name_overrides: Vec::new(),
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["useContentLibrary"], true);
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
        labels.insert("datacenter".to_string(), "dc1".to_string());
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
            workload: None,
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

    #[test]
    fn class_mapping_omits_per_zone_when_empty() {
        let s = sample_storage_class("gold");
        let json = serde_json::to_value(&s).unwrap();
        assert!(!json.as_object().unwrap().contains_key("perZone"));
    }

    // ----------------------------------------------------------------------
    // StorageClassMapping::target_for / NetworkClassMapping::target_for
    // (ADR-0030 per-zone capability targets)
    // ----------------------------------------------------------------------

    fn scoped(datacenter: &str, cluster: &str, key: &str, value: &str) -> ScopedTarget {
        let mut target = BTreeMap::new();
        target.insert(key.to_string(), value.to_string());
        ScopedTarget {
            datacenter: datacenter.to_string(),
            cluster: cluster.to_string(),
            target,
        }
    }

    #[test]
    fn target_for_uses_the_default_when_no_per_zone_entries_exist() {
        let m = sample_network_class("network-01");
        let got = m.target_for("dc-east", "cluster-01").unwrap();
        assert_eq!(got.get("portGroup"), Some(&"vmnet-prod".to_string()));
    }

    #[test]
    fn target_for_prefers_an_exact_per_zone_match_over_the_default() {
        let m = NetworkClassMapping {
            name: "network-01".to_string(),
            per_zone: vec![scoped(
                "dc-east",
                "cluster-02",
                "portGroup",
                "cluster-02-specific-pg",
            )],
            ..sample_network_class("network-01")
        };
        let got = m.target_for("dc-east", "cluster-02").unwrap();
        assert_eq!(
            got.get("portGroup"),
            Some(&"cluster-02-specific-pg".to_string())
        );
    }

    #[test]
    fn target_for_falls_back_to_default_for_a_zone_not_listed_in_per_zone() {
        // The same class, on a cluster NOT covered by any per_zone entry,
        // still resolves via the default target — this is the exact "one
        // VMClass across every cluster of a Provider" case ADR-0030 fixes.
        let m = NetworkClassMapping {
            name: "network-01".to_string(),
            per_zone: vec![scoped(
                "dc-east",
                "cluster-02",
                "portGroup",
                "cluster-02-specific-pg",
            )],
            ..sample_network_class("network-01")
        };
        let got = m.target_for("dc-east", "cluster-01").unwrap();
        assert_eq!(got.get("portGroup"), Some(&"vmnet-prod".to_string()));
    }

    #[test]
    fn target_for_none_when_neither_default_nor_per_zone_covers_the_zone() {
        let m = NetworkClassMapping {
            name: "network-01".to_string(),
            target: None,
            per_zone: vec![scoped(
                "dc-east",
                "cluster-02",
                "portGroup",
                "cluster-02-specific-pg",
            )],
            ..Default::default()
        };
        assert!(m.target_for("dc-east", "cluster-01").is_none());
        assert!(m.target_for("dc-east", "cluster-02").is_some());
    }

    #[test]
    fn storage_class_target_for_has_the_same_precedence() {
        let m = StorageClassMapping {
            name: "gold".to_string(),
            per_zone: vec![scoped(
                "dc-east",
                "cluster-02",
                "datastoreCluster",
                "dsc-cluster-02",
            )],
            ..sample_storage_class("gold")
        };
        assert_eq!(
            m.target_for("dc-east", "cluster-02")
                .and_then(|t| t.get("datastoreCluster")),
            Some(&"dsc-cluster-02".to_string())
        );
        assert_eq!(
            m.target_for("dc-east", "cluster-01")
                .and_then(|t| t.get("datastore")),
            Some(&"ds-fast-01".to_string())
        );
    }

    #[test]
    fn per_zone_round_trips_and_uses_camel_case_key() {
        let m = NetworkClassMapping {
            name: "network-01".to_string(),
            per_zone: vec![scoped("dc-east", "cluster-01", "portGroup", "pg-1")],
            ..Default::default()
        };
        let json = serde_json::to_value(&m).unwrap();
        assert!(!json.as_object().unwrap().contains_key("target"));
        assert_eq!(json["perZone"][0]["datacenter"], "dc-east");
        assert_eq!(json["perZone"][0]["cluster"], "cluster-01");
        assert_eq!(json["perZone"][0]["target"]["portGroup"], "pg-1");
        let back: NetworkClassMapping = serde_json::from_value(json).unwrap();
        assert_eq!(back, m);
    }

    // ----------------------------------------------------------------------
    // NetworkClassMapping::subnet_for (ADR-0032 per-zone subnet shape)
    // ----------------------------------------------------------------------

    fn scoped_subnet(
        datacenter: &str,
        cluster: &str,
        gateway: &str,
        nameservers: &[&str],
        domain: &str,
    ) -> ScopedSubnet {
        ScopedSubnet {
            datacenter: datacenter.to_string(),
            cluster: cluster.to_string(),
            subnet: SubnetShape {
                gateway: Some(gateway.to_string()),
                nameservers: nameservers.iter().map(|s| s.to_string()).collect(),
                domain: Some(domain.to_string()),
            },
        }
    }

    #[test]
    fn subnet_for_is_none_when_nothing_is_declared() {
        let m = sample_network_class("network-01");
        assert!(m.subnet_for("dc-east", "cluster-01").is_none());
    }

    #[test]
    fn subnet_for_uses_the_default_when_no_per_zone_subnet_entries_exist() {
        let m = NetworkClassMapping {
            subnet: Some(SubnetShape {
                gateway: Some("10.0.0.1".to_string()),
                nameservers: vec!["10.0.0.53".to_string()],
                domain: Some("example.internal".to_string()),
            }),
            ..sample_network_class("network-01")
        };
        let got = m.subnet_for("dc-east", "cluster-01").unwrap();
        assert_eq!(got.gateway.as_deref(), Some("10.0.0.1"));
        assert_eq!(got.domain.as_deref(), Some("example.internal"));
    }

    #[test]
    fn subnet_for_prefers_an_exact_per_zone_subnet_match_over_the_default() {
        let m = NetworkClassMapping {
            subnet: Some(SubnetShape {
                gateway: Some("10.0.0.1".to_string()),
                nameservers: vec![],
                domain: None,
            }),
            per_zone_subnet: vec![scoped_subnet(
                "dc-east",
                "cluster-02",
                "10.0.2.1",
                &["10.0.2.53"],
                "cluster-02.internal",
            )],
            ..sample_network_class("network-01")
        };
        let got = m.subnet_for("dc-east", "cluster-02").unwrap();
        assert_eq!(got.gateway.as_deref(), Some("10.0.2.1"));
        assert_eq!(got.domain.as_deref(), Some("cluster-02.internal"));

        // A zone NOT covered by per_zone_subnet still falls back to the
        // default — the same "one class, many clusters" case ADR-0030's
        // target_for exists for.
        let fallback = m.subnet_for("dc-east", "cluster-01").unwrap();
        assert_eq!(fallback.gateway.as_deref(), Some("10.0.0.1"));
    }

    #[test]
    fn subnet_shape_omits_empty_fields() {
        let json = serde_json::to_value(SubnetShape::default()).unwrap();
        assert_eq!(json, serde_json::json!({}));
    }

    #[test]
    fn per_zone_subnet_round_trips_and_uses_camel_case_keys() {
        let m = NetworkClassMapping {
            per_zone_subnet: vec![scoped_subnet(
                "dc-east",
                "cluster-01",
                "10.0.1.1",
                &["10.0.1.53", "10.0.1.54"],
                "cluster-01.internal",
            )],
            ..sample_network_class("network-01")
        };
        let json = serde_json::to_value(&m).unwrap();
        assert!(!json.as_object().unwrap().contains_key("subnet"));
        let pzs = &json["perZoneSubnet"][0];
        assert_eq!(pzs["datacenter"], "dc-east");
        assert_eq!(pzs["cluster"], "cluster-01");
        assert_eq!(pzs["subnet"]["gateway"], "10.0.1.1");
        assert_eq!(pzs["subnet"]["domain"], "cluster-01.internal");
        let back: NetworkClassMapping = serde_json::from_value(json).unwrap();
        assert_eq!(back, m);
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

    #[test]
    fn failure_domain_name_overrides_rejects_duplicate_zone_at_admission() {
        // ADR-0023: an admin can't declare two overrides for the same
        // (datacenter, cluster) pair — the API server enforces list-map-key
        // uniqueness at admission, the same mechanism VMImageSpec.sources[]
        // uses for providerClass.
        let crd = Provider::crd();
        let json = serde_json::to_value(&crd).unwrap();
        let overrides_schema = &json["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]
            ["spec"]["properties"]["failureDomainNameOverrides"];
        assert_eq!(overrides_schema["x-kubernetes-list-type"], "map");
        assert_eq!(
            overrides_schema["x-kubernetes-list-map-keys"][0],
            "datacenter"
        );
        assert_eq!(overrides_schema["x-kubernetes-list-map-keys"][1], "cluster");
    }

    // ----------------------------------------------------------------------
    // FailureDomainNameOverride (ADR-0023)
    // ----------------------------------------------------------------------

    #[test]
    fn failure_domain_name_override_round_trip() {
        let o = FailureDomainNameOverride {
            datacenter: "dc-example".to_string(),
            cluster: "compute-cluster-nonreplicated-01".to_string(),
            name: "cluster-01".to_string(),
        };
        let json = serde_json::to_value(&o).unwrap();
        assert_eq!(json["datacenter"], "dc-example");
        assert_eq!(json["cluster"], "compute-cluster-nonreplicated-01");
        assert_eq!(json["name"], "cluster-01");
        let back: FailureDomainNameOverride = serde_json::from_value(json).unwrap();
        assert_eq!(back, o);
    }

    #[test]
    fn provider_spec_omits_failure_domain_name_overrides_when_empty() {
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
            use_content_library: false,
            failure_domain_name_overrides: Vec::new(),
        };
        let json = serde_json::to_value(&s).unwrap();
        assert!(
            !json
                .as_object()
                .unwrap()
                .contains_key("failureDomainNameOverrides"),
            "empty overrides must be skipped"
        );
    }

    #[test]
    fn provider_spec_with_failure_domain_name_overrides_round_trip() {
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
            use_content_library: false,
            failure_domain_name_overrides: vec![FailureDomainNameOverride {
                datacenter: "dc-example".to_string(),
                cluster: "cluster-example".to_string(),
                name: "cluster-01".to_string(),
            }],
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["failureDomainNameOverrides"][0]["name"], "cluster-01");
        let back: ProviderSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    // ----------------------------------------------------------------------
    // status.workload — owned exclusively by banlieue-operator (ADR-0012)
    // ----------------------------------------------------------------------

    #[test]
    fn status_workload_defaults_to_absent() {
        assert!(ProviderStatus::default().workload.is_none());
    }

    #[test]
    fn status_workload_serializes_as_camel_case() {
        let status = ProviderStatus {
            workload: Some(ProviderWorkloadStatus {
                deployment_name: "banlieue-provider-vsphere-prod-vc".to_string(),
                namespace: "banlieue-system".to_string(),
                ready_replicas: 1,
                observed_generation: Some(4),
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        let workload = &json["workload"];
        assert_eq!(
            workload["deploymentName"],
            "banlieue-provider-vsphere-prod-vc"
        );
        assert_eq!(workload["readyReplicas"], 1);
        assert_eq!(workload["observedGeneration"], 4);
        assert!(workload.get("deployment_name").is_none());
    }

    /// The operator writes `status.workload` while the provider's own field
    /// manager writes `status.conditions`. Serializing a workload-only status
    /// must not emit an empty `conditions`, or server-side apply would have the
    /// operator claim ownership of a list it does not manage.
    #[test]
    fn status_with_only_workload_does_not_emit_conditions() {
        let status = ProviderStatus {
            workload: Some(ProviderWorkloadStatus {
                deployment_name: "banlieue-provider-vsphere-prod-vc".to_string(),
                namespace: "banlieue-system".to_string(),
                ready_replicas: 0,
                observed_generation: None,
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert!(json.get("conditions").is_none());
        assert!(json.get("failureDomains").is_none());
        assert!(json["workload"].get("observedGeneration").is_none());
    }

    #[test]
    fn status_workload_round_trips_through_json() {
        let workload = ProviderWorkloadStatus {
            deployment_name: "banlieue-provider-libvirt-lab".to_string(),
            namespace: "tenant-a".to_string(),
            ready_replicas: 2,
            observed_generation: Some(9),
        };
        let round_tripped: ProviderWorkloadStatus =
            serde_json::from_str(&serde_json::to_string(&workload).unwrap()).unwrap();
        assert_eq!(round_tripped, workload);
    }

    // Parse an RFC3339 timestamp into the meta/v1 `Time` newtype. Goes through
    // serde so we don't need a direct dependency on `jiff` (k8s-openapi 0.27
    // switched its internal time representation from chrono to jiff).
    fn parse_time(rfc3339: &str) -> Time {
        let quoted = format!("\"{rfc3339}\"");
        serde_json::from_str(&quoted).unwrap()
    }
}
