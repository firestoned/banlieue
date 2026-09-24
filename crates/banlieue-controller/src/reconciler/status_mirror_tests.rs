// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::status_mirror`].

#[cfg(test)]
mod tests {
    use banlieue_api::banlieue::VirtualMachineStatus;
    use banlieue_api::common::condition_types;
    use banlieue_api::common::{
        InitializationStatus, MachineAddress, MachineAddressType, PowerState,
    };
    use banlieue_provider_sdk::status::condition_status;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};

    use super::super::*;

    /// Test double — implements [`InfraMachineRead`] without needing a real
    /// VSphereMachine.
    #[derive(Default)]
    struct FakeInfra {
        init: InitializationStatus,
        addresses: Vec<MachineAddress>,
        failure_domain: Option<String>,
        provider_id: Option<String>,
        power_state: Option<PowerState>,
        conditions: Vec<Condition>,
    }

    impl InfraMachineRead for FakeInfra {
        fn initialization(&self) -> &InitializationStatus {
            &self.init
        }
        fn addresses(&self) -> &[MachineAddress] {
            &self.addresses
        }
        fn failure_domain(&self) -> Option<&str> {
            self.failure_domain.as_deref()
        }
        fn provider_id(&self) -> Option<&str> {
            self.provider_id.as_deref()
        }
        fn conditions(&self) -> &[Condition] {
            &self.conditions
        }
        fn observed_power_state(&self) -> Option<&PowerState> {
            self.power_state.as_ref()
        }
        /// Always empty: the ADR-0045 mirror is proven against a real
        /// `LibvirtMachine` below, where the field it reads actually lives.
        fn tpm_endorsement_certificates(&self) -> &[String] {
            &[]
        }
    }

    fn cond(type_: &str, status: &str, reason: &str) -> Condition {
        Condition {
            type_: type_.into(),
            status: status.into(),
            reason: reason.into(),
            message: format!("{type_}={status}"),
            observed_generation: Some(1),
            last_transition_time: Time(k8s_openapi::jiff::Timestamp::now()),
        }
    }

    fn baseline_status_scheduled() -> VirtualMachineStatus {
        let mut s = VirtualMachineStatus::default();
        s.conditions.push(cond(
            condition_types::SCHEDULED,
            condition_status::TRUE,
            "Scheduled",
        ));
        s
    }

    // ----------------------------------------------------------------------

    #[test]
    fn mirrors_initialization_addresses_and_power_state_verbatim() {
        let current = baseline_status_scheduled();
        let infra = FakeInfra {
            power_state: Some(PowerState::PoweredOn),
            init: InitializationStatus {
                provisioned: Some(true),
            },
            addresses: vec![MachineAddress {
                address_type: MachineAddressType::InternalIP,
                address: "10.0.0.5".into(),
            }],
            failure_domain: Some("dc1-cluster-a".into()),
            provider_id: Some("vsphere://uuid".into()),
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
        };

        let out = mirror_status_from_infra(&current, &infra, 7);
        assert_eq!(out.initialization.provisioned, Some(true));
        assert_eq!(out.addresses.len(), 1);
        assert_eq!(out.addresses[0].address, "10.0.0.5");
        assert_eq!(out.observed_power_state, Some(PowerState::PoweredOn));
        assert_eq!(out.observed_generation, Some(7));
    }

    #[test]
    fn observed_power_state_absent_until_infra_reports_one() {
        let current = baseline_status_scheduled();
        let infra = FakeInfra {
            power_state: None,
            init: InitializationStatus::default(),
            addresses: vec![],
            failure_domain: None,
            provider_id: None,
            conditions: vec![],
        };

        let out = mirror_status_from_infra(&current, &infra, 1);
        assert_eq!(out.observed_power_state, None);
    }

    #[test]
    fn maps_infra_ready_to_infrastructure_ready_condition() {
        let current = baseline_status_scheduled();
        let infra = FakeInfra {
            power_state: None,
            init: InitializationStatus::default(),
            addresses: vec![],
            failure_domain: None,
            provider_id: None,
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
        };
        let out = mirror_status_from_infra(&current, &infra, 1);

        let ir = out
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::INFRASTRUCTURE_READY)
            .expect("InfrastructureReady present");
        assert_eq!(ir.status, "True");
        assert_eq!(ir.reason, "Provisioned");
    }

    #[test]
    fn missing_infra_ready_condition_yields_pending_reason() {
        let current = baseline_status_scheduled();
        let infra = FakeInfra {
            power_state: None,
            init: InitializationStatus::default(),
            addresses: vec![],
            failure_domain: None,
            provider_id: None,
            conditions: vec![],
        };
        let out = mirror_status_from_infra(&current, &infra, 1);

        let ir = out
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::INFRASTRUCTURE_READY)
            .unwrap();
        assert_eq!(ir.status, "False");
        assert_eq!(ir.reason, "Pending");
    }

    #[test]
    fn aggregate_ready_is_true_when_scheduled_placement_valid_and_infra_ready() {
        let mut current = baseline_status_scheduled();
        // PlacementValid not explicitly set is treated as not-False = valid.
        current.conditions.push(cond(
            condition_types::PLACEMENT_VALID,
            condition_status::TRUE,
            "Valid",
        ));
        let infra = FakeInfra {
            power_state: None,
            init: InitializationStatus {
                provisioned: Some(true),
            },
            addresses: vec![],
            failure_domain: None,
            provider_id: None,
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
        };
        let out = mirror_status_from_infra(&current, &infra, 1);

        let ready = out
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::READY)
            .unwrap();
        assert_eq!(ready.status, "True");
        assert_eq!(ready.reason, "Reconciled");
    }

    #[test]
    fn aggregate_ready_is_false_when_infra_not_ready() {
        let current = baseline_status_scheduled();
        let infra = FakeInfra {
            power_state: None,
            init: InitializationStatus::default(),
            addresses: vec![],
            failure_domain: None,
            provider_id: None,
            conditions: vec![cond(
                condition_types::READY,
                condition_status::FALSE,
                "Cloning",
            )],
        };
        let out = mirror_status_from_infra(&current, &infra, 1);

        let ready = out
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::READY)
            .unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "InfrastructureNotReady");
    }

    #[test]
    fn aggregate_ready_is_false_when_placement_invalid_even_if_infra_ready() {
        let mut current = baseline_status_scheduled();
        current.conditions.push(cond(
            condition_types::PLACEMENT_VALID,
            condition_status::FALSE,
            "Drift",
        ));
        let infra = FakeInfra {
            power_state: None,
            init: InitializationStatus::default(),
            addresses: vec![],
            failure_domain: None,
            provider_id: None,
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
        };
        let out = mirror_status_from_infra(&current, &infra, 1);

        let ready = out
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::READY)
            .unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "PlacementInvalid");
    }

    #[test]
    fn aggregate_ready_is_false_when_not_scheduled() {
        // current has no Scheduled=True
        let current = VirtualMachineStatus::default();
        let infra = FakeInfra {
            power_state: None,
            init: InitializationStatus::default(),
            addresses: vec![],
            failure_domain: None,
            provider_id: None,
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
        };
        let out = mirror_status_from_infra(&current, &infra, 1);

        let ready = out
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::READY)
            .unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "Scheduling");
    }

    // ======================================================================
    // The concrete LibvirtMachine impl (ADR-0050)
    // ======================================================================
    //
    // The tests above exercise the mirror logic through `FakeInfra`. These
    // exercise the accessors themselves, which is where a real impl goes
    // wrong: reading `spec` where `status` was meant, or the other way
    // round.

    fn libvirt_machine() -> banlieue_api::infrastructure::LibvirtMachine {
        use banlieue_api::infrastructure::{
            LibvirtAddressSource, LibvirtBootSource, LibvirtBootSourceKind, LibvirtMachine,
            LibvirtMachineSpec, LibvirtMachineStatus,
        };
        LibvirtMachine {
            metadata: Default::default(),
            spec: LibvirtMachineSpec {
                provider_id: Some("libvirt://kvm-a/0f3c9a1e".to_string()),
                failure_domain: None,
                provider_ref: banlieue_api::common::LocalObjectReference {
                    name: "kvm-a".to_string(),
                },
                pool: "default".to_string(),
                domain_name: "ns-db-01".to_string(),
                boot_source: LibvirtBootSource {
                    kind: LibvirtBootSourceKind::BackingVolume,
                    volume: "base.qcow2".to_string(),
                },
                vcpus: 2,
                memory_mi_b: 2048,
                firmware: banlieue_api::common::Firmware::Efi,
                machine_type: None,
                tpm_enabled: false,
                disks: vec![],
                network: vec![],
                user_data: None,
                desired_power_state: PowerState::PoweredOn,
            },
            status: Some(LibvirtMachineStatus {
                initialization: InitializationStatus {
                    provisioned: Some(true),
                },
                failure_domain: Some("kvm-a-default".to_string()),
                addresses: vec![MachineAddress {
                    address_type: MachineAddressType::InternalIP,
                    address: "192.0.2.24".to_string(),
                }],
                domain_uuid: Some("0f3c9a1e-0000-4000-8000-000000000001".to_string()),
                observed_power_state: Some(PowerState::PoweredOn),
                address_source: Some(LibvirtAddressSource::GuestAgent),
                guest_installed: None,
                install_media_detached: None,
                tpm_attached: None,
                tpm_endorsement_certificates: vec![],
                conditions: vec![Condition {
                    type_: condition_types::READY.to_string(),
                    status: "True".to_string(),
                    reason: "DomainRunning".to_string(),
                    message: String::new(),
                    last_transition_time: Time(k8s_openapi::jiff::Timestamp::now()),
                    observed_generation: None,
                }],
                observed_generation: None,
            }),
        }
    }

    /// `providerID` is a **spec** field on every InfraMachine (CAPI puts it
    /// there), while everything else the trait reads is status. Getting that
    /// one backwards returns `None` forever and the Node never links up.
    #[test]
    fn libvirt_impl_reads_provider_id_from_spec_and_the_rest_from_status() {
        let m = libvirt_machine();
        assert_eq!(m.provider_id(), Some("libvirt://kvm-a/0f3c9a1e"));
        assert_eq!(m.failure_domain(), Some("kvm-a-default"));
        assert_eq!(m.initialization().provisioned, Some(true));
        assert_eq!(m.addresses().len(), 1);
        assert_eq!(m.addresses()[0].address, "192.0.2.24");
        assert_eq!(m.observed_power_state(), Some(&PowerState::PoweredOn));
        assert_eq!(m.conditions().len(), 1);
    }

    /// The first reconcile sees a LibvirtMachine with no status at all. Every
    /// accessor must return an empty value rather than panicking.
    #[test]
    fn libvirt_impl_tolerates_an_absent_status() {
        let mut m = libvirt_machine();
        m.status = None;
        assert_eq!(m.initialization().provisioned, None);
        assert!(m.addresses().is_empty());
        assert!(m.failure_domain().is_none());
        assert!(m.conditions().is_empty());
        assert!(m.observed_power_state().is_none());
        // providerID lives on spec, so it survives an absent status.
        assert_eq!(m.provider_id(), Some("libvirt://kvm-a/0f3c9a1e"));
    }

    /// End to end through the real impl: a Ready LibvirtMachine projects
    /// `InfrastructureReady=True` onto its parent, by the same code path
    /// vSphere uses. The `FakeInfra` tests above prove the projection logic;
    /// this proves the concrete type is wired into it.
    #[test]
    fn libvirt_machine_drives_infrastructure_ready_on_the_parent() {
        let m = libvirt_machine();
        let status = mirror_status_from_infra(&VirtualMachineStatus::default(), &m, 1);
        let cond = find_condition(&status.conditions, condition_types::INFRASTRUCTURE_READY)
            .expect("InfrastructureReady must be published");
        assert_eq!(cond.status, condition_status::TRUE);
        assert_eq!(status.initialization.provisioned, Some(true));
        assert_eq!(status.addresses.len(), 1);
        assert_eq!(status.observed_power_state, Some(PowerState::PoweredOn));
    }

    // ------------------------------------------------------------------
    // GuestReady mirroring (ADR-0043)
    // ------------------------------------------------------------------

    #[test]
    fn guestready_is_mirrored_from_the_infra_cr_with_its_reason() {
        let current = baseline_status_scheduled();
        let infra = FakeInfra {
            init: InitializationStatus {
                provisioned: Some(true),
            },
            conditions: vec![
                cond(
                    condition_types::READY,
                    condition_status::TRUE,
                    "DomainRunning",
                ),
                cond(
                    condition_types::GUEST_READY,
                    condition_status::TRUE,
                    "GuestAnnounced",
                ),
            ],
            ..Default::default()
        };

        let next = mirror_status_from_infra(&current, &infra, 1);
        let g = next
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::GUEST_READY)
            .expect("GuestReady mirrored onto the VirtualMachine");
        assert_eq!(g.status, condition_status::TRUE);
        assert_eq!(g.reason, "GuestAnnounced");
    }

    /// **The subtle one.** A provider that does not implement the signal
    /// must leave the condition *absent*, not publish `False`.
    ///
    /// `pool.rs::readiness_signal_absent` decides by condition TYPE, not
    /// status: it reports `ReadinessSignalAbsent` only when no member
    /// carries the condition at all. Publishing a blanket `GuestReady=False`
    /// here would make a pool set to `GuestReady` report `Filling` forever
    /// instead — turning "this will never warm" into "wait a bit longer",
    /// which is the exact diagnostic ADR-0046 Decision 3 exists to give.
    #[test]
    fn guestready_is_not_published_when_the_infra_cr_is_silent() {
        let current = baseline_status_scheduled();
        let infra = FakeInfra {
            init: InitializationStatus {
                provisioned: Some(true),
            },
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "DomainRunning",
            )],
            ..Default::default()
        };

        let next = mirror_status_from_infra(&current, &infra, 1);
        assert!(
            !next
                .conditions
                .iter()
                .any(|c| c.type_ == condition_types::GUEST_READY),
            "a silent provider must leave GuestReady absent, not False"
        );
    }

    /// ADR-0043 Decision 4, at the VirtualMachine layer.
    #[test]
    fn aggregate_ready_does_not_depend_on_guestready() {
        let current = baseline_status_scheduled();
        let infra = FakeInfra {
            init: InitializationStatus {
                provisioned: Some(true),
            },
            conditions: vec![
                cond(
                    condition_types::READY,
                    condition_status::TRUE,
                    "DomainRunning",
                ),
                cond(
                    condition_types::GUEST_READY,
                    condition_status::FALSE,
                    "GuestNotAnnounced",
                ),
            ],
            ..Default::default()
        };

        let next = mirror_status_from_infra(&current, &infra, 1);
        let ready = next
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::READY)
            .expect("Ready published");
        assert_eq!(
            ready.status,
            condition_status::TRUE,
            "an Immediate-mode VM with no guest marker must stay Ready"
        );
    }

    /// ADR-0045: the EK certificate is the anchor a verifier checks an
    /// attestation quote against, so it has to reach the parent — and from
    /// there a bound claim — or ADR-0049 has nothing to work with.
    #[test]
    fn the_vtpm_endorsement_certificate_is_mirrored_from_the_infra_cr() {
        const PEM: &str = "-----BEGIN CERTIFICATE-----\nstub\n-----END CERTIFICATE-----";
        let mut infra = libvirt_machine();
        infra
            .status
            .as_mut()
            .expect("fixture has status")
            .tpm_endorsement_certificates = vec![PEM.to_string()];

        let next = mirror_status_from_infra(&VirtualMachineStatus::default(), &infra, 1);
        assert_eq!(next.tpm_endorsement_certificates, vec![PEM.to_string()]);
    }

    /// A machine with no vTPM publishes none, and the parent must not invent
    /// one.
    #[test]
    fn no_certificate_on_the_infra_cr_means_none_on_the_parent() {
        let infra = libvirt_machine();
        let next = mirror_status_from_infra(&VirtualMachineStatus::default(), &infra, 1);
        assert!(next.tpm_endorsement_certificates.is_empty());
    }
}
