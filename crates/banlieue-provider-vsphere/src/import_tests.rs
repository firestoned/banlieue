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
        FailureDomain, FailureDomainAttributes, NetworkClassMapping, NicAdapter, Provider,
        ProviderCapabilities, ProviderConnection, ProviderSpec, ProviderStatus, ScopedTarget,
        StorageClassMapping, VMImageTemplateNic,
    };
    use banlieue_api::common::LocalObjectReference;
    use kube::api::ObjectMeta;

    use super::super::{
        ResolvedNic, ZonePlan, effective_folder, guest_id_for, resolve_concrete_datastore,
        resolve_nic_networks, resolve_zone, upload_progress_milestones,
    };
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
                        target: Some(target(&[("datastore", "ds-fast-01")])),
                        ..Default::default()
                    }],
                    network_classes: vec![NetworkClassMapping {
                        name: "prod".to_string(),
                        target: Some(target(&[("distributedPortGroup", "pg-prod")])),
                        ..Default::default()
                    }],
                    features: Vec::new(),
                },
                paused: false,
                use_content_library: false,
                failure_domain_name_overrides: Vec::new(),
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
        let plan = resolve_zone(&p, "vc-dc-east-cluster-a", None).expect("zone resolves");
        assert_eq!(
            plan,
            ZonePlan {
                datacenter: "dc-east".to_string(),
                cluster: "cluster-a".to_string(),
                datastore: "ds-fast-01".to_string(),
            }
        );
    }

    #[test]
    fn resolve_zone_errors_on_unknown_failure_domain() {
        let p = provider_with_zone();
        assert!(resolve_zone(&p, "nope", None).is_err());
    }

    #[test]
    fn resolve_zone_errors_when_no_storage_class_reachable() {
        let mut p = provider_with_zone();
        p.status.as_mut().unwrap().failure_domains[0]
            .attributes
            .available_storage_classes
            .clear();
        assert!(resolve_zone(&p, "vc-dc-east-cluster-a", None).is_err());
    }

    #[test]
    fn resolve_zone_override_bypasses_capability_introspection() {
        // A failure domain with NO enriched storage classes (the
        // on-cluster-before-rebuild case) still resolves when the operator
        // passes an explicit --datastore. datacenter/cluster still come
        // from the failure domain's discovered attributes.
        let mut p = provider_with_zone();
        p.status.as_mut().unwrap().failure_domains[0]
            .attributes
            .available_storage_classes
            .clear();
        let plan = resolve_zone(&p, "vc-dc-east-cluster-a", Some("DS001"))
            .expect("override resolves without capability introspection");
        assert_eq!(
            plan,
            ZonePlan {
                datacenter: "dc-east".to_string(),
                cluster: "cluster-a".to_string(),
                datastore: "DS001".to_string(),
            }
        );
    }

    // ------------------------------------------------------------------
    // ADR-0030: per-zone capability targets
    // ------------------------------------------------------------------

    #[test]
    fn resolve_zone_uses_the_per_zone_override_datastore_for_this_cluster() {
        // The SAME abstract "gold" class resolves to a different concrete
        // datastore on cluster-a (via per_zone) than its Provider-wide
        // default — exactly the "one class, many clusters" case ADR-0030
        // exists to fix.
        let mut p = provider_with_zone();
        p.spec.capabilities.storage_classes[0].per_zone = vec![ScopedTarget {
            datacenter: "dc-east".to_string(),
            cluster: "cluster-a".to_string(),
            target: target(&[("datastore", "ds-cluster-a-specific")]),
        }];
        let plan = resolve_zone(&p, "vc-dc-east-cluster-a", None).expect("zone resolves");
        assert_eq!(plan.datastore, "ds-cluster-a-specific");
    }

    #[test]
    fn resolve_zone_falls_back_to_the_default_target_for_an_uncovered_zone() {
        // A per_zone entry for a DIFFERENT cluster must not affect this one
        // — the default target still applies here.
        let mut p = provider_with_zone();
        p.spec.capabilities.storage_classes[0].per_zone = vec![ScopedTarget {
            datacenter: "dc-east".to_string(),
            cluster: "cluster-b".to_string(),
            target: target(&[("datastore", "ds-cluster-b-specific")]),
        }];
        let plan = resolve_zone(&p, "vc-dc-east-cluster-a", None).expect("zone resolves");
        assert_eq!(plan.datastore, "ds-fast-01");
    }

    #[test]
    fn resolve_nic_networks_uses_the_per_zone_override_network_for_this_cluster() {
        let mut p = provider_with_zone();
        p.spec.capabilities.network_classes[0].per_zone = vec![ScopedTarget {
            datacenter: "dc-east".to_string(),
            cluster: "cluster-a".to_string(),
            target: target(&[("distributedPortGroup", "pg-cluster-a-specific")]),
        }];
        let nics = resolve_nic_networks(&p, "vc-dc-east-cluster-a", &[]).unwrap();
        assert_eq!(nics[0].network, "pg-cluster-a-specific");
    }

    // ----------------------------------------------------------------------
    // resolve_nic_networks (ADR-0031)
    // ----------------------------------------------------------------------

    #[test]
    fn resolve_nic_networks_empty_input_synthesizes_one_default_nic() {
        let p = provider_with_zone();
        let nics = resolve_nic_networks(&p, "vc-dc-east-cluster-a", &[]).unwrap();
        assert_eq!(
            nics,
            vec![ResolvedNic {
                network: "pg-prod".to_string(),
                adapter: NicAdapter::Vmxnet3,
                pci_slot: 192,
            }]
        );
    }

    #[test]
    fn resolve_nic_networks_applies_defaults_per_entry() {
        let p = provider_with_zone();
        let nics = resolve_nic_networks(
            &p,
            "vc-dc-east-cluster-a",
            &[
                VMImageTemplateNic::default(),
                VMImageTemplateNic {
                    network: Some("vmnet-mgmt".to_string()),
                    adapter: Some(NicAdapter::E1000),
                    pci_slot: Some(300),
                },
                VMImageTemplateNic::default(),
            ],
        )
        .unwrap();
        assert_eq!(
            nics,
            vec![
                ResolvedNic {
                    network: "pg-prod".to_string(),
                    adapter: NicAdapter::Vmxnet3,
                    pci_slot: 192,
                },
                ResolvedNic {
                    network: "vmnet-mgmt".to_string(),
                    adapter: NicAdapter::E1000,
                    pci_slot: 300,
                },
                ResolvedNic {
                    network: "pg-prod".to_string(),
                    adapter: NicAdapter::Vmxnet3,
                    // index 2, not 193 — the explicit override on entry 1
                    // does not shift the auto-increment for entries after it.
                    pci_slot: 194,
                },
            ]
        );
    }

    #[test]
    fn resolve_nic_networks_errors_when_no_network_class_reachable_and_no_override() {
        let mut p = provider_with_zone();
        p.status.as_mut().unwrap().failure_domains[0]
            .attributes
            .available_network_classes
            .clear();
        assert!(resolve_nic_networks(&p, "vc-dc-east-cluster-a", &[]).is_err());
    }

    #[test]
    fn resolve_nic_networks_override_bypasses_capability_introspection() {
        let mut p = provider_with_zone();
        p.status.as_mut().unwrap().failure_domains[0]
            .attributes
            .available_network_classes
            .clear();
        let nics = resolve_nic_networks(
            &p,
            "vc-dc-east-cluster-a",
            &[VMImageTemplateNic {
                network: Some("VM Network".to_string()),
                adapter: None,
                pci_slot: None,
            }],
        )
        .unwrap();
        assert_eq!(nics[0].network, "VM Network");
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

    // ----------------------------------------------------------------------
    // upload_progress_milestones
    // ----------------------------------------------------------------------

    #[test]
    fn upload_progress_no_milestones_when_total_size_unknown() {
        assert!(upload_progress_milestones(0, 10, 0).is_empty());
    }

    #[test]
    fn upload_progress_crosses_one_milestone_per_normal_chunk() {
        assert_eq!(upload_progress_milestones(0, 10, 100), vec![10]);
        assert_eq!(upload_progress_milestones(50, 10, 100), vec![60]);
    }

    #[test]
    fn upload_progress_no_milestone_within_the_same_decile() {
        assert!(upload_progress_milestones(1, 1, 100).is_empty());
    }

    #[test]
    fn upload_progress_one_big_chunk_crosses_every_milestone() {
        assert_eq!(
            upload_progress_milestones(0, 100, 100),
            vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100]
        );
    }

    #[test]
    fn upload_progress_reaches_100_percent_at_the_last_byte() {
        assert_eq!(upload_progress_milestones(90, 10, 100), vec![100]);
    }

    #[test]
    fn upload_progress_caps_at_100_percent_even_if_bytes_overshoot_total() {
        // Defensive: should never happen (Content-Length is the file size),
        // but a stray extra byte must not report >100%.
        assert_eq!(upload_progress_milestones(90, 50, 100), vec![100]);
    }

    #[test]
    fn upload_progress_zero_length_chunk_crosses_nothing() {
        assert!(upload_progress_milestones(50, 0, 100).is_empty());
    }

    // ----------------------------------------------------------------------
    // effective_folder
    // ----------------------------------------------------------------------

    #[test]
    fn effective_folder_nests_the_zone_under_the_configured_root() {
        // Regression: two failure domains commonly share a datacenter (only
        // their cluster differs), and vSphere's VM/Template folder hierarchy
        // is scoped per-datacenter, not per-cluster. Without nesting, every
        // zone's import Job raced CreateVM_Task against the identical
        // <root> folder + template name (found live).
        assert_eq!(
            effective_folder(Some("templates/hadron"), "cluster-01"),
            "templates/hadron/cluster-01"
        );
    }

    #[test]
    fn effective_folder_strips_a_trailing_slash_on_the_root() {
        assert_eq!(
            effective_folder(Some("templates/hadron/"), "cluster-01"),
            "templates/hadron/cluster-01"
        );
    }

    #[test]
    fn effective_folder_is_just_the_zone_when_no_root_is_configured() {
        // No root configured still must not collapse every zone onto the
        // shared datacenter VM-folder root — every zone gets its own folder.
        assert_eq!(effective_folder(None, "cluster-01"), "cluster-01");
    }

    #[test]
    fn effective_folder_is_just_the_zone_when_the_root_is_empty() {
        assert_eq!(effective_folder(Some(""), "cluster-01"), "cluster-01");
    }
}
