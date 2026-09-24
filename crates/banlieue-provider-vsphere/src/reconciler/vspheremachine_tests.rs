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
    fn metadata_does_not_double_append_domain_when_vm_name_is_already_fully_qualified() {
        // metadata.name is a DNS-1123 subdomain and permits dots (confirmed
        // live: a VirtualMachine named as a full FQDN applies cleanly) — a
        // VM already named "db-01.k8s.example.internal" must not render as
        // "db-01.k8s.example.internal.k8s.example.internal".
        let pairs = build_guestinfo("db-01.k8s.example.internal", &[static_nic("eth0")], None);
        let metadata = decoded_metadata(&pairs);
        assert!(
            metadata.contains("local-hostname: db-01.k8s.example.internal"),
            "{metadata}"
        );
        assert!(
            !metadata.contains("k8s.example.internal.k8s.example.internal"),
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
    fn status_with_observed_state_preserves_every_other_field() {
        // Regression (ADR-0034, found live): a narrower re-apply of just
        // {observedPowerState, observedGeneration} from the same field
        // manager that had applied the full status made the apiserver
        // retract — and SSA then wipe — vmRef/conditions/initialization,
        // since nothing else owned them. finalize() then read vm_ref as
        // None and skipped destroy_vm, orphaning the backend VM in vCenter.
        use super::super::status_with_observed_state;
        use crate::guest::GuestProbe;
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

        let next = status_with_observed_state(
            current.clone(),
            PowerState::PoweredOn,
            None,
            GuestProbe::NotEvaluated,
            Vec::new(),
            false,
            2,
        );

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
    fn status_with_observed_state_restores_ready_after_a_backend_problem_clears() {
        // A prior BackendMissing/BackendRefMissing report (ADR-0034) must
        // not stay stuck False forever once a power_state read succeeds
        // again — that would misrepresent a healthy VM as permanently
        // broken.
        use super::super::status_with_observed_state;
        use crate::guest::GuestProbe;
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

        let next = status_with_observed_state(
            current,
            PowerState::PoweredOn,
            None,
            GuestProbe::NotEvaluated,
            Vec::new(),
            false,
            2,
        );

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
    fn reporting_a_backend_problem_preserves_every_other_field() {
        use super::super::status_reporting_failure;
        use banlieue_api::common::InitializationStatus;

        let current = VSphereMachineStatus {
            initialization: InitializationStatus {
                provisioned: Some(true),
            },
            vm_ref: Some("vm-1234".to_string()),
            observed_power_state: Some(PowerState::PoweredOn),
            ..Default::default()
        };

        let next = status_reporting_failure(
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

    // ------------------------------------------------------------------
    // status_with_observed_state — GuestReady (ADR-0043, vSphere transport)
    // ------------------------------------------------------------------

    #[test]
    fn guest_ready_stays_absent_while_the_vm_is_stopped() {
        use super::super::status_with_observed_state;
        use crate::guest::GuestProbe;

        let current = VSphereMachineStatus::default();
        let next = status_with_observed_state(
            current,
            PowerState::PoweredOff,
            None,
            GuestProbe::NotEvaluated,
            Vec::new(),
            false,
            1,
        );

        assert!(
            next.conditions.iter().all(|c| c.type_ != "GuestReady"),
            "an unevaluated guest must leave GuestReady absent, not False — \
             ReadinessSignalAbsent vs. Filling forever (ADR-0043 Decision 10)"
        );
        assert_eq!(next.guest_installed, None);
    }

    #[test]
    fn guest_ready_reports_false_while_running_and_not_yet_announced() {
        use super::super::status_with_observed_state;
        use crate::guest::GuestProbe;

        let current = VSphereMachineStatus::default();
        let next = status_with_observed_state(
            current,
            PowerState::PoweredOn,
            Some(false),
            GuestProbe::NotAnnounced,
            Vec::new(),
            false,
            1,
        );

        let guest_ready = next
            .conditions
            .iter()
            .find(|c| c.type_ == "GuestReady")
            .expect("GuestReady present");
        assert_eq!(guest_ready.status, "False");
        assert_eq!(guest_ready.reason, "GuestNotAnnounced");
        assert_eq!(next.guest_installed, Some(false));
    }

    #[test]
    fn guest_ready_reports_true_once_installed() {
        use super::super::status_with_observed_state;
        use crate::guest::GuestProbe;

        let current = VSphereMachineStatus::default();
        let next = status_with_observed_state(
            current,
            PowerState::PoweredOn,
            Some(true),
            GuestProbe::Installed,
            Vec::new(),
            false,
            1,
        );

        let guest_ready = next
            .conditions
            .iter()
            .find(|c| c.type_ == "GuestReady")
            .expect("GuestReady present");
        assert_eq!(guest_ready.status, "True");
        assert_eq!(guest_ready.reason, "GuestAnnounced");
        assert_eq!(next.guest_installed, Some(true));
    }

    #[test]
    fn guest_ready_never_touches_the_ready_condition() {
        // GuestReady is additive (ADR-0043 Decision 4): Ready must report
        // True/Reconciled here regardless of the guest probe outcome, or an
        // Immediate-mode VM with no phase stage would regress to not-ready.
        use super::super::status_with_observed_state;
        use crate::guest::GuestProbe;

        let current = VSphereMachineStatus::default();
        let next = status_with_observed_state(
            current,
            PowerState::PoweredOn,
            Some(false),
            GuestProbe::NotAnnounced,
            Vec::new(),
            false,
            1,
        );

        let ready = next
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .expect("Ready present");
        assert_eq!(ready.status, "True");
        assert_eq!(ready.reason, "Reconciled");
    }

    /// Every Ready=False report must preserve the rest of status, not just
    /// the backend-problem ones. `patch_status_failed` used to SSA-apply a
    /// narrow `{conditions, observedGeneration}` from the SAME field manager
    /// that elsewhere applies the whole struct, which makes the apiserver
    /// retract every field the narrow payload omits — the exact hazard
    /// `status_with_observed_state`'s doc comment records hitting live
    /// (it wiped `vmRef`, so `finalize()` skipped `destroy_vm` and orphaned
    /// the VM). One transient vCenter blip on an already-provisioned machine
    /// would therefore drop `vmRef`, `tpmAttached` and — since ADR-0045 —
    /// the attestation anchor a claim is supposed to publish.
    #[test]
    fn reporting_a_failure_preserves_every_other_field() {
        use super::super::status_reporting_failure;
        use banlieue_api::common::InitializationStatus;

        let current = VSphereMachineStatus {
            initialization: InitializationStatus {
                provisioned: Some(true),
            },
            vm_ref: Some("vm-1234".to_string()),
            instance_uuid: Some("uuid-1234".to_string()),
            failure_domain: Some("dc1".to_string()),
            observed_power_state: Some(PowerState::PoweredOn),
            tpm_attached: Some(true),
            tpm_endorsement_certificates: vec![
                "-----BEGIN CERTIFICATE-----\nstub\n-----END CERTIFICATE-----".to_string(),
            ],
            ..Default::default()
        };

        let next = status_reporting_failure(
            current.clone(),
            "ProvisionFailed",
            "vCenter connection refused".to_string(),
            3,
        );

        assert_eq!(next.vm_ref, current.vm_ref);
        assert_eq!(next.instance_uuid, current.instance_uuid);
        assert_eq!(next.failure_domain, current.failure_domain);
        assert_eq!(next.initialization, current.initialization);
        assert_eq!(next.observed_power_state, current.observed_power_state);
        assert_eq!(next.tpm_attached, current.tpm_attached);
        assert_eq!(
            next.tpm_endorsement_certificates, current.tpm_endorsement_certificates,
            "a transient failure must not retract the attestation anchor (ADR-0045)"
        );

        let ready = next
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .expect("Ready present");
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "ProvisionFailed");
        assert_eq!(next.observed_generation, Some(3));
    }

    /// ADR-0045, mirroring the libvirt half: a tpmEnabled member that has
    /// announced itself but has no EK certificate yet cannot be attested,
    /// so a pool must not bind it. Before ADR-0043's vSphere transport
    /// landed there was no GuestReady here to gate at all — now there is,
    /// and the asymmetry the ADR originally recorded is gone.
    #[test]
    fn guest_ready_waits_for_the_endorsement_certificate_on_a_tpm_machine() {
        use super::super::status_with_observed_state;
        use crate::guest::GuestProbe;

        let current = VSphereMachineStatus {
            guest_installed: Some(true),
            ..Default::default()
        };
        let next = status_with_observed_state(
            current,
            PowerState::PoweredOn,
            Some(true),
            GuestProbe::Installed,
            Vec::new(),
            true,
            1,
        );

        let gr = next
            .conditions
            .iter()
            .find(|c| c.type_ == "GuestReady")
            .expect("GuestReady published");
        assert_eq!(gr.status, "False");
        assert_eq!(gr.reason, "TpmEndorsementPending");
    }

    /// And it goes True once vCenter has issued one.
    #[test]
    fn guest_ready_is_true_once_the_endorsement_certificate_is_published() {
        use super::super::status_with_observed_state;
        use crate::guest::GuestProbe;

        let next = status_with_observed_state(
            VSphereMachineStatus {
                guest_installed: Some(true),
                ..Default::default()
            },
            PowerState::PoweredOn,
            Some(true),
            GuestProbe::Installed,
            vec!["-----BEGIN CERTIFICATE-----\nstub\n-----END CERTIFICATE-----".to_string()],
            true,
            1,
        );

        let gr = next
            .conditions
            .iter()
            .find(|c| c.type_ == "GuestReady")
            .expect("GuestReady published");
        assert_eq!(gr.status, "True");
        assert_eq!(gr.reason, "GuestAnnounced");
    }

    /// A machine with no vTPM must not be made to wait for a certificate it
    /// can never have — the regression that would strand every non-TPM VM.
    #[test]
    fn guest_ready_ignores_the_certificate_when_there_is_no_vtpm() {
        use super::super::status_with_observed_state;
        use crate::guest::GuestProbe;

        let next = status_with_observed_state(
            VSphereMachineStatus {
                guest_installed: Some(true),
                ..Default::default()
            },
            PowerState::PoweredOn,
            Some(true),
            GuestProbe::Installed,
            Vec::new(),
            false,
            1,
        );

        let gr = next
            .conditions
            .iter()
            .find(|c| c.type_ == "GuestReady")
            .expect("GuestReady published");
        assert_eq!(gr.status, "True");
    }
}
