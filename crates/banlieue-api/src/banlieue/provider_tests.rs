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
            instance_sizes: vec![],
        };
        assert!(!c.is_empty());
    }

    #[test]
    fn provider_capabilities_with_network_class_is_not_empty() {
        let c = ProviderCapabilities {
            storage_classes: Vec::new(),
            network_classes: vec![sample_network_class("prod")],
            features: Vec::new(),
            instance_sizes: vec![],
        };
        assert!(!c.is_empty());
    }

    #[test]
    fn provider_capabilities_with_only_features_is_not_empty() {
        let c = ProviderCapabilities {
            storage_classes: Vec::new(),
            network_classes: Vec::new(),
            features: vec!["hotAddCPU".to_string()],
            instance_sizes: vec![],
        };
        assert!(!c.is_empty());
    }

    #[test]
    fn provider_capabilities_with_everything_is_not_empty() {
        let c = ProviderCapabilities {
            storage_classes: vec![sample_storage_class("gold")],
            network_classes: vec![sample_network_class("prod")],
            features: vec!["efiSecureBoot".to_string()],
            instance_sizes: vec![],
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
            credentials_ref: Some(LocalObjectReference {
                name: "vc-creds".to_string(),
            }),
            insecure_skip_tls_verify: false,
            ca_bundle: None,
            azure: None,
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
            azure: None,
            endpoint: "https://pve:8006".to_string(),
            credentials_ref: Some(LocalObjectReference {
                name: "pve-creds".to_string(),
            }),
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

    /// ADR-0060 Decision 4: a host-resident provider has nothing to
    /// authenticate to and must read no Secret, so the reference is optional
    /// — and absent from the serialized form, not `null`.
    #[test]
    fn provider_connection_without_credentials_ref_is_valid_and_omits_it() {
        let c = serde_json::from_str::<ProviderConnection>(r#"{"endpoint":"bar.foo.io"}"#)
            .expect("credentialsRef is optional");
        assert!(c.credentials_ref.is_none());
        let v = serde_json::to_value(&c).unwrap();
        assert!(v.get("credentialsRef").is_none(), "{v}");
    }

    /// ADR-0060 Decision 3: an External workload has no Deployment, so its
    /// status says so and carries no Deployment name or replica count.
    #[test]
    fn an_external_workload_status_has_no_deployment() {
        let s = ProviderWorkloadStatus {
            mode: WorkloadMode::External,
            deployment_name: None,
            namespace: "banlieue-system".into(),
            ready_replicas: None,
            observed_generation: Some(3),
            ..Default::default()
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["mode"], "External");
        assert!(
            v.get("deploymentName").is_none() && v.get("readyReplicas").is_none(),
            "{v}"
        );
        // An object written before `mode` existed reads as Managed.
        let old: ProviderWorkloadStatus =
            serde_json::from_str(r#"{"deploymentName":"d","namespace":"n","readyReplicas":1}"#)
                .unwrap();
        assert_eq!(old.mode, WorkloadMode::Managed);
    }

    #[test]
    fn provider_spec_minimal_serializes_without_paused_or_capabilities() {
        let s = ProviderSpec {
            provider_class_ref: LocalObjectReference {
                name: "vsphere".to_string(),
            },
            connection: ProviderConnection {
                endpoint: "https://vc.example.com/sdk".to_string(),
                credentials_ref: Some(LocalObjectReference {
                    name: "vc-creds".to_string(),
                }),
                insecure_skip_tls_verify: false,
                ca_bundle: None,
                azure: None,
            },
            capabilities: ProviderCapabilities::default(),
            paused: false,
            use_content_library: false,
            failure_domain_name_overrides: Vec::new(),
            attestation: None,
            scheduling: None,
            namespace_selector: None,
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
                credentials_ref: Some(LocalObjectReference {
                    name: "ssh-key".to_string(),
                }),
                insecure_skip_tls_verify: false,
                ca_bundle: None,
                azure: None,
            },
            capabilities: ProviderCapabilities::default(),
            paused: true,
            use_content_library: false,
            failure_domain_name_overrides: Vec::new(),
            attestation: None,
            scheduling: None,
            namespace_selector: None,
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
                credentials_ref: Some(LocalObjectReference {
                    name: "vc".to_string(),
                }),
                insecure_skip_tls_verify: false,
                ca_bundle: None,
                azure: None,
            },
            capabilities: ProviderCapabilities {
                storage_classes: vec![sample_storage_class("gold")],
                network_classes: vec![sample_network_class("prod")],
                features: vec!["hotAddCPU".to_string()],
                instance_sizes: vec![],
            },
            paused: false,
            use_content_library: true,
            failure_domain_name_overrides: Vec::new(),
            attestation: None,
            scheduling: None,
            namespace_selector: None,
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
                    capacity: None,
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
            ek_ca_certificates: vec![],
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
            zone: None,
            target,
        }
    }

    // ----------------------------------------------------------------------
    // Cloud zones (ADR-0158 Decision 3) and instance sizes (ADR-0160)
    // ----------------------------------------------------------------------

    fn zoned(zone: &str, key: &str, value: &str) -> ScopedTarget {
        ScopedTarget {
            zone: Some(zone.to_string()),
            target: BTreeMap::from([(key.to_string(), value.to_string())]),
            ..Default::default()
        }
    }

    #[test]
    fn target_for_zone_prefers_the_zone_entry() {
        let m = NetworkClassMapping {
            name: "lan".to_string(),
            target: Some(BTreeMap::from([(
                "subnetId".to_string(),
                "default".to_string(),
            )])),
            per_zone: vec![zoned("2", "subnetId", "zone-two")],
            ..Default::default()
        };
        assert_eq!(m.target_for_zone("2").unwrap()["subnetId"], "zone-two");
        assert_eq!(m.target_for_zone("1").unwrap()["subnetId"], "default");
    }

    #[test]
    fn target_for_zone_ignores_vsphere_scoped_entries() {
        let m = StorageClassMapping {
            name: "fast".to_string(),
            target: None,
            per_zone: vec![scoped("dc", "cl", "sku", "Premium_LRS")],
        };
        assert_eq!(m.target_for_zone("1"), None);
    }

    #[test]
    fn zoned_scoped_target_serializes_without_datacenter_or_cluster() {
        let json = serde_json::to_value(zoned("1", "sku", "Premium_LRS")).unwrap();
        assert_eq!(json["zone"], "1");
        assert!(json.get("datacenter").is_none());
        assert!(json.get("cluster").is_none());
    }

    #[test]
    fn instance_size_for_uses_per_zone_override_then_default() {
        let caps = ProviderCapabilities {
            instance_sizes: vec![InstanceSizeMapping {
                vm_class: "small".to_string(),
                size: "Standard_D2s_v5".to_string(),
                per_zone: vec![ZonedInstanceSize {
                    zone: "2".to_string(),
                    size: "Standard_D2as_v5".to_string(),
                }],
            }],
            ..Default::default()
        };
        assert_eq!(
            caps.instance_size_for("small", "1"),
            Some("Standard_D2s_v5")
        );
        assert_eq!(
            caps.instance_size_for("small", "2"),
            Some("Standard_D2as_v5")
        );
        assert_eq!(caps.instance_size_for("large", "1"), None);
        assert!(!caps.is_empty());
    }

    #[test]
    fn azure_connection_round_trips_in_camel_case() {
        let conn = AzureConnection {
            tenant_id: "t".to_string(),
            client_id: "c".to_string(),
            subscription_id: "s".to_string(),
            location: "canadacentral".to_string(),
            resource_group: "rg".to_string(),
            authority_host: None,
            image_gallery: None,
            publisher: None,
            ..AzureConnection::default()
        };
        let json = serde_json::to_value(&conn).unwrap();
        assert_eq!(json["resourceGroup"], "rg");
        assert_eq!(json["location"], "canadacentral");
        assert!(json.get("authorityHost").is_none());
        assert!(json.get("imageGallery").is_none());
        assert!(json.get("publisher").is_none());
        let back: AzureConnection = serde_json::from_value(json).unwrap();
        assert_eq!(back, conn);
    }

    /// ADR-0162 Decision 8: the gallery and the publisher identity a Url
    /// source is published with, in camelCase.
    #[test]
    fn azure_connection_carries_the_image_gallery_and_publisher() {
        let json = serde_json::json!({
            "tenantId": "t",
            "clientId": "c",
            "subscriptionId": "s",
            "location": "canadacentral",
            "resourceGroup": "rg",
            "imageGallery": { "resourceGroup": "rg-img", "name": "banlieue" },
            "publisher": { "clientId": "p" },
        });
        let conn: AzureConnection = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            conn.image_gallery,
            Some(AzureImageGallery {
                resource_group: "rg-img".to_string(),
                name: "banlieue".to_string(),
            })
        );
        assert_eq!(
            conn.publisher,
            Some(AzurePublisherIdentity {
                client_id: "p".to_string(),
            })
        );
        assert_eq!(serde_json::to_value(&conn).unwrap(), json);
    }

    /// ADR-0172: extra tags, a disk encryption set and encryption at host,
    /// in camelCase, all omitted when unset.
    #[test]
    fn azure_connection_carries_the_governance_settings() {
        let json = serde_json::json!({
            "tenantId": "t",
            "clientId": "c",
            "subscriptionId": "00000000-0000-0000-0000-000000000000",
            "location": "canadacentral",
            "resourceGroup": "rg",
            "tags": { "cost-centre": "1234", "env": "prod" },
            "diskEncryptionSetId": "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/rg-sec/providers/Microsoft.Compute/diskEncryptionSets/des-1",
            "encryptionAtHost": true,
        });
        let conn: AzureConnection = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            conn.tags.get("cost-centre").map(String::as_str),
            Some("1234")
        );
        assert!(conn.encryption_at_host);
        assert!(
            conn.disk_encryption_set_id
                .as_deref()
                .is_some_and(|id| id.ends_with("/diskEncryptionSets/des-1"))
        );
        assert_eq!(serde_json::to_value(&conn).unwrap(), json);
    }

    /// The schema caps the extra tags where Azure would (ADR-0172).
    #[test]
    fn azure_connection_tags_are_capped_in_the_schema() {
        use kube::CustomResourceExt;
        let crd = Provider::crd();
        let schema = crd.spec.versions[0].schema.clone().unwrap();
        let root = serde_json::to_value(schema.open_api_v3_schema.unwrap()).unwrap();
        let tags = &root["properties"]["spec"]["properties"]["connection"]["properties"]["azure"]["properties"]
            ["tags"];
        assert_eq!(tags["maxProperties"], 45, "got {tags}");
    }

    #[test]
    fn azure_connection_omits_unset_governance_settings() {
        let json = serde_json::to_value(AzureConnection::default()).unwrap();
        assert!(json.get("tags").is_none());
        assert!(json.get("diskEncryptionSetId").is_none());
        assert!(json.get("encryptionAtHost").is_none());
    }

    /// ADR-0176 Decision 8: the OpenBao block, in camelCase, with the audience
    /// defaulting to `openbao` and the namespace optional.
    #[test]
    fn azure_openbao_round_trips_and_defaults_the_audience() {
        let json = serde_json::json!({
            "address": "https://openbao.example.com",
            "namespace": "banlieue",
            "authMount": "k8s-jwt",
            "loginRole": "azure-az-prod",
            "tokenRole": "azure-az-prod",
            "audience": "openbao",
        });
        let bao: AzureOpenBao = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(bao.address, "https://openbao.example.com");
        assert_eq!(bao.namespace.as_deref(), Some("banlieue"));
        assert_eq!(serde_json::to_value(&bao).unwrap(), json);

        let minimal: AzureOpenBao = serde_json::from_value(serde_json::json!({
            "address": "https://openbao.example.com",
            "authMount": "k8s-jwt",
            "loginRole": "r",
            "tokenRole": "r",
        }))
        .unwrap();
        assert_eq!(minimal.audience, DEFAULT_OPENBAO_AUDIENCE);
        assert_eq!(minimal.audience, "openbao");
        assert_eq!(minimal.namespace, None);
        assert!(
            serde_json::to_value(&minimal)
                .unwrap()
                .get("namespace")
                .is_none()
        );
    }

    #[test]
    fn azure_connection_carries_the_openbao_block_and_omits_it_when_unset() {
        assert!(
            serde_json::to_value(AzureConnection::default())
                .unwrap()
                .get("openbao")
                .is_none()
        );
        let conn: AzureConnection = serde_json::from_value(serde_json::json!({
            "tenantId": "t",
            "clientId": "c",
            "subscriptionId": "s",
            "location": "canadacentral",
            "resourceGroup": "rg",
            "openbao": {
                "address": "https://openbao.example.com",
                "authMount": "k8s-jwt",
                "loginRole": "r",
                "tokenRole": "r",
            },
        }))
        .unwrap();
        assert_eq!(
            conn.openbao.as_ref().map(|b| b.auth_mount.as_str()),
            Some("k8s-jwt")
        );
    }

    fn openbao_schema() -> serde_json::Value {
        use kube::CustomResourceExt;
        let crd = Provider::crd();
        let schema = crd.spec.versions[0].schema.clone().unwrap();
        let root = serde_json::to_value(schema.open_api_v3_schema.unwrap()).unwrap();
        root["properties"]["spec"]["properties"]["connection"]["properties"]["azure"]["properties"]
            ["openbao"]
            .clone()
    }

    /// The schema refuses what the client would refuse (ADR-0176 Decision 8):
    /// a non-https address and names that could change a request path.
    #[test]
    fn the_openbao_schema_requires_https_and_safe_names() {
        let schema = openbao_schema();
        assert_ne!(
            schema,
            serde_json::Value::Null,
            "no openbao block in the CRD"
        );
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        for name in ["address", "authMount", "loginRole", "tokenRole"] {
            assert!(
                required.contains(&name),
                "{name} not required: {required:?}"
            );
        }
        assert!(!required.contains(&"namespace"));
        assert!(!required.contains(&"audience"), "audience has a default");

        let props = &schema["properties"];
        let address = props["address"]["pattern"].as_str().unwrap();
        assert!(address.starts_with("^https://"), "{address}");
        for name in ["namespace", "authMount", "loginRole", "tokenRole"] {
            let pattern = props[name]["pattern"].as_str().unwrap_or_default();
            assert!(
                pattern.starts_with("^[A-Za-z0-9._-]+"),
                "{name} pattern {pattern:?}"
            );
            let rules = props[name]["x-kubernetes-validations"]
                .as_array()
                .unwrap_or_else(|| panic!("{name} has no traversal rule"));
            assert!(
                rules
                    .iter()
                    .any(|r| r["rule"].as_str().is_some_and(|r| r.contains("'..'"))),
                "{name}: {rules:?}"
            );
        }
        assert_eq!(props["audience"]["default"], "openbao");

        // The login token must never be a cloud-audience token (ADR-0176
        // Decision 8): the schema refuses the cloud audience.
        let rules = props["audience"]["x-kubernetes-validations"]
            .as_array()
            .expect("audience has no rule against the cloud audience");
        assert!(
            rules.iter().any(|r| r["rule"]
                .as_str()
                .is_some_and(|r| r.contains("api://AzureADTokenExchange"))),
            "{rules:?}"
        );
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
                credentials_ref: Some(LocalObjectReference {
                    name: "vc-creds".to_string(),
                }),
                insecure_skip_tls_verify: false,
                ca_bundle: None,
                azure: None,
            },
            capabilities: ProviderCapabilities::default(),
            paused: false,
            use_content_library: false,
            failure_domain_name_overrides: Vec::new(),
            attestation: None,
            scheduling: None,
            namespace_selector: None,
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
                credentials_ref: Some(LocalObjectReference {
                    name: "vc-creds".to_string(),
                }),
                insecure_skip_tls_verify: false,
                ca_bundle: None,
                azure: None,
            },
            capabilities: ProviderCapabilities::default(),
            paused: false,
            use_content_library: false,
            failure_domain_name_overrides: vec![FailureDomainNameOverride {
                datacenter: "dc-example".to_string(),
                cluster: "cluster-example".to_string(),
                name: "cluster-01".to_string(),
            }],
            attestation: None,
            scheduling: None,
            namespace_selector: None,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["failureDomainNameOverrides"][0]["name"], "cluster-01");
        let back: ProviderSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    // ----------------------------------------------------------------------
    // spec.attestation — EK trust anchors (ADR-0049 Decision 10)
    // ----------------------------------------------------------------------

    fn minimal_spec() -> ProviderSpec {
        ProviderSpec {
            provider_class_ref: LocalObjectReference {
                name: "libvirt".to_string(),
            },
            connection: ProviderConnection {
                endpoint: "qemu://kvm-host.example.com/system".to_string(),
                credentials_ref: Some(LocalObjectReference {
                    name: "libvirt-creds".to_string(),
                }),
                insecure_skip_tls_verify: false,
                ca_bundle: None,
                azure: None,
            },
            capabilities: ProviderCapabilities::default(),
            paused: false,
            use_content_library: false,
            failure_domain_name_overrides: Vec::new(),
            attestation: None,
            scheduling: None,
            namespace_selector: None,
        }
    }

    #[test]
    fn provider_spec_omits_attestation_when_absent() {
        let json = serde_json::to_value(minimal_spec()).unwrap();
        assert!(
            !json.as_object().unwrap().contains_key("attestation"),
            "absent attestation must be skipped, not serialized as null"
        );
    }

    #[test]
    fn attestation_ek_trust_bundle_round_trips_and_uses_camel_case_keys() {
        let s = ProviderSpec {
            attestation: Some(ProviderAttestation {
                ek_trust_bundle: Some(CABundleSource {
                    config_map_ref: Some(KeySelector {
                        name: "swtpm-issuers".to_string(),
                        key: Some("ek-ca.crt".to_string()),
                    }),
                    ..Default::default()
                }),
                host_ca: None,
            }),
            ..minimal_spec()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json["attestation"]["ekTrustBundle"]["configMapRef"]["name"], "swtpm-issuers",
            "field must serialize as attestation.ekTrustBundle (camelCase)"
        );
        let back: ProviderSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn attestation_ek_trust_bundle_accepts_inline_pem() {
        let yaml = r#"
attestation:
  ekTrustBundle:
    inline: |
      -----BEGIN CERTIFICATE-----
      MIIB...
      -----END CERTIFICATE-----
"#;
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Partial {
            attestation: ProviderAttestation,
        }
        let p: Partial = serde_saphyr::from_str(yaml).unwrap();
        let bundle = p.attestation.ek_trust_bundle.as_ref().unwrap();
        assert!(bundle.inline.is_some());
        assert!(bundle.validate().is_ok());
    }

    #[test]
    fn attestation_ek_trust_bundle_reuses_the_exactly_one_source_invariant() {
        let a = ProviderAttestation {
            ek_trust_bundle: Some(CABundleSource {
                inline: Some("pem".to_string()),
                config_map_ref: Some(KeySelector {
                    name: "also-set".to_string(),
                    key: None,
                }),
                ..Default::default()
            }),
            host_ca: None,
        };
        assert!(
            a.ek_trust_bundle.as_ref().unwrap().validate().is_err(),
            "two sources must fail the CABundleSource invariant"
        );
    }

    #[test]
    fn provider_crd_schema_declares_attestation_ek_trust_bundle() {
        let crd = Provider::crd();
        let json = serde_json::to_value(&crd).unwrap();
        let spec_props = &json["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
            ["properties"];
        let bundle = &spec_props["attestation"]["properties"]["ekTrustBundle"];
        assert!(
            bundle.is_object(),
            "CRD schema must declare spec.attestation.ekTrustBundle"
        );
        for source in ["inline", "configMapRef", "secretRef"] {
            assert!(
                bundle["properties"][source].is_object(),
                "ekTrustBundle must carry the CABundleSource shape ({source})"
            );
        }
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
                mode: WorkloadMode::Managed,
                deployment_name: Some("banlieue-provider-vsphere-prod-vc".to_string()),
                namespace: "banlieue-system".to_string(),
                ready_replicas: Some(1),
                observed_generation: Some(4),
                ..Default::default()
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
                mode: WorkloadMode::Managed,
                deployment_name: Some("banlieue-provider-vsphere-prod-vc".to_string()),
                namespace: "banlieue-system".to_string(),
                ready_replicas: Some(0),
                observed_generation: None,
                ..Default::default()
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
            mode: WorkloadMode::Managed,
            deployment_name: Some("banlieue-provider-libvirt-lab".to_string()),
            service_account_name: Some("banlieue-provider-libvirt-lab".to_string()),
            namespace: "tenant-a".to_string(),
            ready_replicas: Some(2),
            observed_generation: Some(9),
            heartbeat: Some(Heartbeat::Alive),
            last_heartbeat_time: Some(parse_time("2026-10-15T08:00:00Z")),
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

    // ----------------------------------------------------------------------
    // status.workload.heartbeat: provider liveness from its Lease (ADR-0102)
    // ----------------------------------------------------------------------

    #[test]
    fn heartbeat_serializes_with_its_last_renewal() {
        let workload = ProviderWorkloadStatus {
            mode: WorkloadMode::External,
            namespace: "banlieue-system".to_string(),
            heartbeat: Some(Heartbeat::Stale),
            last_heartbeat_time: Some(parse_time("2026-10-13T12:57:53Z")),
            ..Default::default()
        };
        let v = serde_json::to_value(&workload).unwrap();
        assert_eq!(v["heartbeat"], "Stale");
        assert_eq!(v["lastHeartbeatTime"], "2026-10-13T12:57:53Z");
    }

    /// A workload status written before ADR-0102 has no heartbeat, and must
    /// read as "not evaluated", never as any of the three states.
    #[test]
    fn a_workload_status_from_before_the_heartbeat_has_none() {
        let old: ProviderWorkloadStatus =
            serde_json::from_str(r#"{"namespace":"banlieue-system","mode":"External"}"#).unwrap();
        assert!(old.heartbeat.is_none());
        assert!(old.last_heartbeat_time.is_none());
        let v = serde_json::to_value(&old).unwrap();
        assert!(v.get("heartbeat").is_none(), "{v}");
    }

    #[test]
    fn every_heartbeat_state_has_a_stable_wire_name() {
        for (state, wire) in [
            (Heartbeat::Alive, "Alive"),
            (Heartbeat::Stale, "Stale"),
            (Heartbeat::Absent, "Absent"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), wire);
        }
    }

    #[test]
    fn kubectl_get_providers_shows_the_heartbeat() {
        let crd = Provider::crd();
        let columns = crd.spec.versions[0]
            .additional_printer_columns
            .as_ref()
            .expect("printer columns");
        let heartbeat = columns
            .iter()
            .find(|c| c.name == "Heartbeat")
            .expect("a Heartbeat column");
        assert_eq!(heartbeat.json_path, ".status.workload.heartbeat");
    }

    // ------------------------------------------------ capacity (ADR-0107)

    /// A provider that publishes no capacity writes nothing, so a backend
    /// that does not know its capacity is unchanged on the wire.
    #[test]
    fn attributes_without_capacity_omit_it() {
        let v = serde_json::to_value(FailureDomainAttributes::default()).unwrap();
        assert!(v.get("capacity").is_none(), "{v}");
    }

    #[test]
    fn capacity_round_trips_with_camel_case_fields() {
        let c = FailureDomainCapacity {
            cpus: Some(96),
            memory_mi_b: Some(204_800),
            memory_overhead_mi_b_per_vm: Some(512),
            ..FailureDomainCapacity::default()
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"cpus": 96, "memoryMiB": 204_800, "memoryOverheadMiBPerVM": 512})
        );
        assert_eq!(
            serde_json::from_value::<FailureDomainCapacity>(v).unwrap(),
            c
        );
    }

    /// Every figure is optional: a backend fills in what it knows.
    #[test]
    fn a_partial_capacity_is_valid() {
        let c: FailureDomainCapacity =
            serde_json::from_value(serde_json::json!({"memoryMiB": 1024})).unwrap();
        assert_eq!(c.cpus, None);
        assert_eq!(c.memory_mi_b, Some(1024));
        assert_eq!(c.memory_overhead_mi_b_per_vm, None);
    }

    /// No `scheduling` block means the documented default ratio.
    #[test]
    fn the_cpu_overcommit_ratio_defaults_when_unset() {
        assert_eq!(
            ProviderScheduling::default().cpu_overcommit_ratio(),
            DEFAULT_CPU_OVERCOMMIT_RATIO
        );
        let explicit = ProviderScheduling {
            cpu_overcommit_ratio: Some(1),
            ..ProviderScheduling::default()
        };
        assert_eq!(explicit.cpu_overcommit_ratio(), 1);
        assert_eq!(DEFAULT_CPU_OVERCOMMIT_RATIO, 4);
    }

    // ------------------------------------- measured capacity (ADR-0128)

    #[test]
    fn measured_capacity_round_trips_with_camel_case_fields() {
        let c = FailureDomainCapacity {
            cpus: Some(32),
            memory_mi_b: Some(61_440),
            memory_overhead_mi_b_per_vm: Some(512),
            memory_available_mi_b: Some(40_960),
            storage: vec![StorageCapacity {
                storage_class: "fast".into(),
                total_gi_b: Some(718),
                free_gi_b: Some(512),
                shared: true,
            }],
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "cpus": 32,
                "memoryMiB": 61_440,
                "memoryOverheadMiBPerVM": 512,
                "memoryAvailableMiB": 40_960,
                "storage": [{
                    "storageClass": "fast",
                    "totalGiB": 718,
                    "freeGiB": 512,
                    "shared": true
                }]
            })
        );
        assert_eq!(
            serde_json::from_value::<FailureDomainCapacity>(v).unwrap(),
            c
        );
    }

    /// A provider that measures nothing new writes nothing new: ADR-0107's
    /// wire form is unchanged, and a store that is not shared says nothing.
    #[test]
    fn unmeasured_figures_and_unshared_storage_are_omitted() {
        let c = FailureDomainCapacity {
            memory_mi_b: Some(1024),
            storage: vec![StorageCapacity {
                storage_class: "local".into(),
                free_gi_b: Some(10),
                ..StorageCapacity::default()
            }],
            ..FailureDomainCapacity::default()
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "memoryMiB": 1024,
                "storage": [{"storageClass": "local", "freeGiB": 10}]
            })
        );
        let empty = serde_json::to_value(FailureDomainCapacity::default()).unwrap();
        assert_eq!(empty, serde_json::json!({}));
    }

    #[test]
    fn storage_capacity_looks_up_a_class() {
        let c = FailureDomainCapacity {
            storage: vec![
                StorageCapacity {
                    storage_class: "a".into(),
                    free_gi_b: Some(1),
                    ..StorageCapacity::default()
                },
                StorageCapacity {
                    storage_class: "b".into(),
                    free_gi_b: Some(2),
                    ..StorageCapacity::default()
                },
            ],
            ..FailureDomainCapacity::default()
        };
        assert_eq!(c.storage_for("b").and_then(|s| s.free_gi_b), Some(2));
        assert!(c.storage_for("c").is_none());
    }

    #[test]
    fn the_storage_reserve_defaults_when_unset() {
        assert_eq!(
            ProviderScheduling::default().storage_reserve_percent(),
            DEFAULT_STORAGE_RESERVE_PERCENT
        );
        let explicit = ProviderScheduling {
            storage_reserve_percent: Some(0),
            ..ProviderScheduling::default()
        };
        assert_eq!(explicit.storage_reserve_percent(), 0);
        assert_eq!(DEFAULT_STORAGE_RESERVE_PERCENT, 10);
    }

    #[test]
    fn the_crd_bounds_the_storage_reserve() {
        let crd = serde_json::to_value(Provider::crd()).unwrap();
        let reserve = &crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
            ["properties"]["scheduling"]["properties"]["storageReservePercent"];
        assert_eq!(reserve["minimum"].as_f64(), Some(0.0), "{reserve}");
        assert_eq!(
            reserve["maximum"].as_f64(),
            Some(f64::from(MAX_STORAGE_RESERVE_PERCENT)),
            "{reserve}"
        );
        assert_eq!(MAX_STORAGE_RESERVE_PERCENT, 50);
    }

    /// The API server enforces the ratio's range, so a typo cannot disable
    /// the check (0) or make it meaningless (a huge value).
    #[test]
    fn the_crd_bounds_the_cpu_overcommit_ratio() {
        let crd = serde_json::to_value(Provider::crd()).unwrap();
        let ratio = &crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
            ["properties"]["scheduling"]["properties"]["cpuOvercommitRatio"];
        assert_eq!(
            ratio["minimum"].as_f64(),
            Some(f64::from(MIN_CPU_OVERCOMMIT_RATIO)),
            "{ratio}"
        );
        assert_eq!(
            ratio["maximum"].as_f64(),
            Some(f64::from(MAX_CPU_OVERCOMMIT_RATIO)),
            "{ratio}"
        );
    }

    // ----------------------------------------------------------------------
    // spec.namespaceSelector — the namespaces a Provider serves (ADR-0133)
    // ----------------------------------------------------------------------

    /// Absent means "my own namespace only": a Provider written before
    /// ADR-0133 must not start serving anyone else.
    #[test]
    fn provider_spec_omits_namespace_selector_when_absent() {
        let json = serde_json::to_value(minimal_spec()).unwrap();
        assert!(!json.as_object().unwrap().contains_key("namespaceSelector"));
    }

    #[test]
    fn provider_spec_namespace_selector_round_trips() {
        let mut s = minimal_spec();
        s.namespace_selector = Some(LabelSelector {
            match_labels: BTreeMap::from([("banlieue.io/tenant".to_string(), "true".to_string())]),
            ..Default::default()
        });
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json["namespaceSelector"]["matchLabels"]["banlieue.io/tenant"],
            "true"
        );
        let back: ProviderSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back.namespace_selector, s.namespace_selector);
    }

    #[test]
    fn provider_crd_schema_declares_namespace_selector() {
        let crd = serde_json::to_value(Provider::crd()).unwrap();
        let props = &crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
            ["properties"];
        assert!(props.get("namespaceSelector").is_some());
    }

    // ----------------------------------------------------------------------
    // attestation.hostCa (ADR-0143 Decision 2)
    // ----------------------------------------------------------------------

    const BEGIN_CERT: &str = "-----BEGIN CERTIFICATE-----";
    const END_CERT: &str = "-----END CERTIFICATE-----";

    fn one_cert() -> String {
        format!("{BEGIN_CERT}\nMIIB\n{END_CERT}\n")
    }

    fn host_ca_inline(pem: String) -> ProviderAttestation {
        ProviderAttestation {
            ek_trust_bundle: None,
            host_ca: Some(CABundleSource {
                inline: Some(pem),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn attestation_host_ca_round_trips_and_uses_camel_case_keys() {
        let a = host_ca_inline(one_cert());
        let json = serde_json::to_value(&a).unwrap();
        assert!(json["hostCa"]["inline"].is_string(), "{json}");
        assert!(
            json.get("ekTrustBundle").is_none(),
            "absent ekTrustBundle must be skipped: {json}"
        );
        let back: ProviderAttestation = serde_json::from_value(json).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn attestation_with_only_ek_trust_bundle_still_parses() {
        // Existing Providers carry ekTrustBundle alone.
        let json = serde_json::json!({ "ekTrustBundle": { "inline": one_cert() } });
        let a: ProviderAttestation = serde_json::from_value(json).unwrap();
        assert!(a.host_ca.is_none());
        assert!(a.validate().is_ok());
    }

    #[test]
    fn attestation_validate_accepts_one_inline_certificate() {
        assert_eq!(host_ca_inline(one_cert()).validate(), Ok(()));
    }

    #[test]
    fn attestation_validate_accepts_a_reference_without_reading_it() {
        let a = ProviderAttestation {
            ek_trust_bundle: None,
            host_ca: Some(CABundleSource {
                config_map_ref: Some(KeySelector {
                    name: "fc-host-ca".to_string(),
                    key: None,
                }),
                ..Default::default()
            }),
        };
        assert_eq!(a.validate(), Ok(()));
    }

    #[test]
    fn attestation_validate_rejects_no_source_and_two_sources_for_host_ca() {
        let none = ProviderAttestation {
            ek_trust_bundle: None,
            host_ca: Some(CABundleSource::default()),
        };
        assert!(none.validate().is_err());
        let two = ProviderAttestation {
            ek_trust_bundle: None,
            host_ca: Some(CABundleSource {
                inline: Some(one_cert()),
                secret_ref: Some(KeySelector {
                    name: "also".to_string(),
                    key: None,
                }),
                ..Default::default()
            }),
        };
        assert!(two.validate().is_err());
    }

    #[test]
    fn attestation_validate_rejects_two_inline_certificates() {
        let two = format!("{}{}", one_cert(), one_cert());
        let err = host_ca_inline(two).validate().unwrap_err();
        assert!(err.contains("exactly one certificate"), "{err}");
    }

    #[test]
    fn attestation_validate_rejects_inline_without_a_certificate() {
        let err = host_ca_inline("not a pem".to_string())
            .validate()
            .unwrap_err();
        assert!(err.contains("exactly one certificate"), "{err}");
    }

    #[test]
    fn attestation_validate_rejects_a_private_key_block_beside_the_certificate() {
        let key_kind = ["PRIVATE", "KEY"].join(" ");
        let pem = format!(
            "{}-----BEGIN {key_kind}-----\nAAAA\n-----END {key_kind}-----\n",
            one_cert()
        );
        let err = host_ca_inline(pem).validate().unwrap_err();
        assert!(err.contains("private key"), "{err}");
    }

    #[test]
    fn attestation_validate_rejects_other_private_key_flavours() {
        for kind in ["RSA PRIVATE KEY", "EC PRIVATE KEY", "ENCRYPTED PRIVATE KEY"] {
            let pem = format!("{}-----BEGIN {kind}-----\nAAAA\n", one_cert());
            assert!(host_ca_inline(pem).validate().is_err(), "{kind}");
        }
    }

    #[test]
    fn attestation_validate_rejects_an_empty_attestation() {
        let a = ProviderAttestation {
            ek_trust_bundle: None,
            host_ca: None,
        };
        assert!(a.validate().is_err());
    }

    #[test]
    fn attestation_validate_still_applies_the_one_source_rule_to_ek_trust_bundle() {
        let a = ProviderAttestation {
            ek_trust_bundle: Some(CABundleSource::default()),
            host_ca: None,
        };
        assert!(a.validate().is_err());
    }

    #[test]
    fn provider_crd_schema_declares_host_ca_and_no_longer_requires_ek_trust_bundle() {
        let json = serde_json::to_value(Provider::crd()).unwrap();
        let attestation = &json["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
            ["properties"]["attestation"];
        assert!(attestation["properties"]["hostCa"].is_object());
        let required = attestation["required"].as_array();
        assert!(
            required.is_none_or(|r| !r.iter().any(|v| v == "ekTrustBundle")),
            "a firecracker Provider has no EK trust bundle: {attestation}"
        );
    }
}
