// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::status`].

#[cfg(test)]
mod tests {
    use super::super::*;

    fn cond(
        type_: &str,
        status: &str,
    ) -> k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition {
        k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition {
            type_: type_.to_string(),
            status: status.to_string(),
            reason: "Initial".to_string(),
            message: "initial".to_string(),
            observed_generation: Some(1),
            last_transition_time: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                k8s_openapi::jiff::Timestamp::now(),
            ),
        }
    }

    #[test]
    fn set_condition_inserts_when_absent() {
        let mut conditions = Vec::new();

        set_condition(&mut conditions, "Ready", "True", "Reconciled", "ok", 7);

        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].type_, "Ready");
        assert_eq!(conditions[0].status, "True");
        assert_eq!(conditions[0].reason, "Reconciled");
        assert_eq!(conditions[0].observed_generation, Some(7));
    }

    #[test]
    fn set_condition_preserves_transition_time_when_status_unchanged() {
        let mut conditions = vec![cond("Ready", "True")];
        let original_transition = conditions[0].last_transition_time.clone();

        std::thread::sleep(std::time::Duration::from_millis(2));
        set_condition(
            &mut conditions,
            "Ready",
            "True",
            "StillReady",
            "still ok",
            9,
        );

        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].last_transition_time.0, original_transition.0);
        assert_eq!(conditions[0].reason, "StillReady");
        assert_eq!(conditions[0].observed_generation, Some(9));
    }

    #[test]
    fn set_condition_updates_transition_time_when_status_flips() {
        let mut conditions = vec![cond("Ready", "True")];
        let original_transition = conditions[0].last_transition_time.clone();

        std::thread::sleep(std::time::Duration::from_millis(2));
        set_condition(&mut conditions, "Ready", "False", "Reason", "msg", 9);

        assert_eq!(conditions[0].status, "False");
        assert!(conditions[0].last_transition_time.0 > original_transition.0);
    }

    #[test]
    fn set_condition_keeps_sorted_by_type() {
        let mut conditions = Vec::new();
        set_condition(&mut conditions, "Scheduled", "True", "r", "m", 1);
        set_condition(&mut conditions, "InfrastructureReady", "False", "r", "m", 1);
        set_condition(&mut conditions, "Ready", "False", "r", "m", 1);

        let types: Vec<_> = conditions.iter().map(|c| c.type_.as_str()).collect();
        assert_eq!(types, vec!["InfrastructureReady", "Ready", "Scheduled"]);
    }

    #[test]
    fn is_condition_true_matches_only_true_status() {
        let conditions = vec![cond("Ready", "True"), cond("Migrating", "False")];

        assert!(is_condition_true(&conditions, "Ready"));
        assert!(!is_condition_true(&conditions, "Migrating"));
        assert!(!is_condition_true(&conditions, "Absent"));
    }

    #[test]
    fn find_condition_returns_reference() {
        let conditions = vec![cond("Ready", "True")];
        let found = find_condition(&conditions, "Ready").expect("present");
        assert_eq!(found.status, "True");
        assert!(find_condition(&conditions, "Missing").is_none());
    }

    // ------------------------------------------------------------------
    // declared addresses (ADR-0114, amendment 2026-11-06)
    // ------------------------------------------------------------------

    fn static_ipam(address: &str) -> banlieue_api::common::IpamSpec {
        banlieue_api::common::IpamSpec {
            static_: Some(banlieue_api::common::StaticIpamConfig {
                address: address.to_string(),
                prefix: 24,
                gateway: None,
                nameservers: Vec::new(),
                domain: None,
            }),
            pool: None,
        }
    }

    /// Each static NIC's address, in spec order, as `InternalIP`.
    #[test]
    fn declared_addresses_list_each_static_nic_in_order() {
        let a = static_ipam("192.0.2.10");
        let b = static_ipam("192.0.2.11");
        let got = declared_addresses([&a, &b]);
        let addrs: Vec<&str> = got.iter().map(|m| m.address.as_str()).collect();
        assert_eq!(addrs, ["192.0.2.10", "192.0.2.11"]);
        assert!(
            got.iter()
                .all(|m| m.address_type == banlieue_api::common::MachineAddressType::InternalIP)
        );
    }

    /// A DHCP interface declares nothing; a DHCP-only machine reports
    /// nothing, as before the amendment.
    #[test]
    fn a_dhcp_nic_declares_nothing() {
        let dhcp = banlieue_api::common::IpamSpec::default();
        let s = static_ipam("192.0.2.10");
        assert!(declared_addresses([&dhcp]).is_empty());
        let got = declared_addresses([&dhcp, &s]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].address, "192.0.2.10");
    }
}
