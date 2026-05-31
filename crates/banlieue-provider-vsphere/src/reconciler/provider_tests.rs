// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::provider`].
//!
//! These tests target the pure helpers and the in-process `discover_inventory`
//! function (which takes a `&dyn VSphereClient`, so the FakeClient drives it
//! without any kube cluster contact).

#[cfg(test)]
mod tests {
    use crate::client::{Datacenter, FakeClient, Inventory, VSphereClient};

    use super::super::{discover_inventory, failure_domain_name};

    fn as_client(c: &FakeClient) -> &dyn VSphereClient {
        c
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
            failure_domain_name("prod-vsphere", "DC East", "Cluster A"),
            "prod-vsphere-dc-east-cluster-a"
        );
    }

    #[test]
    fn failure_domain_name_strips_special_characters() {
        assert_eq!(
            failure_domain_name("p", "dc/east", "c.l.u_s_ter:1"),
            "p-dc-east-c-l-u-s-ter-1"
        );
    }

    #[test]
    fn failure_domain_name_collapses_consecutive_separators() {
        assert_eq!(
            failure_domain_name("p", "DC  East", "Cluster--A"),
            "p-dc-east-cluster-a"
        );
    }

    #[test]
    fn failure_domain_name_truncates_to_dns_label_limit() {
        // Kubernetes object names cap at 253 chars but resource names embedded
        // inside other fields (label values, condition reasons) are often
        // capped at 63 chars by webhooks. Guard against unbounded length.
        let huge = "x".repeat(200);
        let name = failure_domain_name("p", &huge, &huge);
        assert!(name.len() <= 63, "name too long: {} chars", name.len());
    }

    #[tokio::test]
    async fn discover_inventory_returns_one_fd_per_dc_cluster_pair() {
        let client = fake_client(small_inventory());
        let fds = discover_inventory(as_client(&client), "prod-vsphere")
            .await
            .expect("inventory walk succeeds");

        assert_eq!(fds.len(), 3, "two DCs × (2+1) clusters = 3 FDs");

        let names: Vec<&str> = fds.iter().map(|fd| fd.name.as_str()).collect();
        assert!(names.contains(&"prod-vsphere-dc-east-cluster-a"));
        assert!(names.contains(&"prod-vsphere-dc-east-cluster-b"));
        assert!(names.contains(&"prod-vsphere-dc-west-cluster-z"));
    }

    #[tokio::test]
    async fn discover_inventory_populates_labels_and_raw_attributes() {
        let client = fake_client(small_inventory());
        let fds = discover_inventory(as_client(&client), "p").await.unwrap();
        let fd = fds
            .iter()
            .find(|f| f.name == "p-dc-east-cluster-a")
            .unwrap();

        assert_eq!(fd.labels.get("dc").map(String::as_str), Some("dc-east"));
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
    async fn discover_inventory_with_no_datacenters_returns_empty() {
        let client = fake_client(Inventory::default());
        let fds = discover_inventory(as_client(&client), "p").await.unwrap();
        assert!(fds.is_empty());
    }

    #[tokio::test]
    async fn discover_inventory_with_dc_but_no_clusters_returns_empty() {
        // Pre-built inventory with a DC but no clusters. Documents that a
        // bare DC produces zero FDs — clusters are the scheduling unit.
        let inv = Inventory::builder().with_dc("dc-empty").build();
        let client = fake_client(inv);
        let fds = discover_inventory(as_client(&client), "p").await.unwrap();
        assert!(fds.is_empty());
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
