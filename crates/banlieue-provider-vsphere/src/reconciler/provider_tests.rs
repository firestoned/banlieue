// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::provider`].
//!
//! These tests target the pure helpers and the in-process `discover_inventory`
//! function (which takes a `&dyn VSphereClient`, so the FakeClient drives it
//! without any kube cluster contact).

#[cfg(test)]
mod tests {
    use crate::client::{Datacenter, Datastore, FakeClient, Inventory, Network, VSphereClient};

    use banlieue_api::banlieue::{
        FailureDomainNameOverride, NetworkClassMapping, ProviderCapabilities, StorageClassMapping,
    };
    use std::collections::BTreeMap;

    use super::super::{
        FailureDomainIdentity, compute_failure_domain_attributes, discover_inventory,
        failure_domain_name,
    };

    fn as_client(c: &FakeClient) -> &dyn VSphereClient {
        c
    }

    fn fd_name(provider: &str, dc: &str, cluster: &str) -> String {
        failure_domain_name(&FailureDomainIdentity {
            provider,
            dc,
            cluster,
        })
    }

    fn small_inventory() -> Inventory {
        Inventory::builder()
            .with_dc("dc-east")
            .with_cluster("dc-east", "cluster-a")
            .with_cluster("dc-east", "cluster-b")
            .with_dc("dc-west")
            .with_cluster("dc-west", "cluster-z")
            .build()
    }

    fn fake_client(inv: Inventory) -> FakeClient {
        FakeClient::new(inv)
    }

    #[test]
    fn failure_domain_name_is_slug_of_provider_dc_cluster() {
        assert_eq!(
            fd_name("prod-vsphere", "DC East", "Cluster A"),
            "prod-vsphere-dc-east-cluster-a"
        );
    }

    #[test]
    fn failure_domain_name_strips_special_characters() {
        assert_eq!(
            fd_name("p", "dc/east", "c.l.u_s_ter:1"),
            "p-dc-east-c-l-u-s-ter-1"
        );
    }

    #[test]
    fn failure_domain_name_collapses_consecutive_separators() {
        assert_eq!(
            fd_name("p", "DC  East", "Cluster--A"),
            "p-dc-east-cluster-a"
        );
    }

    #[test]
    fn failure_domain_name_truncates_to_dns_label_limit() {
        // Kubernetes object names cap at 253 chars but resource names embedded
        // inside other fields (label values, condition reasons) are often
        // capped at 63 chars by webhooks. Guard against unbounded length.
        let huge = "x".repeat(200);
        let name = fd_name("p", &huge, &huge);
        assert!(name.len() <= 63, "name too long: {} chars", name.len());
    }

    #[test]
    fn failure_domain_name_stays_unique_when_truncated() {
        // Regression: enterprise cluster names can be long and share a common
        // prefix, differing only in a trailing `-01/-02/-03`. That suffix falls
        // past the 63-char cap, so naive front-truncation collapsed every
        // failure domain onto one identical name.
        let dc = "dc-example";
        let base = "compute-cluster-dedicated-nonreplicated-availability-domain";
        let n1 = fd_name("vcenter-example", dc, &format!("{base}-01"));
        let n2 = fd_name("vcenter-example", dc, &format!("{base}-02"));
        let n3 = fd_name("vcenter-example", dc, &format!("{base}-03"));
        assert!(n1.len() <= 63 && n2.len() <= 63 && n3.len() <= 63);
        assert_ne!(n1, n2);
        assert_ne!(n2, n3);
        assert_ne!(n1, n3);
    }

    #[test]
    fn failure_domain_name_is_deterministic() {
        let huge = "y".repeat(120);
        assert_eq!(
            fd_name("p", &huge, "cluster-01"),
            fd_name("p", &huge, "cluster-01"),
        );
    }

    #[tokio::test]
    async fn discover_inventory_returns_one_fd_per_dc_cluster_pair() {
        let client = fake_client(small_inventory());
        let fds = discover_inventory(
            as_client(&client),
            "prod-vsphere",
            &ProviderCapabilities::default(),
            &[],
        )
        .await
        .expect("inventory walk succeeds");

        assert_eq!(fds.len(), 3, "two DCs × (2+1) clusters = 3 FDs");

        let names: Vec<&str> = fds.iter().map(|fd| fd.name.as_str()).collect();
        assert!(names.contains(&"prod-vsphere-dc-east-cluster-a"));
        assert!(names.contains(&"prod-vsphere-dc-east-cluster-b"));
        assert!(names.contains(&"prod-vsphere-dc-west-cluster-z"));

        // Order must be deterministic (sorted by name): an unstable order would
        // rewrite status.failureDomains every reconcile and hot-loop the
        // controller.
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(
            names, sorted,
            "failure domains must be returned sorted by name"
        );
    }

    #[tokio::test]
    async fn discover_inventory_populates_labels_and_raw_attributes() {
        let client = fake_client(small_inventory());
        let fds = discover_inventory(
            as_client(&client),
            "p",
            &ProviderCapabilities::default(),
            &[],
        )
        .await
        .unwrap();
        let fd = fds
            .iter()
            .find(|f| f.name == "p-dc-east-cluster-a")
            .unwrap();

        assert_eq!(
            fd.labels.get("datacenter").map(String::as_str),
            Some("dc-east")
        );
        assert_eq!(
            fd.labels.get("cluster").map(String::as_str),
            Some("cluster-a")
        );
        assert_eq!(
            fd.attributes.raw.get("datacenter").map(String::as_str),
            Some("dc-east")
        );
        assert_eq!(
            fd.attributes.raw.get("cluster").map(String::as_str),
            Some("cluster-a")
        );
    }

    #[tokio::test]
    async fn discover_inventory_labels_each_fd_with_its_own_resolved_name() {
        // A `failureDomainSelector` can only ever match `labels`, never the
        // top-level `name` field directly — without this, a friendly
        // `failureDomainNameOverrides` name (ADR-0023) was unselectable: an
        // operator writing `matchLabels: { name: cluster-01 }` silently
        // matched zero failure domains.
        let overrides = [FailureDomainNameOverride {
            datacenter: "dc-east".to_string(),
            cluster: "cluster-a".to_string(),
            name: "cluster-01".to_string(),
        }];
        let client = fake_client(small_inventory());
        let fds = discover_inventory(
            as_client(&client),
            "p",
            &ProviderCapabilities::default(),
            &overrides,
        )
        .await
        .unwrap();
        let fd = fds.iter().find(|f| f.name == "cluster-01").unwrap();
        assert_eq!(
            fd.labels.get("name").map(String::as_str),
            Some("cluster-01")
        );

        // And for a failure domain with no override, the auto-computed name
        // is what ends up in the label too — the invariant is "labels[name]
        // always equals this FailureDomain's own name", not just for
        // overridden ones.
        let auto = fds
            .iter()
            .find(|f| f.name == "p-dc-east-cluster-b")
            .unwrap();
        assert_eq!(
            auto.labels.get("name").map(String::as_str),
            Some("p-dc-east-cluster-b")
        );
    }

    #[tokio::test]
    async fn discover_inventory_with_no_datacenters_returns_empty() {
        let client = fake_client(Inventory::default());
        let fds = discover_inventory(
            as_client(&client),
            "p",
            &ProviderCapabilities::default(),
            &[],
        )
        .await
        .unwrap();
        assert!(fds.is_empty());
    }

    #[tokio::test]
    async fn discover_inventory_with_dc_but_no_clusters_returns_empty() {
        // Pre-built inventory with a DC but no clusters. Documents that a
        // bare DC produces zero FDs — clusters are the scheduling unit.
        let inv = Inventory::builder().with_dc("dc-empty").build();
        let client = fake_client(inv);
        let fds = discover_inventory(
            as_client(&client),
            "p",
            &ProviderCapabilities::default(),
            &[],
        )
        .await
        .unwrap();
        assert!(fds.is_empty());
    }

    // ----------------------------------------------------------------------
    // failureDomainNameOverrides (ADR-0023)
    // ----------------------------------------------------------------------

    fn fd_override(datacenter: &str, cluster: &str, name: &str) -> FailureDomainNameOverride {
        FailureDomainNameOverride {
            datacenter: datacenter.to_string(),
            cluster: cluster.to_string(),
            name: name.to_string(),
        }
    }

    #[tokio::test]
    async fn discover_inventory_uses_the_override_name_when_one_matches() {
        let client = fake_client(small_inventory());
        let overrides = [fd_override("dc-east", "cluster-a", "cluster-01")];
        let fds = discover_inventory(
            as_client(&client),
            "prod-vsphere",
            &ProviderCapabilities::default(),
            &overrides,
        )
        .await
        .expect("inventory walk succeeds");

        let names: Vec<&str> = fds.iter().map(|fd| fd.name.as_str()).collect();
        // The overridden zone gets the simple name...
        assert!(names.contains(&"cluster-01"));
        // ...and every other zone still gets the auto-computed one.
        assert!(names.contains(&"prod-vsphere-dc-east-cluster-b"));
        assert!(names.contains(&"prod-vsphere-dc-west-cluster-z"));
        assert!(!names.contains(&"prod-vsphere-dc-east-cluster-a"));
    }

    #[tokio::test]
    async fn discover_inventory_slugifies_an_override_name() {
        let client = fake_client(small_inventory());
        let overrides = [fd_override("dc-east", "cluster-a", "Cluster 01")];
        let fds = discover_inventory(
            as_client(&client),
            "prod-vsphere",
            &ProviderCapabilities::default(),
            &overrides,
        )
        .await
        .unwrap();
        let names: Vec<&str> = fds.iter().map(|fd| fd.name.as_str()).collect();
        assert!(names.contains(&"cluster-01"));
    }

    #[tokio::test]
    async fn discover_inventory_ignores_an_override_for_an_unmatched_zone() {
        let client = fake_client(small_inventory());
        let overrides = [fd_override("dc-nonexistent", "cluster-x", "cluster-01")];
        let fds = discover_inventory(
            as_client(&client),
            "prod-vsphere",
            &ProviderCapabilities::default(),
            &overrides,
        )
        .await
        .unwrap();
        let names: Vec<&str> = fds.iter().map(|fd| fd.name.as_str()).collect();
        assert_eq!(names.len(), 3);
        assert!(!names.contains(&"cluster-01"));
    }

    #[tokio::test]
    async fn discover_inventory_fails_when_two_zones_override_to_the_same_name() {
        // Two DIFFERENT (dc, cluster) pairs mapped to the same override name
        // would silently reintroduce the exact cross-zone collision
        // ADR-0020 Decision #5 fixed — a bad override must fail the whole
        // reconcile, not publish two failure domains with the same identity.
        let client = fake_client(small_inventory());
        let overrides = [
            fd_override("dc-east", "cluster-a", "cluster-01"),
            fd_override("dc-east", "cluster-b", "cluster-01"),
        ];
        let err = discover_inventory(
            as_client(&client),
            "prod-vsphere",
            &ProviderCapabilities::default(),
            &overrides,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("cluster-01"));
    }

    // ----------------------------------------------------------------------
    // capability introspection (ADR-0019)
    // ----------------------------------------------------------------------

    fn target(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn compute_attributes_matches_targets_by_kind() {
        let caps = ProviderCapabilities {
            storage_classes: vec![
                StorageClassMapping {
                    name: "gold".into(),
                    target: Some(target(&[("datastoreCluster", "DSC-A")])),
                    ..Default::default()
                },
                StorageClassMapping {
                    name: "by-name".into(),
                    target: Some(target(&[("datastore", "ds-01")])),
                    ..Default::default()
                },
                StorageClassMapping {
                    name: "absent".into(),
                    target: Some(target(&[("datastoreCluster", "DSC-Z")])),
                    ..Default::default()
                },
                StorageClassMapping {
                    name: "tagged".into(),
                    target: Some(target(&[("tagCategory", "tier"), ("tag", "gold")])),
                    ..Default::default()
                },
            ],
            network_classes: vec![
                NetworkClassMapping {
                    name: "dvs".into(),
                    target: Some(target(&[("distributedPortGroup", "pg-dvs")])),
                    ..Default::default()
                },
                NetworkClassMapping {
                    name: "std".into(),
                    target: Some(target(&[("portGroup", "pg-std")])),
                    ..Default::default()
                },
                // portGroup target against a *distributed* net must NOT match.
                NetworkClassMapping {
                    name: "wrong-kind".into(),
                    target: Some(target(&[("portGroup", "pg-dvs")])),
                    ..Default::default()
                },
            ],
            features: vec!["hotAddCPU".into(), "efiSecureBoot".into()],
        };
        let datastores = vec![
            Datastore {
                name: "ds-01".into(),
                moref: "datastore-1".into(),
                datastore_cluster: None,
                free_space_bytes: None,
            },
            Datastore {
                name: "ds-99".into(),
                moref: "datastore-2".into(),
                datastore_cluster: Some("DSC-A".into()),
                free_space_bytes: None,
            },
        ];
        let networks = vec![
            Network {
                name: "pg-dvs".into(),
                moref: "n1".into(),
                distributed: true,
            },
            Network {
                name: "pg-std".into(),
                moref: "n2".into(),
                distributed: false,
            },
        ];
        let a = compute_failure_domain_attributes(&caps, &datastores, &networks, "dc", "cl");
        assert_eq!(a.available_storage_classes, vec!["gold", "by-name"]);
        assert_eq!(a.available_network_classes, vec!["dvs", "std"]);
        assert_eq!(a.features, vec!["hotAddCPU", "efiSecureBoot"]); // passthrough
        assert_eq!(a.raw.get("datacenter").map(String::as_str), Some("dc"));
        assert_eq!(a.raw.get("cluster").map(String::as_str), Some("cl"));
    }

    #[tokio::test]
    async fn discover_inventory_reports_per_cluster_reachability() {
        // c1 can reach the gold datastore cluster + the prod DVS port group; c2
        // reaches neither, so the same declared classes are available only in c1.
        let inv = Inventory::builder()
            .with_dc("dc")
            .with_cluster("dc", "c1")
            .with_cluster("dc", "c2")
            .with_datastore("dc", "c1", "ds-gold", Some("DSC-GOLD"))
            .with_network("dc", "c1", "pg-prod", true)
            .build();
        let caps = ProviderCapabilities {
            storage_classes: vec![StorageClassMapping {
                name: "gold".into(),
                target: Some(target(&[("datastoreCluster", "DSC-GOLD")])),
                ..Default::default()
            }],
            network_classes: vec![NetworkClassMapping {
                name: "prod".into(),
                target: Some(target(&[("distributedPortGroup", "pg-prod")])),
                ..Default::default()
            }],
            features: Vec::new(),
        };
        let client = fake_client(inv);
        let fds = discover_inventory(as_client(&client), "p", &caps, &[])
            .await
            .unwrap();
        let c1 = fds.iter().find(|f| f.labels["cluster"] == "c1").unwrap();
        let c2 = fds.iter().find(|f| f.labels["cluster"] == "c2").unwrap();
        assert_eq!(c1.attributes.available_storage_classes, vec!["gold"]);
        assert_eq!(c1.attributes.available_network_classes, vec!["prod"]);
        assert!(c2.attributes.available_storage_classes.is_empty());
        assert!(c2.attributes.available_network_classes.is_empty());
    }

    // Smoke: make sure the slim domain types stay usable through Clone/Eq —
    // tests that future PartialEq removal triggers a compile failure here
    // (rather than deep inside a reconciler test).
    #[test]
    fn datacenter_clone_and_equality_work() {
        let dc1 = Datacenter {
            name: "a".into(),
            moref: "datacenter-a".into(),
        };
        let dc2 = dc1.clone();
        assert_eq!(dc1, dc2);
    }
}
