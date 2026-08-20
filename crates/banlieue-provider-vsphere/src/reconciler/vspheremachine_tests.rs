// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::vspheremachine`].

#[cfg(test)]
mod tests {
    use banlieue_api::common::{IpamSpec, PowerState, StaticIpamConfig};
    use banlieue_api::infrastructure::{VSphereMachineStatus, VSphereNicSpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};

    use super::super::build_guestinfo;

    fn dhcp_nic(name: &str) -> VSphereNicSpec {
        VSphereNicSpec {
            name: name.to_string(),
            port_group: "vmnet-prod".to_string(),
            mac_address: None,
            ipam: IpamSpec::default(),
        }
    }

    fn static_nic(name: &str) -> VSphereNicSpec {
        VSphereNicSpec {
            name: name.to_string(),
            port_group: "vmnet-prod".to_string(),
            mac_address: None,
            ipam: IpamSpec {
                static_: Some(StaticIpamConfig {
                    address: "10.0.0.90".to_string(),
                    prefix: 24,
                    gateway: Some("10.0.0.1".to_string()),
                    nameservers: vec!["10.0.1.53".to_string(), "10.0.1.54".to_string()],
                    domain: Some("k8s.example.internal".to_string()),
                }),
                ..Default::default()
            },
        }
    }

    fn find<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn dhcp_only_and_no_userdata_produces_hostname_and_metadata_only() {
        // hostname is unconditional — it's VM identity, not network config,
        // so a plain-DHCP interface still gets one. guestinfo.metadata is
        // also unconditional (ADR-0029) — the only two keys with no static
        // network and no userData.
        let pairs = build_guestinfo("db-01", &[dhcp_nic("eth0")], None);
        assert_eq!(pairs.len(), 2);
        assert_eq!(find(&pairs, "guestinfo.network.hostname"), Some("db-01"));
        assert!(find(&pairs, "guestinfo.metadata").is_some());
    }

    #[test]
    fn hostname_is_set_for_both_dhcp_and_static_interfaces() {
        // Sourced from the VirtualMachine's own name, exactly like
        // build_guestinfo's userData placeholder substitution (ADR-0024's
        // ${VM_NAME}) — so drone/worker nodes get a stable hostname without
        // needing a per-host userData cloud-config at all.
        let dhcp = build_guestinfo("db-01", &[dhcp_nic("eth0")], None);
        assert_eq!(find(&dhcp, "guestinfo.network.hostname"), Some("db-01"));

        let static_ = build_guestinfo("db-02", &[static_nic("eth0")], None);
        assert_eq!(find(&static_, "guestinfo.network.hostname"), Some("db-02"));
    }

    #[test]
    fn static_nic_sets_every_guestinfo_network_key() {
        let pairs = build_guestinfo("db-01", &[static_nic("eth0")], None);
        assert_eq!(find(&pairs, "guestinfo.network.ip"), Some("10.0.0.90"));
        assert_eq!(find(&pairs, "guestinfo.network.prefix"), Some("24"));
        assert_eq!(find(&pairs, "guestinfo.network.gateway"), Some("10.0.0.1"));
        assert_eq!(
            find(&pairs, "guestinfo.network.dns"),
            Some("10.0.1.53,10.0.1.54")
        );
        assert_eq!(
            find(&pairs, "guestinfo.network.domain"),
            Some("k8s.example.internal")
        );
    }

    #[test]
    fn dhcp_nic_omits_guestinfo_network_keys_entirely() {
        let pairs = build_guestinfo("db-01", &[dhcp_nic("eth0")], None);
        assert_eq!(find(&pairs, "guestinfo.network.ip"), None);
        assert_eq!(find(&pairs, "guestinfo.network.prefix"), None);
    }

    #[test]
    fn static_nic_with_no_gateway_or_dns_omits_only_those_keys() {
        let nic = VSphereNicSpec {
            name: "eth0".to_string(),
            port_group: "vmnet-prod".to_string(),
            mac_address: None,
            ipam: IpamSpec {
                static_: Some(StaticIpamConfig {
                    address: "10.0.0.90".to_string(),
                    prefix: 24,
                    gateway: None,
                    nameservers: Vec::new(),
                    domain: None,
                }),
                ..Default::default()
            },
        };
        let pairs = build_guestinfo("db-01", &[nic], None);
        assert_eq!(find(&pairs, "guestinfo.network.ip"), Some("10.0.0.90"));
        assert_eq!(find(&pairs, "guestinfo.network.gateway"), None);
        assert_eq!(find(&pairs, "guestinfo.network.dns"), None);
        assert_eq!(find(&pairs, "guestinfo.network.domain"), None);
    }

    #[test]
    fn first_static_nic_wins_when_multiple_are_declared() {
        // guestinfo.network.* is a flat, non-indexed convention (matches
        // this environment's existing hand-provisioned VMs) — it can only
        // represent one primary static network, not one per NIC.
        let other = VSphereNicSpec {
            name: "eth1".to_string(),
            port_group: "vmnet-mgmt".to_string(),
            mac_address: None,
            ipam: IpamSpec {
                static_: Some(StaticIpamConfig {
                    address: "10.0.0.99".to_string(),
                    prefix: 24,
                    gateway: None,
                    nameservers: Vec::new(),
                    domain: None,
                }),
                ..Default::default()
            },
        };
        let pairs = build_guestinfo("db-01", &[static_nic("eth0"), other], None);
        assert_eq!(find(&pairs, "guestinfo.network.ip"), Some("10.0.0.90"));
    }

    #[test]
    fn dhcp_first_static_second_still_uses_the_static_one() {
        let pairs = build_guestinfo("db-01", &[dhcp_nic("eth0"), static_nic("eth1")], None);
        assert_eq!(find(&pairs, "guestinfo.network.ip"), Some("10.0.0.90"));
    }

    #[test]
    fn userdata_is_base64_encoded_with_encoding_marker() {
        let pairs = build_guestinfo(
            "db-01",
            &[dhcp_nic("eth0")],
            Some("#cloud-config\nhostname: bar01\n"),
        );
        let encoded = find(&pairs, "guestinfo.userdata").expect("userdata key present");
        let decoded =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).unwrap();
        assert_eq!(decoded, b"#cloud-config\nhostname: bar01\n");
        assert_eq!(find(&pairs, "guestinfo.userdata.encoding"), Some("base64"));
    }

    #[test]
    fn no_userdata_omits_both_userdata_keys() {
        let pairs = build_guestinfo("db-01", &[dhcp_nic("eth0")], None);
        assert_eq!(find(&pairs, "guestinfo.userdata"), None);
        assert_eq!(find(&pairs, "guestinfo.userdata.encoding"), None);
    }

    #[test]
    fn static_network_and_userdata_together() {
        let pairs = build_guestinfo("db-01", &[static_nic("eth0")], Some("#cloud-config\n"));
        assert_eq!(find(&pairs, "guestinfo.network.ip"), Some("10.0.0.90"));
        assert!(find(&pairs, "guestinfo.userdata").is_some());
    }

    // ------------------------------------------------------------------
    // ADR-0029: guestinfo.metadata — real cloud-init VMware datasource
    // hostname/FQDN default, independent of spec.userData
    // ------------------------------------------------------------------

    fn decoded_metadata(pairs: &[(String, String)]) -> String {
        let encoded = find(pairs, "guestinfo.metadata").expect("guestinfo.metadata present");
        let decoded =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).unwrap();
        String::from_utf8(decoded).unwrap()
    }

    #[test]
    fn metadata_uses_short_hostname_when_no_domain_is_known() {
        let pairs = build_guestinfo("db-01", &[dhcp_nic("eth0")], None);
        let metadata = decoded_metadata(&pairs);
        assert!(metadata.contains("instance-id: db-01"), "{metadata}");
        assert!(metadata.contains("local-hostname: db-01"), "{metadata}");
        // No domain resolved — must not fabricate a trailing-dot FQDN.
        assert!(!metadata.contains("db-01."), "{metadata}");
    }

    #[test]
    fn metadata_uses_fqdn_as_local_hostname_when_domain_is_known() {
        // cloud-init's cc_set_hostname module derives both the short
        // hostname and the FQDN from a single dotted local-hostname value —
        // there is no separate `fqdn` key in its metadata schema.
        let pairs = build_guestinfo("db-01", &[static_nic("eth0")], None);
        let metadata = decoded_metadata(&pairs);
        assert!(metadata.contains("instance-id: db-01"), "{metadata}");
        assert!(
            metadata.contains("local-hostname: db-01.k8s.example.internal"),
            "{metadata}"
        );
    }

    #[test]
    fn metadata_is_set_regardless_of_userdata() {
        // Independent inputs to cloud-init (datasource metadata vs.
        // userdata module config) — never gated on spec.userData being set.
        let with_userdata = build_guestinfo(
            "db-01",
            &[dhcp_nic("eth0")],
            Some("#cloud-config\nhostname: overridden\n"),
        );
        assert!(find(&with_userdata, "guestinfo.metadata").is_some());

        let without_userdata = build_guestinfo("db-01", &[dhcp_nic("eth0")], None);
        assert!(find(&without_userdata, "guestinfo.metadata").is_some());
    }

    #[test]
    fn metadata_never_touches_or_parses_userdata() {
        // Even userData that already sets a conflicting hostname is passed
        // through byte-for-byte — banlieue never parses or merges into it.
        let pairs = build_guestinfo(
            "db-01",
            &[dhcp_nic("eth0")],
            Some("#cloud-config\nhostname: user-supplied\n"),
        );
        let encoded = find(&pairs, "guestinfo.userdata").expect("userdata key present");
        let decoded =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).unwrap();
        assert_eq!(decoded, b"#cloud-config\nhostname: user-supplied\n");
    }

    #[test]
    fn status_with_observed_power_state_preserves_every_other_field() {
        // Regression (ADR-0034, found live): a narrower re-apply of just
        // {observedPowerState, observedGeneration} from the same field
        // manager that had applied the full status made the apiserver
        // retract — and SSA then wipe — vmRef/conditions/initialization,
        // since nothing else owned them. finalize() then read vm_ref as
        // None and skipped destroy_vm, orphaning the backend VM in vCenter.
        use super::super::status_with_observed_power_state;
        use banlieue_api::common::InitializationStatus;

        let current = VSphereMachineStatus {
            initialization: InitializationStatus {
                provisioned: Some(true),
            },
            vm_ref: Some("vm-1234".to_string()),
            conditions: vec![Condition {
                type_: "Ready".to_string(),
                status: "True".to_string(),
                reason: "Reconciled".to_string(),
                message: "VSphereMachine provisioned".to_string(),
                observed_generation: Some(1),
                last_transition_time: Time(k8s_openapi::jiff::Timestamp::now()),
            }],
            observed_power_state: Some(PowerState::PoweredOff),
            observed_generation: Some(1),
            ..Default::default()
        };

        let next = status_with_observed_power_state(current.clone(), PowerState::PoweredOn, 2);

        assert_eq!(next.vm_ref, current.vm_ref);
        assert_eq!(next.initialization, current.initialization);
        // Ready was already True/Reconciled — same status, so
        // last_transition_time doesn't move, but observedGeneration still
        // advances to this pass's generation.
        assert_eq!(next.conditions.len(), 1);
        assert_eq!(next.conditions[0].status, "True");
        assert_eq!(next.conditions[0].reason, "Reconciled");
        assert_eq!(
            next.conditions[0].last_transition_time,
            current.conditions[0].last_transition_time
        );
        assert_eq!(next.conditions[0].observed_generation, Some(2));
        assert_eq!(next.observed_power_state, Some(PowerState::PoweredOn));
        assert_eq!(next.observed_generation, Some(2));
    }

    #[test]
    fn status_with_observed_power_state_restores_ready_after_a_backend_problem_clears() {
        // A prior BackendMissing/BackendRefMissing report (ADR-0034) must
        // not stay stuck False forever once a power_state read succeeds
        // again — that would misrepresent a healthy VM as permanently
        // broken.
        use super::super::status_with_observed_power_state;
        use banlieue_api::common::InitializationStatus;

        let current = VSphereMachineStatus {
            initialization: InitializationStatus {
                provisioned: Some(true),
            },
            vm_ref: Some("vm-1234".to_string()),
            conditions: vec![Condition {
                type_: "Ready".to_string(),
                status: "False".to_string(),
                reason: "BackendMissing".to_string(),
                message: "backend VM \"vm-1234\" no longer exists in vCenter".to_string(),
                observed_generation: Some(1),
                last_transition_time: Time(k8s_openapi::jiff::Timestamp::now()),
            }],
            ..Default::default()
        };

        let next = status_with_observed_power_state(current, PowerState::PoweredOn, 2);

        let ready = next
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .expect("Ready present");
        assert_eq!(ready.status, "True");
        assert_eq!(ready.reason, "Reconciled");
    }

    #[test]
    fn is_backend_missing_error_matches_managed_object_not_found_case_insensitively() {
        use super::super::is_backend_missing_error;
        use crate::error::Error;

        assert!(is_backend_missing_error(&Error::Vsphere(
            "ServerFaultCode: ManagedObjectNotFound".to_string()
        )));
        assert!(is_backend_missing_error(&Error::Vsphere(
            "managedobjectnotfound".to_string()
        )));
        assert!(!is_backend_missing_error(&Error::Vsphere(
            "connection refused".to_string()
        )));
    }

    #[test]
    fn status_reporting_backend_problem_preserves_every_other_field() {
        use super::super::status_reporting_backend_problem;
        use banlieue_api::common::InitializationStatus;

        let current = VSphereMachineStatus {
            initialization: InitializationStatus {
                provisioned: Some(true),
            },
            vm_ref: Some("vm-1234".to_string()),
            observed_power_state: Some(PowerState::PoweredOn),
            ..Default::default()
        };

        let next = status_reporting_backend_problem(
            current.clone(),
            "BackendMissing",
            "backend VM \"vm-1234\" no longer exists in vCenter".to_string(),
            2,
        );

        assert_eq!(next.vm_ref, current.vm_ref);
        assert_eq!(next.initialization, current.initialization);
        assert_eq!(next.observed_power_state, current.observed_power_state);
        let ready = next
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .expect("Ready present");
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "BackendMissing");
        assert_eq!(next.observed_generation, Some(2));
    }
}
