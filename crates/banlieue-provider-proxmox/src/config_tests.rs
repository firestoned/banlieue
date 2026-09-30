// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `config.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::common::{Firmware, IpamSpec, LocalObjectReference, PowerState};
    use banlieue_api::infrastructure::{
        ProxmoxDataDisk, ProxmoxMachineSpec, ProxmoxNicModel, ProxmoxNicSpec,
    };
    use banlieue_proxmox::VmConfig;
    use std::collections::BTreeMap;

    fn spec() -> ProxmoxMachineSpec {
        ProxmoxMachineSpec {
            provider_id: None,
            failure_domain: None,
            provider_ref: LocalObjectReference {
                name: "pve-a".into(),
            },
            node: "pve1".into(),
            template_vmid: 9000,
            storage: "local-lvm".into(),
            pool: None,
            cores: 2,
            sockets: 1,
            memory_mi_b: 4096,
            cpu_type: None,
            firmware: Firmware::Bios,
            tpm_enabled: false,
            os_disk_size_gi_b: 20,
            data_disks: vec![],
            nics: vec![ProxmoxNicSpec {
                name: "eth0".into(),
                bridge: "vmbr0".into(),
                vlan: None,
                model: ProxmoxNicModel::Virtio,
                mac_address: None,
                ipam: IpamSpec::default(),
            }],
            iso_storage: None,
            user_data: None,
            desired_power_state: PowerState::PoweredOn,
        }
    }

    fn existing(pairs: &[(&str, &str)]) -> VmConfig {
        VmConfig(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    fn cfg(s: &ProxmoxMachineSpec, ex: &VmConfig, seed: Option<&str>) -> banlieue_proxmox::Params {
        desired_config(s, &s.nics, ex, seed)
    }

    #[test]
    fn basic_keys_come_from_the_spec() {
        let p = cfg(&spec(), &existing(&[]), None);
        assert_eq!(p.get("cores"), Some("2"));
        assert_eq!(p.get("sockets"), Some("1"));
        assert_eq!(p.get("memory"), Some("4096"));
        assert_eq!(p.get("bios"), Some("seabios"));
        assert_eq!(p.get("agent"), Some("1"));
        assert_eq!(p.get("net0"), Some("virtio,bridge=vmbr0"));
        assert_eq!(p.get("cpu"), None);
        assert_eq!(p.get("ide2"), None);
    }

    #[test]
    fn cpu_type_is_set_only_when_given() {
        let mut s = spec();
        s.cpu_type = Some("host".into());
        assert_eq!(cfg(&s, &existing(&[]), None).get("cpu"), Some("host"));
    }

    #[test]
    fn efi_adds_an_efidisk_when_the_template_has_none() {
        let mut s = spec();
        s.firmware = Firmware::Efi;
        let p = cfg(&s, &existing(&[]), None);
        assert_eq!(p.get("bios"), Some("ovmf"));
        assert_eq!(
            p.get("efidisk0"),
            Some("local-lvm:1,efitype=4m,pre-enrolled-keys=0")
        );
    }

    #[test]
    fn efi_secure_pre_enrols_keys() {
        let mut s = spec();
        s.firmware = Firmware::EfiSecure;
        let p = cfg(&s, &existing(&[]), None);
        assert!(p.get("efidisk0").unwrap().ends_with("pre-enrolled-keys=1"));
    }

    #[test]
    fn an_existing_efidisk_is_never_replaced() {
        let mut s = spec();
        s.firmware = Firmware::Efi;
        let ex = existing(&[("efidisk0", "local-lvm:vm-1-disk-1,efitype=4m")]);
        assert_eq!(cfg(&s, &ex, None).get("efidisk0"), None);
    }

    #[test]
    fn bios_never_adds_an_efidisk() {
        assert_eq!(cfg(&spec(), &existing(&[]), None).get("efidisk0"), None);
    }

    #[test]
    fn tpm_adds_a_v2_state_disk_once() {
        let mut s = spec();
        s.tpm_enabled = true;
        assert_eq!(
            cfg(&s, &existing(&[]), None).get("tpmstate0"),
            Some("local-lvm:1,version=v2.0")
        );
        let ex = existing(&[("tpmstate0", "local-lvm:vm-1-disk-2,version=v2.0")]);
        assert_eq!(cfg(&s, &ex, None).get("tpmstate0"), None);
    }

    #[test]
    fn data_disks_take_scsi1_upwards_and_default_to_the_machine_storage() {
        let mut s = spec();
        s.data_disks = vec![
            ProxmoxDataDisk {
                name: "a".into(),
                size_gi_b: 10,
                storage: None,
                iothread: false,
                discard: false,
                ssd: false,
            },
            ProxmoxDataDisk {
                name: "b".into(),
                size_gi_b: 50,
                storage: Some("fast".into()),
                iothread: true,
                discard: true,
                ssd: true,
            },
        ];
        let p = cfg(&s, &existing(&[]), None);
        assert_eq!(p.get("scsi1"), Some("local-lvm:10"));
        assert_eq!(p.get("scsi2"), Some("fast:50,iothread=1,discard=on,ssd=1"));
        assert_eq!(p.get("scsihw"), Some("virtio-scsi-single"));
    }

    #[test]
    fn an_existing_data_disk_is_not_recreated() {
        let mut s = spec();
        s.data_disks = vec![ProxmoxDataDisk {
            name: "a".into(),
            size_gi_b: 10,
            storage: None,
            iothread: false,
            discard: false,
            ssd: false,
        }];
        let ex = existing(&[("scsi1", "local-lvm:vm-1-disk-3,size=10G")]);
        assert_eq!(cfg(&s, &ex, None).get("scsi1"), None);
    }

    #[test]
    fn no_iothread_means_no_scsihw_override() {
        assert_eq!(cfg(&spec(), &existing(&[]), None).get("scsihw"), None);
    }

    #[test]
    fn the_seed_is_attached_as_a_cdrom_on_ide2() {
        let p = cfg(&spec(), &existing(&[]), Some("local:iso/banlieue-u.iso"));
        assert_eq!(p.get("ide2"), Some("local:iso/banlieue-u.iso,media=cdrom"));
    }

    #[test]
    fn seed_names_derive_from_the_machine_uid() {
        assert_eq!(seed_filename("abc"), "banlieue-abc.iso");
        assert_eq!(seed_volid("local", "abc"), "local:iso/banlieue-abc.iso");
    }

    #[test]
    fn disk_sizes_round_up_to_whole_gib() {
        let g = |v: &str| os_disk_size_gib(&existing(&[("scsi0", v)]));
        assert_eq!(g("local-lvm:vm-1-disk-0,size=20G"), Some(20));
        assert_eq!(g("local-lvm:vm-1-disk-0,iothread=1,size=2T"), Some(2048));
        assert_eq!(g("local-lvm:vm-1-disk-0,size=1500M"), Some(2));
        assert_eq!(g("local-lvm:vm-1-disk-0,size=1024M"), Some(1));
        assert_eq!(g("local-lvm:vm-1-disk-0,size=1073741824"), Some(1));
    }

    #[test]
    fn a_missing_or_unparseable_size_is_none() {
        assert_eq!(os_disk_size_gib(&existing(&[])), None);
        assert_eq!(
            os_disk_size_gib(&existing(&[("scsi0", "local-lvm:x")])),
            None
        );
        assert_eq!(
            os_disk_size_gib(&existing(&[("scsi0", "x,size=abc")])),
            None
        );
    }

    // ---- ownership marker -------------------------------------------

    fn described(text: &str) -> VmConfig {
        existing(&[("description", text)])
    }

    #[test]
    fn the_marker_is_one_line_naming_the_machine_uid() {
        assert_eq!(ownership_marker("abc"), "banlieue-machine-uid=abc");
        assert!(!ownership_marker("abc").contains('\n'));
    }

    #[test]
    fn a_vm_carrying_the_marker_is_ours() {
        assert!(is_ours(&described(&ownership_marker("abc")), "abc"));
    }

    #[test]
    fn a_vm_with_no_description_is_not_ours() {
        assert!(!is_ours(&existing(&[]), "abc"));
        assert!(!is_ours(&described(""), "abc"));
    }

    #[test]
    fn another_machines_marker_is_not_ours() {
        assert!(!is_ours(&described(&ownership_marker("xyz")), "abc"));
    }

    /// A uid that is a prefix of another must not collide.
    #[test]
    fn a_uid_prefix_does_not_match_a_longer_uid() {
        assert!(!is_ours(&described(&ownership_marker("abcd")), "abc"));
        assert!(!is_ours(&described(&ownership_marker("abc")), "abcd"));
    }

    #[test]
    fn the_uid_must_not_merely_appear_inside_the_text() {
        assert!(!is_ours(
            &described("notes about banlieue-machine-uid=abc here"),
            "abc"
        ));
        assert!(!is_ours(&described("abc"), "abc"));
    }

    /// An admin may write notes around the marker line without orphaning it.
    #[test]
    fn text_around_the_marker_line_is_harmless() {
        let d = format!(
            "Owned by the web team\n{}\nrenew in 2027\n",
            ownership_marker("abc")
        );
        assert!(is_ours(&described(&d), "abc"));
    }

    #[test]
    fn the_marker_line_tolerates_surrounding_whitespace_and_crlf() {
        let d = format!("  {}  \r\nother", ownership_marker("abc"));
        assert!(is_ours(&described(&d), "abc"));
    }
}
