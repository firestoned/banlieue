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
        fn desired_power_state(&self) -> &PowerState {
            &PowerState::PoweredOn
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
        let infra = FakeInfra::default();

        let out = mirror_status_from_infra(&current, &infra, 1);
        assert_eq!(out.observed_power_state, None);
    }

    #[test]
    fn maps_infra_ready_to_infrastructure_ready_condition() {
        let current = baseline_status_scheduled();
        let infra = FakeInfra {
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
            ..Default::default()
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
        let infra = FakeInfra::default();
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
            init: InitializationStatus {
                provisioned: Some(true),
            },
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
            ..Default::default()
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
            conditions: vec![cond(
                condition_types::READY,
                condition_status::FALSE,
                "Cloning",
            )],
            ..Default::default()
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
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
            ..Default::default()
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
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
            ..Default::default()
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
                provider_ref: banlieue_api::common::ProviderReference {
                    name: "kvm-a".to_string(),
                    namespace: None,
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
                first_boot_sealing: None,
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
                sealed_partitions: vec![],
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

    // ======================================================================
    // The concrete CloudHypervisorMachine impl (ADR-0062)
    // ======================================================================

    fn cloud_hypervisor_machine() -> banlieue_api::infrastructure::CloudHypervisorMachine {
        use banlieue_api::infrastructure::{
            ChAddressSource, ChBootSource, ChBootSourceKind, ChCpuSpec, ChMemorySpec,
            CloudHypervisorMachine, CloudHypervisorMachineSpec, CloudHypervisorMachineStatus,
        };
        CloudHypervisorMachine {
            metadata: Default::default(),
            spec: CloudHypervisorMachineSpec {
                provider_id: Some("cloudhypervisor://ch-a/0f3c9a1e".to_string()),
                failure_domain: None,
                provider_ref: banlieue_api::common::ProviderReference {
                    name: "ch-a".to_string(),
                    namespace: None,
                },
                cpus: ChCpuSpec { boot: 2, max: None },
                memory: ChMemorySpec {
                    size_mi_b: 2048,
                    hugepages: false,
                },
                storage_class: "default".to_string(),
                boot_source: ChBootSource {
                    kind: ChBootSourceKind::Image,
                    image: "kairos".to_string(),
                },
                os_disk_size_gi_b: 20,
                nics: vec![],
                tpm_enabled: false,
                user_data: None,
                desired_power_state: PowerState::PoweredOn,
                first_boot_sealing: None,
                identity: None,
                move_in: None,
                move_out: None,
            },
            status: Some(CloudHypervisorMachineStatus {
                initialization: InitializationStatus {
                    provisioned: Some(true),
                },
                failure_domain: Some("ch-a".to_string()),
                addresses: vec![MachineAddress {
                    address_type: MachineAddressType::InternalIP,
                    address: "192.0.2.25".to_string(),
                }],
                address_source: Some(ChAddressSource::Neighbour),
                observed_power_state: Some(PowerState::PoweredOn),
                conditions: vec![Condition {
                    type_: condition_types::READY.to_string(),
                    status: "True".to_string(),
                    reason: "GuestRunning".to_string(),
                    message: String::new(),
                    last_transition_time: Time(k8s_openapi::jiff::Timestamp::now()),
                    observed_generation: None,
                }],
                ..Default::default()
            }),
        }
    }

    #[test]
    fn cloud_hypervisor_impl_reads_provider_id_from_spec_and_the_rest_from_status() {
        let m = cloud_hypervisor_machine();
        assert_eq!(m.provider_id(), Some("cloudhypervisor://ch-a/0f3c9a1e"));
        assert_eq!(m.failure_domain(), Some("ch-a"));
        assert_eq!(m.initialization().provisioned, Some(true));
        assert_eq!(m.addresses()[0].address, "192.0.2.25");
        assert_eq!(m.observed_power_state(), Some(&PowerState::PoweredOn));
        assert_eq!(m.conditions().len(), 1);
        assert!(m.tpm_endorsement_certificates().is_empty());
    }

    #[test]
    fn cloud_hypervisor_impl_tolerates_an_absent_status() {
        let mut m = cloud_hypervisor_machine();
        m.status = None;
        assert_eq!(m.initialization().provisioned, None);
        assert!(m.addresses().is_empty());
        assert!(m.failure_domain().is_none());
        assert!(m.conditions().is_empty());
        assert!(m.observed_power_state().is_none());
        assert_eq!(m.provider_id(), Some("cloudhypervisor://ch-a/0f3c9a1e"));
    }

    #[test]
    fn cloud_hypervisor_machine_drives_infrastructure_ready_on_the_parent() {
        let m = cloud_hypervisor_machine();
        let status = mirror_status_from_infra(&VirtualMachineStatus::default(), &m, 1);
        let cond = find_condition(&status.conditions, condition_types::INFRASTRUCTURE_READY)
            .expect("InfrastructureReady must be published");
        assert_eq!(cond.status, condition_status::TRUE);
        assert_eq!(status.initialization.provisioned, Some(true));
        assert_eq!(status.addresses.len(), 1);
    }

    // ---- desired_power_state (ADR-0083) ----------------------------------

    #[test]
    fn libvirt_machine_reports_its_spec_power_state() {
        let mut m = libvirt_machine();
        m.spec.desired_power_state = PowerState::PoweredOff;
        assert_eq!(
            InfraMachineRead::desired_power_state(&m),
            &PowerState::PoweredOff
        );
    }

    #[test]
    fn cloud_hypervisor_machine_reports_its_spec_power_state() {
        let mut m = cloud_hypervisor_machine();
        m.spec.desired_power_state = PowerState::Suspended;
        assert_eq!(
            InfraMachineRead::desired_power_state(&m),
            &PowerState::Suspended
        );
    }

    // ======================================================================
    // The concrete ProxmoxMachine impl (ADR-0075)
    // ======================================================================

    fn proxmox_machine() -> banlieue_api::infrastructure::ProxmoxMachine {
        use banlieue_api::infrastructure::{
            ProxmoxAddressSource, ProxmoxMachine, ProxmoxMachineSpec, ProxmoxMachineStatus,
        };
        ProxmoxMachine {
            metadata: Default::default(),
            spec: ProxmoxMachineSpec {
                provider_id: Some("proxmox://pve-a/100".to_string()),
                failure_domain: None,
                provider_ref: banlieue_api::common::ProviderReference {
                    name: "pve-a".to_string(),
                    namespace: None,
                },
                node: "pve1".to_string(),
                template_vmid: 9000,
                storage: "local-lvm".to_string(),
                pool: None,
                cores: 2,
                sockets: 1,
                memory_mi_b: 2048,
                cpu_type: None,
                firmware: banlieue_api::common::Firmware::Efi,
                tpm_enabled: false,
                os_disk_size_gi_b: 20,
                data_disks: vec![],
                nics: vec![],
                iso_storage: None,
                user_data: None,
                desired_power_state: PowerState::PoweredOn,
            },
            status: Some(ProxmoxMachineStatus {
                initialization: InitializationStatus {
                    provisioned: Some(true),
                },
                failure_domain: Some("pve-a-pve1".to_string()),
                addresses: vec![MachineAddress {
                    address_type: MachineAddressType::InternalIP,
                    address: "192.0.2.30".to_string(),
                }],
                address_source: Some(ProxmoxAddressSource::GuestAgent),
                observed_power_state: Some(PowerState::PoweredOn),
                conditions: vec![Condition {
                    type_: condition_types::READY.to_string(),
                    status: "True".to_string(),
                    reason: "VMRunning".to_string(),
                    message: String::new(),
                    last_transition_time: Time(k8s_openapi::jiff::Timestamp::now()),
                    observed_generation: None,
                }],
                ..Default::default()
            }),
        }
    }

    #[test]
    fn proxmox_impl_reads_provider_id_from_spec_and_the_rest_from_status() {
        let m = proxmox_machine();
        assert_eq!(m.provider_id(), Some("proxmox://pve-a/100"));
        assert_eq!(m.failure_domain(), Some("pve-a-pve1"));
        assert_eq!(m.initialization().provisioned, Some(true));
        assert_eq!(m.addresses()[0].address, "192.0.2.30");
        assert_eq!(m.observed_power_state(), Some(&PowerState::PoweredOn));
        assert_eq!(m.conditions().len(), 1);
        assert!(m.tpm_endorsement_certificates().is_empty());
    }

    #[test]
    fn proxmox_impl_tolerates_an_absent_status() {
        let mut m = proxmox_machine();
        m.status = None;
        assert_eq!(m.initialization().provisioned, None);
        assert!(m.addresses().is_empty());
        assert!(m.failure_domain().is_none());
        assert!(m.conditions().is_empty());
        assert!(m.observed_power_state().is_none());
        assert_eq!(m.provider_id(), Some("proxmox://pve-a/100"));
    }

    #[test]
    fn proxmox_machine_drives_infrastructure_ready_on_the_parent() {
        let m = proxmox_machine();
        let status = mirror_status_from_infra(&VirtualMachineStatus::default(), &m, 1);
        let cond = find_condition(&status.conditions, condition_types::INFRASTRUCTURE_READY)
            .expect("InfrastructureReady must be published");
        assert_eq!(cond.status, condition_status::TRUE);
        assert_eq!(status.initialization.provisioned, Some(true));
        assert_eq!(status.addresses.len(), 1);
    }

    #[test]
    fn proxmox_machine_reports_its_spec_power_state() {
        let mut m = proxmox_machine();
        m.spec.desired_power_state = PowerState::PoweredOff;
        assert_eq!(
            InfraMachineRead::desired_power_state(&m),
            &PowerState::PoweredOff
        );
    }
    // ======================================================================
    // The concrete AzureVirtualMachine impl (ADR-0164)
    // ======================================================================

    fn azure_machine() -> banlieue_api::infrastructure::AzureVirtualMachine {
        use banlieue_api::infrastructure::{
            AzureVirtualMachine, AzureVirtualMachineSpec, AzureVirtualMachineStatus,
        };
        AzureVirtualMachine {
            metadata: Default::default(),
            spec: AzureVirtualMachineSpec {
                provider_id: Some(
                    "azure:///subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/rg/providers/Microsoft.Compute/virtualMachines/vm-1"
                        .to_string(),
                ),
                failure_domain: Some("az1-zone-1".to_string()),
                provider_ref: banlieue_api::common::ProviderReference {
                    name: "az1".to_string(),
                    namespace: None,
                },
                zone: "1".to_string(),
                size: "Standard_D2s_v5".to_string(),
                image_version_id: "/subscriptions/s/versions/1.0.0".to_string(),
                os_disk_size_gi_b: 30,
                os_disk_sku: "Premium_LRS".to_string(),
                subnet_id: "/subscriptions/s/subnets/a".to_string(),
                accelerated_networking: false,
                trusted_launch: true,
                user_data: None,
                desired_power_state: PowerState::PoweredOn,
            },
            status: Some(AzureVirtualMachineStatus {
                initialization: InitializationStatus {
                    provisioned: Some(true),
                },
                failure_domain: Some("az1-zone-1".to_string()),
                addresses: vec![MachineAddress {
                    address_type: MachineAddressType::InternalIP,
                    address: "10.0.1.4".to_string(),
                }],
                observed_power_state: Some(PowerState::PoweredOn),
                conditions: vec![Condition {
                    type_: condition_types::READY.to_string(),
                    status: "True".to_string(),
                    reason: "VMRunning".to_string(),
                    message: String::new(),
                    last_transition_time: Time(k8s_openapi::jiff::Timestamp::now()),
                    observed_generation: None,
                }],
                ..Default::default()
            }),
        }
    }

    #[test]
    fn azure_impl_reads_provider_id_from_spec_and_the_rest_from_status() {
        let m = azure_machine();
        assert!(m.provider_id().is_some_and(|p| p.starts_with("azure:///")));
        assert_eq!(m.failure_domain(), Some("az1-zone-1"));
        assert_eq!(m.initialization().provisioned, Some(true));
        assert_eq!(m.observed_power_state(), Some(&PowerState::PoweredOn));
        assert_eq!(m.provider_name(), Some("az1"));
        assert!(
            m.tpm_endorsement_certificates().is_empty(),
            "AK-only in v1 (ADR-0165)"
        );
        assert_eq!(m.attestation_subject(), None);
    }

    #[test]
    fn azure_machine_provisioned_and_addresses_mirror_onto_the_vm() {
        let m = azure_machine();
        let status = mirror_status_from_infra(&VirtualMachineStatus::default(), &m, 1);
        let cond = find_condition(&status.conditions, condition_types::INFRASTRUCTURE_READY)
            .expect("InfrastructureReady must be published");
        assert_eq!(cond.status, condition_status::TRUE);
        assert_eq!(status.initialization.provisioned, Some(true));
        assert_eq!(status.addresses.len(), 1);
        assert_eq!(status.addresses[0].address, "10.0.1.4");
    }

    #[test]
    fn azure_impl_tolerates_an_absent_status() {
        let mut m = azure_machine();
        m.status = None;
        assert_eq!(m.initialization().provisioned, None);
        assert!(m.addresses().is_empty());
        assert!(m.conditions().is_empty());
        assert!(m.observed_power_state().is_none());
    }

    // ======================================================================
    // The concrete EC2Machine impl (ADR-0174, ADR-0167 Decision 1)
    // ======================================================================

    const EC2_EK_RSA: &str = "-----BEGIN PUBLIC KEY-----\nRSA\n-----END PUBLIC KEY-----\n";
    const EC2_EK_ECC: &str = "-----BEGIN PUBLIC KEY-----\nECC\n-----END PUBLIC KEY-----\n";

    fn ec2_machine() -> banlieue_api::infrastructure::EC2Machine {
        use banlieue_api::infrastructure::{
            EC2Machine, EC2MachineSpec, EC2MachineStatus, Ec2VolumeType,
        };
        EC2Machine {
            metadata: Default::default(),
            spec: EC2MachineSpec {
                provider_id: Some("aws:///ca-central-1a/i-0123456789abcdef0".to_string()),
                failure_domain: Some("aws1-ca-central-1a".to_string()),
                provider_ref: banlieue_api::common::ProviderReference {
                    name: "aws1".to_string(),
                    namespace: None,
                },
                zone: "ca-central-1a".to_string(),
                instance_type: "m7i.large".to_string(),
                image_id: "ami-0123456789abcdef0".to_string(),
                root_device_name: "/dev/xvda".to_string(),
                root_volume_gi_b: 30,
                volume_type: Ec2VolumeType::Gp3,
                iops: None,
                throughput: None,
                kms_key_id: None,
                subnet_id: "subnet-0123456789abcdef0".to_string(),
                security_group_ids: vec!["sg-0123456789abcdef0".to_string()],
                tpm_enabled: true,
                user_data: None,
                desired_power_state: PowerState::PoweredOn,
            },
            status: Some(EC2MachineStatus {
                initialization: InitializationStatus {
                    provisioned: Some(true),
                },
                failure_domain: Some("aws1-ca-central-1a".to_string()),
                addresses: vec![MachineAddress {
                    address_type: MachineAddressType::InternalIP,
                    address: "10.0.1.4".to_string(),
                }],
                observed_power_state: Some(PowerState::PoweredOn),
                tpm_endorsement_public_keys: vec![EC2_EK_RSA.to_string(), EC2_EK_ECC.to_string()],
                conditions: vec![
                    cond(
                        condition_types::READY,
                        condition_status::TRUE,
                        "InstanceRunning",
                    ),
                    cond(
                        condition_types::GUEST_READY,
                        condition_status::TRUE,
                        "GuestReported",
                    ),
                ],
                ..Default::default()
            }),
        }
    }

    #[test]
    fn ec2_impl_reads_provider_id_from_spec_and_the_rest_from_status() {
        let m = ec2_machine();
        assert!(m.provider_id().is_some_and(|p| p.starts_with("aws:///")));
        assert_eq!(m.failure_domain(), Some("aws1-ca-central-1a"));
        assert_eq!(m.initialization().provisioned, Some(true));
        assert_eq!(m.observed_power_state(), Some(&PowerState::PoweredOn));
        assert_eq!(m.provider_name(), Some("aws1"));
        assert!(
            m.tpm_endorsement_certificates().is_empty(),
            "no EK certificate on EC2 (ADR-0167)"
        );
        assert_eq!(m.tpm_endorsement_public_keys().len(), 2);
        assert_eq!(m.attestation_subject(), None);
    }

    #[test]
    fn ec2_machine_mirrors_provisioned_addresses_guest_ready_and_ek_public_keys() {
        let m = ec2_machine();
        let status = mirror_status_from_infra(&baseline_status_scheduled(), &m, 1);
        let infra = find_condition(&status.conditions, condition_types::INFRASTRUCTURE_READY)
            .expect("InfrastructureReady must be published");
        assert_eq!(infra.status, condition_status::TRUE);
        let guest = find_condition(&status.conditions, condition_types::GUEST_READY)
            .expect("GuestReady is mirrored");
        assert_eq!(guest.status, condition_status::TRUE);
        assert_eq!(guest.reason, "GuestReported");
        assert_eq!(status.initialization.provisioned, Some(true));
        assert_eq!(status.addresses[0].address, "10.0.1.4");
        assert_eq!(
            status.tpm_endorsement_public_keys,
            vec![EC2_EK_RSA.to_string(), EC2_EK_ECC.to_string()]
        );
        assert!(status.tpm_endorsement_certificates.is_empty());
    }

    #[test]
    fn withdrawn_ek_public_keys_are_cleared_on_the_parent() {
        let mut current = baseline_status_scheduled();
        current.tpm_endorsement_public_keys = vec![EC2_EK_RSA.to_string()];
        let mut m = ec2_machine();
        if let Some(s) = m.status.as_mut() {
            s.tpm_endorsement_public_keys.clear();
        }
        let status = mirror_status_from_infra(&current, &m, 1);
        assert!(status.tpm_endorsement_public_keys.is_empty());
    }

    #[test]
    fn a_kind_without_ek_public_keys_mirrors_none() {
        let mut current = baseline_status_scheduled();
        current.tpm_endorsement_public_keys = vec![EC2_EK_RSA.to_string()];
        let status = mirror_status_from_infra(&current, &azure_machine(), 1);
        assert!(status.tpm_endorsement_public_keys.is_empty());
    }

    #[test]
    fn ec2_impl_tolerates_an_absent_status() {
        let mut m = ec2_machine();
        m.status = None;
        assert_eq!(m.initialization().provisioned, None);
        assert!(m.addresses().is_empty());
        assert!(m.conditions().is_empty());
        assert!(m.observed_power_state().is_none());
        assert!(m.tpm_endorsement_public_keys().is_empty());
    }

    /// A zone EC2 refused (no capacity, a type not offered) reaches the
    /// re-placement path through the same accessor (ADR-0173).
    #[test]
    fn an_ec2_zone_refusal_is_an_allocation_failure() {
        use banlieue_api::banlieue::ScheduledPlacement;
        use banlieue_api::common::allocation;
        let mut m = ec2_machine();
        if let Some(s) = m.status.as_mut() {
            s.initialization.provisioned = None;
            s.conditions = vec![cond(
                allocation::VM_CREATED,
                condition_status::FALSE,
                allocation::ZONE_UNAVAILABLE,
            )];
        }
        let recorded = ScheduledPlacement {
            provider_name: "aws1".into(),
            provider_class: "ec2".into(),
            failure_domain: "aws1-ca-central-1a".into(),
            resolved_storage: Vec::new(),
            resolved_networks: Vec::new(),
            scheduled_at: None,
        };
        let failure = crate::reconciler::placement_failure::allocation_failure(&m, &recorded)
            .expect("ZoneUnavailable is a refusal");
        assert_eq!(failure.failure_domain, "aws1-ca-central-1a");
        assert_eq!(failure.reason, allocation::ZONE_UNAVAILABLE);
    }

    // ======================================================================
    // The concrete FirecrackerMachine impl and attestationSubject (ADR-0143)
    // ======================================================================

    const SUBJECT: &str = "0f3c9a1e-0000-4000-8000-000000000001";

    fn firecracker_machine(
        subject: Option<&str>,
    ) -> banlieue_api::infrastructure::FirecrackerMachine {
        use banlieue_api::infrastructure::{
            FirecrackerMachine, FirecrackerMachineSpec, FirecrackerMachineStatus,
        };
        FirecrackerMachine {
            metadata: Default::default(),
            spec: FirecrackerMachineSpec {
                provider_id: Some("firecracker://fc-a/0f3c9a1e".to_string()),
                failure_domain: None,
                provider_ref: banlieue_api::common::ProviderReference {
                    name: "fc-a".to_string(),
                    namespace: None,
                },
                vcpus: 2,
                memory_mi_b: 1024,
                image: "microvm".to_string(),
                nics: vec![],
                user_data: None,
                desired_power_state: PowerState::PoweredOn,
            },
            status: Some(FirecrackerMachineStatus {
                initialization: InitializationStatus {
                    provisioned: Some(true),
                },
                failure_domain: Some("fc-a".to_string()),
                observed_power_state: Some(PowerState::PoweredOn),
                attestation_subject: subject.map(str::to_string),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn firecracker_impl_reads_provider_id_from_spec_and_the_rest_from_status() {
        let m = firecracker_machine(Some(SUBJECT));
        assert_eq!(m.provider_id(), Some("firecracker://fc-a/0f3c9a1e"));
        assert_eq!(m.failure_domain(), Some("fc-a"));
        assert_eq!(m.initialization().provisioned, Some(true));
        assert_eq!(m.observed_power_state(), Some(&PowerState::PoweredOn));
        assert_eq!(m.desired_power_state(), &PowerState::PoweredOn);
        assert!(m.tpm_endorsement_certificates().is_empty());
    }

    #[test]
    fn firecracker_impl_tolerates_an_absent_status() {
        let mut m = firecracker_machine(Some(SUBJECT));
        m.status = None;
        assert_eq!(m.initialization().provisioned, None);
        assert!(m.addresses().is_empty());
        assert!(m.conditions().is_empty());
        assert_eq!(m.attestation_subject(), None);
    }

    /// The subject is a fact the machine publishes; the trait must hand it
    /// over verbatim, not a value derived from anything else.
    #[test]
    fn firecracker_impl_reads_the_attestation_subject_from_status() {
        assert_eq!(
            firecracker_machine(Some(SUBJECT)).attestation_subject(),
            Some(SUBJECT)
        );
        assert_eq!(firecracker_machine(None).attestation_subject(), None);
    }

    #[test]
    fn the_attestation_subject_is_mirrored_from_the_infra_cr() {
        let infra = firecracker_machine(Some(SUBJECT));
        let next = mirror_status_from_infra(&VirtualMachineStatus::default(), &infra, 1);
        assert_eq!(next.attestation_subject.as_deref(), Some(SUBJECT));
    }

    /// A machine that stops publishing a subject must not leave a stale one
    /// on the parent: the claim would hand mediatore a parent ID that no
    /// longer exists.
    #[test]
    fn a_withdrawn_attestation_subject_is_cleared_on_the_parent() {
        let current = VirtualMachineStatus {
            attestation_subject: Some(SUBJECT.to_string()),
            ..Default::default()
        };
        let next = mirror_status_from_infra(&current, &firecracker_machine(None), 1);
        assert_eq!(next.attestation_subject, None);
    }

    /// Every other kind has no such fact, and the parent must not invent one.
    #[test]
    fn a_kind_without_an_attestation_subject_mirrors_none() {
        let infra = cloud_hypervisor_machine();
        let current = VirtualMachineStatus {
            attestation_subject: Some(SUBJECT.to_string()),
            ..Default::default()
        };
        let next = mirror_status_from_infra(&current, &infra, 1);
        assert_eq!(next.attestation_subject, None);
    }

    // ADR-0146: a Cloud Hypervisor guest may attest with x509pop too.

    fn cloud_hypervisor_machine_with_subject(
        subject: Option<&str>,
    ) -> banlieue_api::infrastructure::CloudHypervisorMachine {
        let mut m = cloud_hypervisor_machine();
        if let Some(st) = m.status.as_mut() {
            st.attestation_subject = subject.map(str::to_string);
        }
        m
    }

    #[test]
    fn cloud_hypervisor_impl_reads_the_attestation_subject_from_status() {
        assert_eq!(
            cloud_hypervisor_machine_with_subject(Some(SUBJECT)).attestation_subject(),
            Some(SUBJECT)
        );
        assert_eq!(
            cloud_hypervisor_machine_with_subject(None).attestation_subject(),
            None
        );
    }

    #[test]
    fn the_cloud_hypervisor_attestation_subject_is_mirrored_from_the_infra_cr() {
        let infra = cloud_hypervisor_machine_with_subject(Some(SUBJECT));
        let next = mirror_status_from_infra(&VirtualMachineStatus::default(), &infra, 1);
        assert_eq!(next.attestation_subject.as_deref(), Some(SUBJECT));
    }

    #[test]
    fn a_withdrawn_cloud_hypervisor_attestation_subject_is_cleared_on_the_parent() {
        let current = VirtualMachineStatus {
            attestation_subject: Some(SUBJECT.to_string()),
            ..Default::default()
        };
        let infra = cloud_hypervisor_machine_with_subject(None);
        let next = mirror_status_from_infra(&current, &infra, 1);
        assert_eq!(next.attestation_subject, None);
    }
}
