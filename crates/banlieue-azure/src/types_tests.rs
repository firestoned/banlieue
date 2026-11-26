// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `types.rs`, against real ARM responses (see
//! `fixtures/README.md`).

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;

    use serde_json::json;

    use super::super::*;
    use crate::tags::{OwnershipTags, TAG_OWNER_UID};

    const SKUS: &[u8] = include_bytes!("../fixtures/resource-skus.json");
    const USAGES: &[u8] = include_bytes!("../fixtures/usages.json");
    const RUNNING: &[u8] = include_bytes!("../fixtures/instance-view-running.json");
    const DEALLOCATED: &[u8] = include_bytes!("../fixtures/instance-view-deallocated.json");
    const TIMED_OUT: &[u8] =
        include_bytes!("../fixtures/instance-view-provisioning-timed-out.json");
    const NIC: &[u8] = include_bytes!("../fixtures/nic.json");
    const RESOURCES: &[u8] = include_bytes!("../fixtures/resources.json");

    fn sku<'a>(skus: &'a [ResourceSku], name: &str) -> &'a ResourceSku {
        skus.iter().find(|s| s.name == name).expect(name)
    }

    #[test]
    fn usable_zones_are_offered_minus_this_subscriptions_restrictions() {
        let skus = parse_resource_skus(SKUS).expect("parses");
        assert_eq!(
            sku(&skus, "Standard_D2s_v7").usable_zones(),
            ["1", "2", "3", "4"]
        );
        assert_eq!(sku(&skus, "Standard_D2as_v6").usable_zones(), ["1", "4"]);
        assert!(sku(&skus, "Standard_D2s_v6").usable_zones().is_empty());
        assert_eq!(
            sku(&skus, "Standard_F1als_v7").usable_zones(),
            ["1", "2", "4"]
        );
    }

    #[test]
    fn sku_capabilities_carry_vcpus_family_and_disk_controllers() {
        let skus = parse_resource_skus(SKUS).expect("parses");
        let d2s = sku(&skus, "Standard_D2s_v7");
        assert_eq!(d2s.vcpus, Some(2));
        assert_eq!(d2s.family, "StandardDsv7Family");
        assert_eq!(d2s.disk_controller_types, ["NVMe"]);
        assert!(d2s.supports_controller("nvme"), "case-insensitive");
        assert!(!d2s.supports_controller("SCSI"));
    }

    #[test]
    fn a_location_restriction_makes_every_zone_unusable() {
        let body = json!({"value": [{
            "resourceType": "virtualMachines", "name": "Standard_X", "family": "f",
            "locationInfo": [{"location": "eastus2", "zones": ["1", "2"]}],
            "restrictions": [{"type": "Location", "reasonCode": "NotAvailableForSubscription",
                              "restrictionInfo": {"locations": ["eastus2"]}}],
            "capabilities": []
        }]});
        let skus = parse_resource_skus(body.to_string().as_bytes()).expect("parses");
        assert!(skus[0].usable_zones().is_empty());
    }

    #[test]
    fn non_vm_skus_are_ignored() {
        let body = json!({"value": [{"resourceType": "disks", "name": "Premium_LRS"}]});
        assert!(
            parse_resource_skus(body.to_string().as_bytes())
                .expect("parses")
                .is_empty()
        );
    }

    #[test]
    fn headroom_is_the_smaller_of_regional_and_family() {
        let usages = parse_usages(USAGES).expect("parses");
        // Captured: cores 3/4, StandardDsv7Family 2/4.
        assert_eq!(vcpu_headroom(&usages, "StandardDsv7Family"), Some(1));
        // standardDav6Family 0/4 but regional cores leave 1.
        assert_eq!(vcpu_headroom(&usages, "standardDav6Family"), Some(1));
        // Family casing differs across Azure's own lists.
        assert_eq!(vcpu_headroom(&usages, "standarddsv7family"), Some(1));
    }

    #[test]
    fn an_unknown_family_falls_back_to_regional_headroom() {
        let usages = parse_usages(USAGES).expect("parses");
        assert_eq!(vcpu_headroom(&usages, "StandardNoSuchFamily"), Some(1));
    }

    #[test]
    fn no_regional_figure_means_no_headroom_figure() {
        let usages = parse_usages(br#"{"value":[]}"#).expect("parses");
        assert_eq!(vcpu_headroom(&usages, "StandardDsv7Family"), None);
    }

    #[test]
    fn usage_over_limit_is_zero_headroom_not_negative() {
        let body = json!({"value": [
            {"name": {"value": "cores"}, "currentValue": 6, "limit": 4, "unit": "Count"}
        ]});
        let usages = parse_usages(body.to_string().as_bytes()).expect("parses");
        assert_eq!(vcpu_headroom(&usages, "x"), Some(0));
    }

    #[test]
    fn a_running_vm_has_succeeded_and_is_guest_ready() {
        let view = parse_instance_view(RUNNING).expect("parses");
        assert_eq!(view.provisioning, Provisioning::Succeeded);
        assert_eq!(view.power, Power::Running);
        assert!(view.guest_ready());
    }

    #[test]
    fn a_deallocated_vm_is_still_provisioned() {
        let view = parse_instance_view(DEALLOCATED).expect("parses");
        assert_eq!(view.provisioning, Provisioning::Succeeded);
        assert_eq!(view.power, Power::Deallocated);
        assert!(
            view.guest_ready(),
            "guestInstalled is sticky (ADR-0043 Decision 5)"
        );
    }

    #[test]
    fn a_provisioning_timeout_is_a_failure_with_its_reason() {
        let view = parse_instance_view(TIMED_OUT).expect("parses");
        assert_eq!(
            view.provisioning,
            Provisioning::Failed {
                reason: "OSProvisioningTimedOut".to_string()
            }
        );
        assert_eq!(view.power, Power::Running);
        assert!(!view.guest_ready());
        assert!(
            view.provisioning_message
                .as_deref()
                .is_some_and(|m| m.contains("did not finish"))
        );
    }

    #[test]
    fn creating_is_not_ready_and_unknown_states_are_preserved() {
        let body = json!({"statuses": [
            {"code": "ProvisioningState/creating"},
            {"code": "PowerState/starting"}
        ]});
        let view = parse_instance_view(body.to_string().as_bytes()).expect("parses");
        assert_eq!(view.provisioning, Provisioning::Creating);
        assert_eq!(view.power, Power::Starting);
        assert!(!view.guest_ready());

        let body = json!({"statuses": [{"code": "ProvisioningState/migrating"}]});
        let view = parse_instance_view(body.to_string().as_bytes()).expect("parses");
        assert_eq!(
            view.provisioning,
            Provisioning::Other("migrating".to_string())
        );
        assert_eq!(view.power, Power::Unknown);
    }

    #[test]
    fn the_nic_reports_its_primary_private_address() {
        assert_eq!(
            primary_private_ip(NIC).expect("parses").as_deref(),
            Some("10.240.1.4")
        );
    }

    #[test]
    fn a_nic_without_an_address_yet_is_none() {
        let body = json!({"properties": {"ipConfigurations": [{"properties": {"primary": true}}]}});
        assert_eq!(
            primary_private_ip(body.to_string().as_bytes()).expect("parses"),
            None
        );
    }

    #[test]
    fn a_vm_read_yields_its_tags() {
        let tags = parse_vm_tags(br#"{"name":"vm","tags":{"banlieue-owner-uid":"u1","x":"y"}}"#)
            .expect("parses");
        assert_eq!(
            tags.get("banlieue-owner-uid").map(String::as_str),
            Some("u1")
        );
        assert_eq!(tags.len(), 2);
    }

    /// ARM omits `tags` on an untagged resource: that is no tags, not an
    /// error, so an untagged VM is seen as untagged (and so not ours).
    #[test]
    fn a_vm_without_tags_has_none() {
        assert!(
            parse_vm_tags(br#"{"name":"vm"}"#)
                .expect("parses")
                .is_empty()
        );
    }

    #[test]
    fn a_vm_read_that_is_not_json_is_a_decode_error() {
        assert!(parse_vm_tags(b"not json").is_err());
    }

    #[test]
    fn listed_resources_parse_with_creation_time() {
        let resources = parse_resources(RESOURCES).expect("parses");
        let vm = resources
            .iter()
            .find(|r| {
                r.resource_type == "Microsoft.Compute/virtualMachines" && r.name == "spike-router"
            })
            .expect("router listed");
        assert!(vm.created_time.is_some());
        assert_eq!(
            vm.tags.get("banlieue-purpose").map(String::as_str),
            Some("roadmap22-23-spike")
        );
    }

    #[test]
    fn resource_ids_compare_case_insensitively() {
        // ARM upper-cased the resource group inside disk IDs.
        assert!(same_resource_id(
            "/subscriptions/s/resourceGroups/BANLIEUE-EXAMPLE/providers/Microsoft.Compute/disks/d",
            "/subscriptions/s/resourceGroups/banlieue-example/providers/Microsoft.Compute/disks/d",
        ));
        assert!(!same_resource_id("/a/b/c", "/a/b/d"));
    }

    fn resource(name: &str, tags: &BTreeMap<String, String>, created: &str) -> serde_json::Value {
        json!({
            "id": format!("/subscriptions/s/resourceGroups/g/providers/Microsoft.Network/networkInterfaces/{name}"),
            "name": name,
            "type": "Microsoft.Network/networkInterfaces",
            "tags": tags,
            "createdTime": created
        })
    }

    #[test]
    fn orphans_are_ours_with_a_dead_owner_and_older_than_the_grace() {
        let provider = "banlieue-system.azure-eastus2";
        let live = OwnershipTags::new(
            "live-uid",
            "AzureVirtualMachine",
            "banlieue-system",
            "azure-eastus2",
        );
        let dead = OwnershipTags::new(
            "dead-uid",
            "AzureVirtualMachine",
            "banlieue-system",
            "azure-eastus2",
        );
        let other_provider = OwnershipTags::new(
            "dead-uid",
            "AzureVirtualMachine",
            "banlieue-system",
            "azure-westus3",
        );
        let body = json!({"value": [
            resource("live-old", &live.to_map(), "2026-11-24T00:00:00Z"),
            resource("dead-old", &dead.to_map(), "2026-11-24T00:00:00Z"),
            resource("dead-young", &dead.to_map(), "2026-11-24T00:50:00Z"),
            resource("other-provider", &other_provider.to_map(), "2026-11-24T00:00:00Z"),
            resource("untagged", &BTreeMap::new(), "2026-11-24T00:00:00Z"),
        ]});
        let resources = parse_resources(body.to_string().as_bytes()).expect("parses");
        let live_uids = BTreeSet::from(["live-uid".to_string()]);
        let now: jiff::Timestamp = "2026-11-24T01:00:00Z".parse().expect("time");

        let found: Vec<&str> = orphans(
            &resources,
            provider,
            &live_uids,
            now,
            Duration::from_secs(15 * 60),
        )
        .into_iter()
        .map(|r| r.name.as_str())
        .collect();
        assert_eq!(found, ["dead-old"]);
    }

    #[test]
    fn a_resource_without_creation_time_is_never_swept() {
        let dead = OwnershipTags::new("dead-uid", "AzureVirtualMachine", "ns", "p");
        let mut r = resource("no-time", &dead.to_map(), "x");
        r.as_object_mut().expect("object").remove("createdTime");
        let resources =
            parse_resources(json!({"value": [r]}).to_string().as_bytes()).expect("parses");
        let now: jiff::Timestamp = "2030-01-01T00:00:00Z".parse().expect("time");
        assert!(orphans(&resources, "ns.p", &BTreeSet::new(), now, Duration::ZERO).is_empty());
        assert!(resources[0].tags.contains_key(TAG_OWNER_UID));
    }
}
