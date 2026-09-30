// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `proxmox_machine.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    fn sample_nic(name: &str) -> ProxmoxNicSpec {
        ProxmoxNicSpec {
            name: name.to_string(),
            bridge: "vmbr0".to_string(),
            vlan: None,
            model: ProxmoxNicModel::default(),
            mac_address: None,
            ipam: IpamSpec::default(),
        }
    }

    fn minimal_spec() -> ProxmoxMachineSpec {
        ProxmoxMachineSpec {
            provider_id: None,
            failure_domain: None,
            provider_ref: LocalObjectReference {
                name: "pve-a".to_string(),
            },
            node: "pve1".to_string(),
            template_vmid: 9000,
            storage: "local-lvm".to_string(),
            pool: None,
            cores: 2,
            sockets: 1,
            memory_mi_b: 4096,
            cpu_type: None,
            firmware: Firmware::Efi,
            tpm_enabled: false,
            os_disk_size_gi_b: 20,
            data_disks: vec![],
            nics: vec![sample_nic("eth0")],
            iso_storage: None,
            user_data: None,
            desired_power_state: PowerState::PoweredOn,
        }
    }

    // ------------------------------------------------------------------
    // Round-tripping and wire names
    // ------------------------------------------------------------------

    #[test]
    fn spec_round_trips_through_json() {
        let spec = minimal_spec();
        let back: ProxmoxMachineSpec =
            serde_json::from_value(serde_json::to_value(&spec).unwrap()).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn spec_serializes_camel_case() {
        let json = serde_json::to_value(minimal_spec()).unwrap();
        for key in [
            "providerRef",
            "node",
            "templateVmid",
            "storage",
            "cores",
            "sockets",
            "memoryMiB",
            "firmware",
            "osDiskSizeGiB",
            "nics",
            "desiredPowerState",
        ] {
            assert!(json.get(key).is_some(), "missing {key}: {json}");
        }
        assert_eq!(json["nics"][0]["bridge"], "vmbr0");
        assert_eq!(json["nics"][0]["model"], "virtio");
    }

    #[test]
    fn provider_id_uses_the_capi_spelling() {
        let mut spec = minimal_spec();
        spec.provider_id = Some("proxmox://pve-a/100".to_string());
        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("providerID").is_some(), "{json}");
        assert!(json.get("providerId").is_none(), "{json}");
    }

    /// ADR-0075 Decision 2: provider name and VMID, never the node.
    #[test]
    fn provider_id_is_scheme_provider_vmid() {
        assert_eq!(proxmox_provider_id("pve-a", 100), "proxmox://pve-a/100");
    }

    #[test]
    fn optional_fields_default_when_absent() {
        let json = serde_json::json!({
            "providerRef": { "name": "pve-a" },
            "node": "pve1",
            "templateVmid": 9000,
            "storage": "local-lvm",
            "cores": 2,
            "memoryMiB": 2048,
            "osDiskSizeGiB": 20,
            "nics": [],
        });
        let s: ProxmoxMachineSpec = serde_json::from_value(json).unwrap();
        assert_eq!(s.sockets, 1);
        assert!(!s.tpm_enabled);
        assert!(s.data_disks.is_empty());
        assert_eq!(s.iso_storage, None);
        assert_eq!(s.firmware, Firmware::Efi);
        assert_eq!(s.desired_power_state, PowerState::PoweredOn);
    }

    /// The VMID is the provider's to allocate (ADR-0075 Decision 4): a spec
    /// field would let two writers pick one.
    #[test]
    fn spec_has_no_vmid_field() {
        let json = serde_json::to_value(minimal_spec()).unwrap();
        assert!(json.get("vmid").is_none(), "{json}");
    }

    // ------------------------------------------------------------------
    // Firmware
    // ------------------------------------------------------------------

    #[test]
    fn bios_firmware_is_seabios_without_secure_boot() {
        let mut s = minimal_spec();
        s.firmware = Firmware::Bios;
        assert_eq!(s.bios(), "seabios");
        assert!(!s.secure_boot());
    }

    #[test]
    fn efi_firmware_is_ovmf_without_secure_boot() {
        let s = minimal_spec();
        assert_eq!(s.bios(), "ovmf");
        assert!(!s.secure_boot());
    }

    #[test]
    fn efi_secure_is_ovmf_with_secure_boot() {
        let mut s = minimal_spec();
        s.firmware = Firmware::EfiSecure;
        assert_eq!(s.bios(), "ovmf");
        assert!(s.secure_boot());
    }

    // ------------------------------------------------------------------
    // Cloud-init seed
    // ------------------------------------------------------------------

    #[test]
    fn no_user_data_means_no_seed() {
        assert!(!minimal_spec().needs_seed());
    }

    #[test]
    fn user_data_means_a_seed() {
        let mut s = minimal_spec();
        s.user_data = Some("#cloud-config\n".to_string());
        assert!(s.needs_seed());
    }

    /// ADR-0075 Decision 3: `isoStorage` must be set whenever `userData` is.
    #[test]
    fn validate_rejects_user_data_without_an_iso_storage() {
        let mut s = minimal_spec();
        s.user_data = Some("#cloud-config\n".to_string());
        let e = s.validate().unwrap_err();
        assert!(e.contains("isoStorage"), "{e}");
    }

    #[test]
    fn validate_accepts_user_data_with_an_iso_storage() {
        let mut s = minimal_spec();
        s.user_data = Some("#cloud-config\n".to_string());
        s.iso_storage = Some("local".to_string());
        s.validate().unwrap();
    }

    #[test]
    fn validate_accepts_a_minimal_spec() {
        minimal_spec().validate().unwrap();
    }

    #[test]
    fn validate_rejects_duplicate_nic_names() {
        let mut s = minimal_spec();
        s.nics = vec![sample_nic("eth0"), sample_nic("eth0")];
        assert!(s.validate().unwrap_err().contains("eth0"));
    }

    #[test]
    fn validate_rejects_duplicate_data_disk_names() {
        let mut s = minimal_spec();
        let d = ProxmoxDataDisk {
            name: "data".to_string(),
            size_gi_b: 10,
            storage: None,
            iothread: false,
            discard: false,
            ssd: false,
        };
        s.data_disks = vec![d.clone(), d];
        assert!(s.validate().unwrap_err().contains("data"));
    }

    #[test]
    fn validate_rejects_a_vlan_outside_802_1q() {
        for bad in [0u16, 4095] {
            let mut s = minimal_spec();
            s.nics[0].vlan = Some(bad);
            assert!(s.validate().is_err(), "vlan {bad} should be rejected");
        }
        let mut s = minimal_spec();
        s.nics[0].vlan = Some(100);
        s.validate().unwrap();
    }

    /// Proxmox names a VM disk `scsi0`..`scsi30`; the OS disk takes `scsi0`.
    #[test]
    fn validate_rejects_more_data_disks_than_scsi_slots_allow() {
        let mut s = minimal_spec();
        s.data_disks = (0..MAX_DATA_DISKS + 1)
            .map(|i| ProxmoxDataDisk {
                name: format!("d{i}"),
                size_gi_b: 1,
                storage: None,
                iothread: false,
                discard: false,
                ssd: false,
            })
            .collect();
        assert!(s.validate().is_err());
    }

    // ------------------------------------------------------------------
    // NIC rendering
    // ------------------------------------------------------------------

    #[test]
    fn nic_model_defaults_to_virtio() {
        assert_eq!(ProxmoxNicModel::default().as_str(), "virtio");
        assert_eq!(ProxmoxNicModel::E1000.as_str(), "e1000");
        assert_eq!(ProxmoxNicModel::Vmxnet3.as_str(), "vmxnet3");
        assert_eq!(ProxmoxNicModel::Rtl8139.as_str(), "rtl8139");
    }

    #[test]
    fn nic_config_value_minimal() {
        assert_eq!(sample_nic("eth0").config_value(), "virtio,bridge=vmbr0");
    }

    #[test]
    fn nic_config_value_with_mac_and_vlan() {
        let mut n = sample_nic("eth0");
        n.mac_address = Some("BC:24:11:00:00:01".to_string());
        n.vlan = Some(30);
        n.model = ProxmoxNicModel::E1000;
        assert_eq!(
            n.config_value(),
            "e1000=BC:24:11:00:00:01,bridge=vmbr0,tag=30"
        );
    }

    // ------------------------------------------------------------------
    // Status
    // ------------------------------------------------------------------

    #[test]
    fn status_defaults_to_not_provisioned_and_serialises_empty() {
        let st = ProxmoxMachineStatus::default();
        assert_ne!(st.initialization.provisioned, Some(true));
        let json = serde_json::to_value(&st).unwrap();
        assert!(json.get("vmid").is_none(), "{json}");
        assert!(json.get("addresses").is_none(), "{json}");
    }

    #[test]
    fn status_round_trips() {
        let st = ProxmoxMachineStatus {
            vmid: Some(100),
            node: Some("pve1".to_string()),
            address_source: Some(ProxmoxAddressSource::GuestAgent),
            observed_power_state: Some(PowerState::PoweredOn),
            tpm_attached: Some(true),
            observed_generation: Some(3),
            ..Default::default()
        };
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(json["vmid"], 100);
        assert_eq!(json["addressSource"], "guestAgent");
        let back: ProxmoxMachineStatus = serde_json::from_value(json).unwrap();
        assert_eq!(st, back);
    }

    // ------------------------------------------------------------------
    // CRD generation
    // ------------------------------------------------------------------

    #[test]
    fn crd_has_the_expected_identity() {
        use kube::CustomResourceExt;
        let crd = ProxmoxMachine::crd();
        assert_eq!(crd.spec.group, "infrastructure.banlieue.io");
        assert_eq!(crd.spec.names.kind, "ProxmoxMachine");
        assert_eq!(crd.spec.names.plural, "proxmoxmachines");
        assert_eq!(crd.spec.scope, "Namespaced");
    }

    /// The InfraMachine contract requires a status subresource.
    #[test]
    fn crd_has_a_status_subresource() {
        use kube::CustomResourceExt;
        let v = &ProxmoxMachine::crd().spec.versions[0];
        assert!(v.subresources.as_ref().is_some_and(|s| s.status.is_some()));
    }

    #[test]
    fn template_crd_has_the_expected_identity() {
        use kube::CustomResourceExt;
        let crd = ProxmoxMachineTemplate::crd();
        assert_eq!(crd.spec.names.kind, "ProxmoxMachineTemplate");
        assert_eq!(crd.spec.names.plural, "proxmoxmachinetemplates");
    }

    #[test]
    fn template_wraps_a_machine_spec() {
        let t = ProxmoxMachineTemplateSpec {
            template: ProxmoxMachineTemplateResource {
                spec: minimal_spec(),
            },
        };
        let json = serde_json::to_value(&t).unwrap();
        assert!(
            json["template"]["spec"]["templateVmid"].is_number(),
            "{json}"
        );
    }

    /// One list feeds crdgen, the API reference and `banlieue bootstrap`.
    #[test]
    fn both_crds_are_registered_and_carry_the_capi_label() {
        let crds = crate::crdgen_support::all_crds();
        for kind in ["ProxmoxMachine", "ProxmoxMachineTemplate"] {
            let crd = crds
                .iter()
                .find(|c| c.spec.names.kind == kind)
                .unwrap_or_else(|| panic!("{kind} is not in all_crds()"));
            let labels = crd.metadata.labels.as_ref().expect("labels");
            assert_eq!(
                labels.get("cluster.x-k8s.io/v1beta2").map(String::as_str),
                Some("v1alpha1"),
                "{kind} lacks the CAPI contract label"
            );
        }
    }
}
