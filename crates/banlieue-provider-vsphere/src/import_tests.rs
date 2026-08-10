// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the pure helpers of the `image-import` subcommand.
//!
//! The vCenter mutation itself ([`crate::client::VSphereClient::import_iso_template`])
//! is verified against a live vCenter, not here; these cover the zone-selection
//! and guest-id rules, which are pure functions.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use banlieue_api::banlieue::{
        FailureDomain, FailureDomainAttributes, NetworkClassMapping, Provider,
        ProviderCapabilities, ProviderConnection, ProviderSpec, ProviderStatus,
        StorageClassMapping,
    };
    use banlieue_api::common::LocalObjectReference;
    use kube::api::ObjectMeta;

    use super::super::{ZonePlan, guest_id_for, resolve_concrete_datastore, resolve_zone};
    use crate::client::Datastore;

    fn target(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn provider_with_zone() -> Provider {
        let mut raw = BTreeMap::new();
        raw.insert("datacenter".to_string(), "dc-east".to_string());
        raw.insert("cluster".to_string(), "cluster-a".to_string());

        Provider {
            metadata: ObjectMeta {
                name: Some("vc".to_string()),
                namespace: Some("banlieue-system".to_string()),
                ..Default::default()
            },
            spec: ProviderSpec {
                provider_class_ref: LocalObjectReference {
                    name: "vsphere".to_string(),
                },
                connection: ProviderConnection {
                    endpoint: "https://vc/sdk".to_string(),
                    credentials_ref: LocalObjectReference {
                        name: "creds".to_string(),
                    },
                    insecure_skip_tls_verify: false,
                    ca_bundle: None,
                },
                capabilities: ProviderCapabilities {
                    storage_classes: vec![StorageClassMapping {
                        name: "gold".to_string(),
                        target: target(&[("datastore", "ds-fast-01")]),
                    }],
                    network_classes: vec![NetworkClassMapping {
                        name: "prod".to_string(),
                        target: target(&[("distributedPortGroup", "pg-prod")]),
                    }],
                    features: Vec::new(),
                },
                paused: false,
                use_content_library: false,
            },
            status: Some(ProviderStatus {
                failure_domains: vec![FailureDomain {
                    name: "vc-dc-east-cluster-a".to_string(),
                    labels: Default::default(),
                    attributes: FailureDomainAttributes {
                        raw,
                        available_storage_classes: vec!["gold".to_string()],
                        available_network_classes: vec!["prod".to_string()],
                        ..Default::default()
                    },
                }],
                conditions: vec![],
                workload: None,
                observed_generation: Some(1),
            }),
        }
    }

    #[test]
    fn resolve_zone_maps_fd_to_concrete_placement() {
        let p = provider_with_zone();
        let plan = resolve_zone(&p, "vc-dc-east-cluster-a", None, None).expect("zone resolves");
        assert_eq!(
            plan,
            ZonePlan {
                datacenter: "dc-east".to_string(),
                cluster: "cluster-a".to_string(),
                datastore: "ds-fast-01".to_string(),
                network: "pg-prod".to_string(),
            }
        );
    }

    #[test]
    fn resolve_zone_errors_on_unknown_failure_domain() {
        let p = provider_with_zone();
        assert!(resolve_zone(&p, "nope", None, None).is_err());
    }

    #[test]
    fn resolve_zone_errors_when_no_storage_class_reachable() {
        let mut p = provider_with_zone();
        p.status.as_mut().unwrap().failure_domains[0]
            .attributes
            .available_storage_classes
            .clear();
        assert!(resolve_zone(&p, "vc-dc-east-cluster-a", None, None).is_err());
    }

    #[test]
    fn resolve_zone_overrides_bypass_capability_introspection() {
        // A failure domain with NO enriched storage/network classes (the
        // on-cluster-before-rebuild case) still resolves when the operator
        // passes explicit --datastore / --network. datacenter/cluster still
        // come from the failure domain's discovered attributes.
        let mut p = provider_with_zone();
        {
            let attrs = &mut p.status.as_mut().unwrap().failure_domains[0].attributes;
            attrs.available_storage_classes.clear();
            attrs.available_network_classes.clear();
        }
        let plan = resolve_zone(
            &p,
            "vc-dc-east-cluster-a",
            Some("DS001"),
            Some("VM Network"),
        )
        .expect("overrides resolve without capability introspection");
        assert_eq!(
            plan,
            ZonePlan {
                datacenter: "dc-east".to_string(),
                cluster: "cluster-a".to_string(),
                datastore: "DS001".to_string(),
                network: "VM Network".to_string(),
            }
        );
    }

    fn ds(name: &str, cluster: Option<&str>, free_space_bytes: Option<i64>) -> Datastore {
        Datastore {
            name: name.to_string(),
            moref: format!("datastore-{name}"),
            datastore_cluster: cluster.map(str::to_string),
            free_space_bytes,
        }
    }

    #[test]
    fn resolve_concrete_datastore_exact_name_wins() {
        let dss = vec![
            ds("DS001", Some("DSC-01"), None),
            ds("DS002", Some("DSC-01"), None),
            ds("local-1", None, None),
        ];
        assert_eq!(resolve_concrete_datastore(&dss, "DS002").unwrap(), "DS002");
        assert_eq!(
            resolve_concrete_datastore(&dss, "local-1").unwrap(),
            "local-1"
        );
    }

    #[test]
    fn resolve_concrete_datastore_maps_cluster_to_emptiest_member() {
        // A datastore-cluster (StoragePod) name resolves to the member with the
        // MOST free space, so the import lands where there is room.
        const GB: i64 = 1024 * 1024 * 1024;
        let dss = vec![
            ds("DS001", Some("DSC-01"), Some(10 * GB)),
            ds("DS002", Some("DSC-01"), Some(90 * GB)), // most free -> chosen
            ds("DS003", Some("DSC-01"), Some(50 * GB)),
            ds("other", Some("DSC-99"), Some(1000 * GB)), // different cluster, ignored
        ];
        assert_eq!(resolve_concrete_datastore(&dss, "DSC-01").unwrap(), "DS002");
    }

    #[test]
    fn resolve_concrete_datastore_ties_break_lexicographically() {
        // Equal (or unknown) free space -> smallest name, so re-runs are stable.
        let dss = vec![
            ds("DS002", Some("DSC-01"), None),
            ds("DS001", Some("DSC-01"), None),
        ];
        assert_eq!(resolve_concrete_datastore(&dss, "DSC-01").unwrap(), "DS001");
    }

    #[test]
    fn resolve_concrete_datastore_errors_when_unknown() {
        let dss = vec![ds("DS001", Some("DSC-01"), None)];
        assert!(resolve_concrete_datastore(&dss, "nope").is_err());
    }

    #[test]
    fn guest_id_for_maps_common_distributions() {
        assert_eq!(guest_id_for("linux", "rhel"), "rhel9_64Guest");
        assert_eq!(guest_id_for("linux", "ubuntu"), "ubuntu64Guest");
        assert_eq!(guest_id_for("linux", "debian"), "debian11_64Guest");
        assert_eq!(guest_id_for("linux", "fedora-coreos"), "fedora64Guest");
        assert_eq!(guest_id_for("linux", "alpine"), "otherLinux64Guest");
        assert_eq!(guest_id_for("windows", "server"), "windows2019srv_64Guest");
        assert_eq!(guest_id_for("bsd", "freebsd"), "otherGuest64");
    }
}
