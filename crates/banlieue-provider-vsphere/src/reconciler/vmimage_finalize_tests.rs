// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::destroy_zone_templates`] (ADR-0028:
//! `VMImage` deletion lifecycle).

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use banlieue_api::banlieue::{FailureDomain, FailureDomainAttributes, ZoneImageStatus};

    use crate::client::{Datacenter, FakeClient, Inventory, VSphereClient};

    use super::super::destroy_zone_templates;

    fn as_client(c: &FakeClient) -> &dyn VSphereClient {
        c
    }

    fn dc(name: &str) -> Datacenter {
        Datacenter {
            name: name.to_string(),
            moref: format!("datacenter-{name}"),
        }
    }

    fn failure_domain(zone_name: &str, datacenter: &str) -> FailureDomain {
        let mut raw = BTreeMap::new();
        raw.insert("datacenter".to_string(), datacenter.to_string());
        raw.insert("cluster".to_string(), "cluster-a".to_string());
        FailureDomain {
            name: zone_name.to_string(),
            labels: Default::default(),
            attributes: FailureDomainAttributes {
                raw,
                ..Default::default()
            },
        }
    }

    fn zone(name: &str, template: &str, folder: &str) -> ZoneImageStatus {
        ZoneImageStatus {
            name: name.to_string(),
            ready: true,
            resolved_ref: Some(template.to_string()),
            template_folder: Some(folder.to_string()),
            reason: None,
            message: None,
        }
    }

    #[tokio::test]
    async fn destroys_the_template_when_found() {
        let inv = Inventory::builder()
            .with_dc("dc-east")
            .with_template_in_folder("dc-east", "templates/cluster-01", "hadron-kairos-v0.1.0")
            .build();
        let client = FakeClient::new(inv);
        let datacenters = vec![dc("dc-east")];
        let failure_domains = vec![failure_domain("cluster-01", "dc-east")];
        let zones = vec![zone(
            "cluster-01",
            "hadron-kairos-v0.1.0",
            "templates/cluster-01",
        )];

        destroy_zone_templates(as_client(&client), &datacenters, &failure_domains, &zones)
            .await
            .unwrap();

        assert_eq!(
            client.destroyed_vms(),
            vec!["vm-template-templates/cluster-01-hadron-kairos-v0.1.0".to_string()]
        );
    }

    #[tokio::test]
    async fn is_a_noop_when_template_already_absent() {
        let client = FakeClient::new(Inventory::default());
        let datacenters = vec![dc("dc-east")];
        let failure_domains = vec![failure_domain("cluster-01", "dc-east")];
        let zones = vec![zone(
            "cluster-01",
            "hadron-kairos-v0.1.0",
            "templates/cluster-01",
        )];

        destroy_zone_templates(as_client(&client), &datacenters, &failure_domains, &zones)
            .await
            .unwrap();

        assert!(client.destroyed_vms().is_empty());
    }

    #[tokio::test]
    async fn skips_a_zone_with_no_resolved_ref() {
        // Import never got far enough to build a template — nothing to
        // destroy, and must not error (the finalizer would never clear).
        let client = FakeClient::new(Inventory::default());
        let datacenters = vec![dc("dc-east")];
        let failure_domains = vec![failure_domain("cluster-01", "dc-east")];
        let zones = vec![ZoneImageStatus {
            name: "cluster-01".to_string(),
            ready: false,
            resolved_ref: None,
            template_folder: None,
            reason: None,
            message: None,
        }];

        destroy_zone_templates(as_client(&client), &datacenters, &failure_domains, &zones)
            .await
            .unwrap();

        assert!(client.destroyed_vms().is_empty());
    }

    #[tokio::test]
    async fn skips_a_zone_whose_failure_domain_is_gone() {
        // Provider.status.failureDomains no longer lists this zone (e.g. the
        // cluster was decommissioned) — can't resolve a datacenter, so skip
        // rather than fail the whole finalize over one stale zone.
        let client = FakeClient::new(Inventory::default());
        let datacenters = vec![dc("dc-east")];
        let failure_domains = vec![failure_domain("cluster-02", "dc-east")];
        let zones = vec![zone(
            "cluster-01",
            "hadron-kairos-v0.1.0",
            "templates/cluster-01",
        )];

        destroy_zone_templates(as_client(&client), &datacenters, &failure_domains, &zones)
            .await
            .unwrap();

        assert!(client.destroyed_vms().is_empty());
    }

    #[tokio::test]
    async fn destroys_multiple_zones_in_their_own_folders_without_cross_zone_collision() {
        // The exact bug ADR-0020 Decision #5 already fixed for lookups: two
        // zones share the identical template display name. Both must be
        // found and destroyed via their own folder, never each other's.
        let inv = Inventory::builder()
            .with_dc("dc-east")
            .with_template_in_folder("dc-east", "templates/cluster-01", "hadron-kairos-v0.1.0")
            .with_template_in_folder("dc-east", "templates/cluster-02", "hadron-kairos-v0.1.0")
            .build();
        let client = FakeClient::new(inv);
        let datacenters = vec![dc("dc-east")];
        let failure_domains = vec![
            failure_domain("cluster-01", "dc-east"),
            failure_domain("cluster-02", "dc-east"),
        ];
        let zones = vec![
            zone("cluster-01", "hadron-kairos-v0.1.0", "templates/cluster-01"),
            zone("cluster-02", "hadron-kairos-v0.1.0", "templates/cluster-02"),
        ];

        destroy_zone_templates(as_client(&client), &datacenters, &failure_domains, &zones)
            .await
            .unwrap();

        let mut destroyed = client.destroyed_vms();
        destroyed.sort();
        assert_eq!(
            destroyed,
            vec![
                "vm-template-templates/cluster-01-hadron-kairos-v0.1.0".to_string(),
                "vm-template-templates/cluster-02-hadron-kairos-v0.1.0".to_string(),
            ]
        );
    }
}
