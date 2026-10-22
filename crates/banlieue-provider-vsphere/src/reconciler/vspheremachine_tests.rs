// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::vspheremachine`].

#[cfg(test)]
mod tests {
    use banlieue_api::common::{IpamSpec, PowerState, StaticIpamConfig};
    use banlieue_api::infrastructure::{VSphereMachineStatus, VSphereNicSpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};

    use super::super::build_guestinfo;
    use super::super::{TpmObservation, refresh_needs_patch, status_with_observed_state};

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
            2,
            TpmObservation::plain(false),
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
            2,
            TpmObservation::plain(false),
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
            1,
            TpmObservation::plain(false),
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
            1,
            TpmObservation::plain(false),
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
            1,
            TpmObservation::plain(false),
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
            1,
            TpmObservation::plain(false),
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
            1,
            TpmObservation::plain(true),
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
            1,
            TpmObservation::plain(true),
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
            1,
            TpmObservation::plain(false),
        );

        let gr = next
            .conditions
            .iter()
            .find(|c| c.type_ == "GuestReady")
            .expect("GuestReady published");
        assert_eq!(gr.status, "True");
    }

    // ------------------------------------------------------------------
    // status_with_observed_state — first-boot sealing (ADR-0088 Decision 5)
    // ------------------------------------------------------------------

    fn ready_status(ready_since_epoch: i64) -> VSphereMachineStatus {
        use banlieue_api::common::InitializationStatus;
        VSphereMachineStatus {
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
                last_transition_time: Time(
                    k8s_openapi::jiff::Timestamp::from_second(ready_since_epoch).unwrap(),
                ),
            }],
            observed_power_state: Some(PowerState::PoweredOn),
            tpm_endorsement_certificates: vec!["PEM".to_string()],
            ..Default::default()
        }
    }

    fn observe(
        current: VSphereMachineStatus,
        guest: crate::guest::GuestProbe,
        sealed: &[&str],
        now_epoch: i64,
    ) -> VSphereMachineStatus {
        let required = banlieue_api::common::FirstBootSealing::default();
        status_with_observed_state(
            current,
            PowerState::PoweredOn,
            Some(guest == crate::guest::GuestProbe::Installed),
            guest,
            vec!["PEM".to_string()],
            2,
            TpmObservation {
                tpm_enabled: true,
                sealing: Some(&required),
                sealed: sealed.iter().map(|p| (*p).to_string()).collect(),
                now: k8s_openapi::jiff::Timestamp::from_second(now_epoch).unwrap(),
            },
        )
    }

    fn guest_ready(s: &VSphereMachineStatus) -> &Condition {
        s.conditions
            .iter()
            .find(|c| c.type_ == "GuestReady")
            .expect("GuestReady present")
    }

    #[test]
    fn a_sealed_installed_guest_is_guest_ready() {
        use crate::guest::GuestProbe;
        let next = observe(
            ready_status(0),
            GuestProbe::Installed,
            &["COS_PERSISTENT"],
            30,
        );
        assert_eq!(guest_ready(&next).status, "True");
        assert_eq!(next.sealed_partitions, vec!["COS_PERSISTENT".to_string()]);
    }

    #[test]
    fn an_installed_guest_that_has_not_reported_sealing_is_unverified() {
        use crate::guest::GuestProbe;
        let next = observe(ready_status(0), GuestProbe::Installed, &[], 30);
        let c = guest_ready(&next);
        assert_eq!(c.status, "False");
        assert_eq!(
            c.reason,
            banlieue_provider_sdk::sealing::REASON_FIRST_BOOT_SEALING_UNVERIFIED
        );
        assert!(c.message.contains("COS_PERSISTENT"));
    }

    /// The halted-clone case: it never reaches userspace, so it never
    /// announces, and only the deadline can say so.
    #[test]
    fn a_guest_that_never_announces_times_out_after_the_deadline() {
        use crate::guest::GuestProbe;
        let next = observe(ready_status(0), GuestProbe::NotAnnounced, &[], 601);
        let c = guest_ready(&next);
        assert_eq!(c.status, "False");
        assert_eq!(
            c.reason,
            banlieue_provider_sdk::sealing::REASON_FIRST_BOOT_SEALING_TIMEOUT
        );
    }

    #[test]
    fn before_the_deadline_a_silent_guest_is_still_just_not_announced() {
        use crate::guest::GuestProbe;
        let next = observe(ready_status(0), GuestProbe::NotAnnounced, &[], 30);
        assert_eq!(guest_ready(&next).reason, "GuestNotAnnounced");
    }

    #[test]
    fn a_sealing_report_is_sticky_across_an_empty_observation() {
        use crate::guest::GuestProbe;
        let mut current = ready_status(0);
        current.sealed_partitions = vec!["COS_PERSISTENT".to_string()];
        current.guest_installed = Some(true);
        let next = observe(current, GuestProbe::Installed, &[], 10_000);
        assert_eq!(next.sealed_partitions, vec!["COS_PERSISTENT".to_string()]);
        assert_eq!(guest_ready(&next).status, "True");
    }

    #[test]
    fn without_sealing_nothing_about_guest_ready_changes() {
        use crate::guest::GuestProbe;
        let next = status_with_observed_state(
            ready_status(0),
            PowerState::PoweredOn,
            Some(true),
            GuestProbe::Installed,
            vec!["PEM".to_string()],
            2,
            TpmObservation::plain(true),
        );
        assert_eq!(guest_ready(&next).status, "True");
        assert!(next.sealed_partitions.is_empty());
    }

    // ------------------------------------------------------------------
    // refresh_needs_patch — when a provisioned machine's status is written
    // ------------------------------------------------------------------

    fn settled_status() -> VSphereMachineStatus {
        let mut s = ready_status(0);
        s.guest_installed = Some(true);
        s
    }

    fn cos_persistent() -> Vec<String> {
        vec!["COS_PERSISTENT".to_string()]
    }

    #[test]
    fn nothing_changed_needs_no_patch() {
        let current = settled_status();
        assert!(!refresh_needs_patch(
            &current,
            &PowerState::PoweredOn,
            Some(true),
            None,
            &["PEM".to_string()],
            &[],
            false,
        ));
    }

    /// Found live: the pass on which the report first arrives closes the
    /// gate (Satisfied), so "gate open" no longer forces the write, and
    /// nothing else changed. Without this trigger sealedPartitions and
    /// GuestReady=True are never recorded.
    #[test]
    fn a_newly_reported_seal_needs_a_patch_even_with_the_gate_closed() {
        let current = settled_status();
        assert!(refresh_needs_patch(
            &current,
            &PowerState::PoweredOn,
            Some(true),
            None,
            &["PEM".to_string()],
            &cos_persistent(),
            false,
        ));
    }

    #[test]
    fn an_already_recorded_seal_needs_no_patch() {
        let mut current = settled_status();
        current.sealed_partitions = cos_persistent();
        assert!(!refresh_needs_patch(
            &current,
            &PowerState::PoweredOn,
            Some(true),
            None,
            &["PEM".to_string()],
            &cos_persistent(),
            false,
        ));
    }

    #[test]
    fn an_open_sealing_gate_needs_a_patch() {
        let current = settled_status();
        assert!(refresh_needs_patch(
            &current,
            &PowerState::PoweredOn,
            Some(true),
            None,
            &["PEM".to_string()],
            &[],
            true,
        ));
    }

    #[test]
    fn a_power_state_change_needs_a_patch() {
        let current = settled_status();
        assert!(refresh_needs_patch(
            &current,
            &PowerState::PoweredOff,
            Some(true),
            None,
            &["PEM".to_string()],
            &[],
            false,
        ));
    }

    #[test]
    fn a_newly_recorded_eject_needs_a_patch() {
        let current = settled_status();
        assert!(refresh_needs_patch(
            &current,
            &PowerState::PoweredOn,
            Some(true),
            Some(true),
            &["PEM".to_string()],
            &[],
            false,
        ));
    }

    #[test]
    fn an_already_recorded_eject_needs_no_patch() {
        let mut current = settled_status();
        current.install_media_detached = Some(true);
        assert!(!refresh_needs_patch(
            &current,
            &PowerState::PoweredOn,
            Some(true),
            Some(true),
            &["PEM".to_string()],
            &[],
            false,
        ));
    }

    // ------------------------------------------------------------------
    // ADR-0044, vSphere amendment: install media is ejected once the guest
    // is installed, and GuestReady waits for it.
    // ------------------------------------------------------------------

    fn guest_ready_with_media(detached: Option<bool>) -> Condition {
        use crate::guest::GuestProbe;

        let next = status_with_observed_state(
            VSphereMachineStatus {
                guest_installed: Some(true),
                install_media_detached: detached,
                ..Default::default()
            },
            PowerState::PoweredOn,
            Some(true),
            GuestProbe::Installed,
            Vec::new(),
            1,
            TpmObservation::plain(false),
        );
        next.conditions
            .into_iter()
            .find(|c| c.type_ == "GuestReady")
            .expect("GuestReady published")
    }

    /// The invariant: a member whose installer is still attached is never
    /// GuestReady, so a pool cannot bind it.
    #[test]
    fn guest_ready_waits_for_the_install_media_eject() {
        let gr = guest_ready_with_media(Some(false));
        assert_eq!(gr.status, "False");
        assert_eq!(gr.reason, "InstallMediaAttached");
    }

    #[test]
    fn guest_ready_is_true_once_the_install_media_is_ejected() {
        let gr = guest_ready_with_media(Some(true));
        assert_eq!(gr.status, "True");
        assert_eq!(gr.reason, "GuestAnnounced");
    }

    #[test]
    fn eject_is_due_only_for_an_installed_guest_not_yet_recorded() {
        use super::super::eject_install_media_due;

        assert!(eject_install_media_due(Some(true), None));
        assert!(eject_install_media_due(Some(true), Some(false)));
        // Sticky: never re-read once recorded.
        assert!(!eject_install_media_due(Some(true), Some(true)));
        // The installer is still running from it.
        assert!(!eject_install_media_due(Some(false), None));
        assert!(!eject_install_media_due(None, None));
    }

    const VM: &str = "vm-1";

    fn fake() -> crate::client::FakeClient {
        crate::client::FakeClient::new(crate::client::Inventory::default())
    }

    #[tokio::test]
    async fn an_installed_deferred_clone_has_its_installer_ejected() {
        use super::super::settle_install_media;

        let client = fake();
        client.attach_install_media(VM, 1);
        let detached = settle_install_media(&client, VM, Some(true), None).await;
        assert_eq!(detached, Some(true));
        assert_eq!(client.ejected_vms(), vec![VM.to_string()]);
        assert_eq!(client.install_media_of(VM), 0);
    }

    /// An Immediate clone has no CD-ROM: verified, recorded, nothing ejected.
    #[tokio::test]
    async fn a_clone_without_install_media_records_it_as_detached() {
        use super::super::settle_install_media;

        let client = fake();
        let detached = settle_install_media(&client, VM, Some(true), None).await;
        assert_eq!(detached, Some(true));
        assert!(client.ejected_vms().is_empty());
    }

    #[tokio::test]
    async fn nothing_is_ejected_while_the_installer_runs() {
        use super::super::settle_install_media;

        let client = fake();
        client.attach_install_media(VM, 1);
        let detached = settle_install_media(&client, VM, Some(false), None).await;
        assert_eq!(detached, None);
        assert_eq!(client.install_media_of(VM), 1);
    }

    #[tokio::test]
    async fn a_recorded_eject_is_not_repeated() {
        use super::super::settle_install_media;

        let client = fake();
        client.attach_install_media(VM, 1);
        let detached = settle_install_media(&client, VM, Some(true), Some(true)).await;
        assert_eq!(detached, Some(true));
        assert!(client.ejected_vms().is_empty());
    }

    /// A failed eject (a locked tray, a vCenter fault) withholds GuestReady
    /// and is retried, never reported as done.
    #[tokio::test]
    async fn a_failed_eject_stays_pending() {
        use super::super::settle_install_media;

        let client = fake();
        client.attach_install_media(VM, 1);
        client.fail(crate::client::FakeStep::EjectInstallMedia);
        let detached = settle_install_media(&client, VM, Some(true), None).await;
        assert_eq!(detached, Some(false));
        assert_eq!(client.install_media_of(VM), 1);
    }

    // ------------------------------------------------------------------
    // ADR-0034 amendment 2026-10-23: a provisioned VM is driven to
    // `spec.desiredPowerState`. Found live: a Deferred install that ends
    // with `install.poweroff: true` left the VM off forever, because the
    // provider only observed power state after the clone.
    // ------------------------------------------------------------------

    #[test]
    fn power_action_is_none_when_the_vm_is_already_in_the_desired_state() {
        use super::super::power_action;

        for state in [
            PowerState::PoweredOn,
            PowerState::PoweredOff,
            PowerState::Suspended,
        ] {
            assert_eq!(power_action(&state, &state), None);
        }
    }

    #[test]
    fn power_action_drives_the_vm_to_the_desired_state() {
        use super::super::power_action;

        assert_eq!(
            power_action(&PowerState::PoweredOn, &PowerState::PoweredOff),
            Some(PowerState::PoweredOn)
        );
        // A suspended VM is resumed by powering it on.
        assert_eq!(
            power_action(&PowerState::PoweredOn, &PowerState::Suspended),
            Some(PowerState::PoweredOn)
        );
        assert_eq!(
            power_action(&PowerState::PoweredOff, &PowerState::PoweredOn),
            Some(PowerState::PoweredOff)
        );
        assert_eq!(
            power_action(&PowerState::Suspended, &PowerState::PoweredOn),
            Some(PowerState::Suspended)
        );
    }

    /// A fake VM already in `state`, seeded the only way the fake allows (a
    /// first `set_power_state` on a moref records it without complaint).
    async fn fake_vm_in(state: PowerState) -> crate::client::FakeClient {
        use crate::client::VSphereClient;

        let client = crate::client::FakeClient::new(crate::client::Inventory::default());
        client.set_power_state(VM, state).await.unwrap();
        client
    }

    #[tokio::test]
    async fn a_powered_off_vm_is_powered_on() {
        use super::super::converge_power_state;

        let client = fake_vm_in(PowerState::PoweredOff).await;
        let calls = client.power_state_call_count(VM);

        let (state, retry) =
            converge_power_state(&client, VM, &PowerState::PoweredOn, PowerState::PoweredOff).await;

        assert_eq!(state, PowerState::PoweredOn);
        assert!(!retry);
        assert_eq!(client.power_state_of(VM), Some(PowerState::PoweredOn));
        assert_eq!(client.power_state_call_count(VM), calls + 1);
    }

    /// The steady state costs no vCenter mutation, and the fake would fault
    /// a redundant request (`InvalidPowerState`) just as vCenter does.
    #[tokio::test]
    async fn a_vm_already_in_the_desired_state_is_left_alone() {
        use super::super::converge_power_state;

        let client = fake_vm_in(PowerState::PoweredOn).await;
        let calls = client.power_state_call_count(VM);

        let (state, retry) =
            converge_power_state(&client, VM, &PowerState::PoweredOn, PowerState::PoweredOn).await;

        assert_eq!(state, PowerState::PoweredOn);
        assert!(!retry);
        assert_eq!(client.power_state_call_count(VM), calls);
    }

    #[tokio::test]
    async fn a_vm_that_should_be_off_is_powered_off() {
        use super::super::converge_power_state;

        let client = fake_vm_in(PowerState::PoweredOn).await;

        let (state, retry) =
            converge_power_state(&client, VM, &PowerState::PoweredOff, PowerState::PoweredOn).await;

        assert_eq!(state, PowerState::PoweredOff);
        assert!(!retry);
        assert_eq!(client.power_state_of(VM), Some(PowerState::PoweredOff));
    }

    /// A vCenter fault must not stop the observation from being recorded: the
    /// real state is reported and the caller retries soon.
    #[tokio::test]
    async fn a_failed_power_change_reports_the_real_state_and_asks_for_a_retry() {
        use super::super::converge_power_state;

        let client = fake_vm_in(PowerState::PoweredOff).await;
        client.fail(crate::client::FakeStep::SetPowerState);

        let (state, retry) =
            converge_power_state(&client, VM, &PowerState::PoweredOn, PowerState::PoweredOff).await;

        assert_eq!(state, PowerState::PoweredOff);
        assert!(retry);
        assert_eq!(client.power_state_of(VM), Some(PowerState::PoweredOff));
    }
}
