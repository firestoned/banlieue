// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `plan.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::host_config::HostConfig;
    use banlieue_api::common::{IpamSpec, LocalObjectReference, PowerState};
    use banlieue_api::infrastructure::{
        ChBootSource, ChBootSourceKind, ChCpuSpec, ChMemorySpec, ChNicSpec,
        CloudHypervisorMachineSpec,
    };
    use std::collections::BTreeSet;
    use std::path::Path;

    const UID: &str = "0f3c9a1e-5b7d-4e2a-9c11-3f2b7a5d9e01";

    const MACHINE_NAME: &str = "m1";
    const HOST_UID: u32 = 2_000_007;

    fn config() -> HostConfig {
        HostConfig::parse(
            r#"
[provider]
name = "ch-a"
namespace = "banlieue-system"
kubeconfig = "/etc/banlieue/kubeconfig"
[vmm]
binary = "/usr/local/bin/cloud-hypervisor"
version = "v53.0"
firmware = "/opt/banlieue/firmware/ch-97eeb7b09/CLOUDHV.fd"
[paths]
run_root = "/run/banlieue/ch"
state_root = "/var/lib/banlieue"
[guests]
uid_base = 2000000
uid_count = 10000
[storage_classes]
fast = "/srv/banlieue/ch"
[network_classes]
lan = "br0"
"#,
        )
        .unwrap()
    }

    fn spec() -> CloudHypervisorMachineSpec {
        CloudHypervisorMachineSpec {
            provider_id: None,
            failure_domain: Some("ch-a".into()),
            provider_ref: LocalObjectReference {
                name: "ch-a".into(),
            },
            cpus: ChCpuSpec { boot: 2, max: None },
            memory: ChMemorySpec {
                size_mi_b: 4096,
                hugepages: false,
            },
            storage_class: "fast".into(),
            boot_source: ChBootSource {
                kind: ChBootSourceKind::Image,
                image: "kairos-ubuntu-2404.raw".into(),
            },
            os_disk_size_gi_b: 20,
            nics: vec![ChNicSpec {
                name: "eth0".into(),
                network_class: "lan".into(),
                mac_address: None,
                ipam: IpamSpec::default(),
            }],
            tpm_enabled: false,
            user_data: Some("#cloud-config\n".into()),
            desired_power_state: PowerState::PoweredOn,
        }
    }

    fn plan() -> MachinePlan {
        plan_machine(UID, MACHINE_NAME, "ch-a", &spec(), &config(), HOST_UID).expect("plan")
    }

    // ------------------------------------------------------------------
    // Names — every host object traces back to one machine (ADR-0062 D2)
    // ------------------------------------------------------------------

    /// Instances of the root-owned templates, keyed by the guest's host uid:
    /// the provider names an instance, never what it runs.
    #[test]
    fn units_are_template_instances_keyed_by_the_guest_uid() {
        assert_eq!(plan().unit, format!("banlieue-ch@{HOST_UID}.service"));
        let t = tpm_plan();
        let tpm = t.tpm.as_ref().unwrap();
        assert_eq!(tpm.unit, format!("banlieue-swtpm@{HOST_UID}.service"));
        assert_eq!(
            tpm.setup_unit,
            format!("banlieue-swtpm-setup@{HOST_UID}.service")
        );
    }

    /// Every unit the provider starts has a template file in deploy/, which
    /// the bootstrap installs; a name with no template is a unit systemd
    /// cannot load. (What polkit allows is `scripts/test-cloud-hypervisor-polkit.js`.)
    #[test]
    fn every_unit_the_provider_starts_has_an_installed_template() {
        let t = tpm_plan();
        let tpm = t.tpm.as_ref().unwrap();
        let import = crate::vmimage::import_unit_name(UID).unwrap();
        let host = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/provider-cloud-hypervisor/host"
        );
        for unit in [&plan().unit, &tpm.unit, &tpm.setup_unit, &import] {
            let (template, _) = unit.split_once('@').expect("an instance");
            let file = Path::new(host).join(format!("{template}@.service"));
            assert!(file.is_file(), "{unit}: no {}", file.display());
        }
    }

    /// Linux interface names are at most 15 characters.
    #[test]
    fn tap_names_fit_ifnamsiz_and_are_per_nic() {
        let p = plan();
        assert_eq!(p.nics.len(), 1);
        let tap = &p.nics[0].tap;
        assert!(tap.len() <= crate::host_config::MAX_INTERFACE_NAME, "{tap}");
        assert!(tap.starts_with("bch"), "{tap}");
        assert_eq!(tap, "bch0f3c9a1e5b0");
    }

    #[test]
    fn the_nic_resolves_its_bridge_from_the_host_config() {
        assert_eq!(plan().nics[0].bridge, "br0");
    }

    /// A derived MAC is locally administered and unicast, and the same on
    /// every reconcile, so the guest keeps its address across restarts.
    #[test]
    fn a_derived_mac_is_stable_local_and_unicast() {
        let mac = plan().nics[0].mac.clone();
        assert_eq!(mac, plan().nics[0].mac);
        let first = u8::from_str_radix(&mac[0..2], 16).unwrap();
        assert_eq!(first & 0b10, 0b10, "locally administered: {mac}");
        assert_eq!(first & 0b01, 0, "unicast: {mac}");
        assert_eq!(mac.split(':').count(), 6);
    }

    #[test]
    fn two_nics_get_different_taps_and_macs() {
        let mut s = spec();
        let mut second = s.nics[0].clone();
        second.name = "eth1".into();
        s.nics.push(second);
        let p = plan_machine(UID, MACHINE_NAME, "ch-a", &s, &config(), HOST_UID).unwrap();
        assert_ne!(p.nics[0].tap, p.nics[1].tap);
        assert_ne!(p.nics[0].mac, p.nics[1].mac);
    }

    #[test]
    fn an_explicit_mac_is_used_as_given() {
        let mut s = spec();
        s.nics[0].mac_address = Some("52:54:00:ab:cd:ef".into());
        let p = plan_machine(UID, MACHINE_NAME, "ch-a", &s, &config(), HOST_UID).unwrap();
        assert_eq!(p.nics[0].mac, "52:54:00:ab:cd:ef");
    }

    // ------------------------------------------------------------------
    // Paths
    // ------------------------------------------------------------------

    #[test]
    fn files_live_in_a_per_machine_directory_under_the_storage_class() {
        let p = plan();
        let dir = Path::new("/srv/banlieue/ch").join(UID);
        assert_eq!(p.machine_dir, dir);
        assert_eq!(p.os_disk, dir.join("os.raw"));
        assert_eq!(p.seed, dir.join("seed.iso"));
        assert_eq!(p.serial_log, dir.join("serial.log"));
    }

    #[test]
    fn sockets_live_in_a_per_guest_run_directory() {
        let p = plan();
        // Keyed by the guest's host uid: the VMM template derives
        // `@RUN_ROOT@/%i/api.sock` from its instance.
        assert_eq!(
            p.run_dir,
            Path::new("/run/banlieue/ch").join(HOST_UID.to_string())
        );
        assert_eq!(p.api_socket, p.run_dir.join("api.sock"));
    }

    #[test]
    fn the_image_comes_from_the_storage_class_image_cache() {
        assert_eq!(
            plan().image,
            Path::new("/srv/banlieue/ch/images/kairos-ubuntu-2404.raw")
        );
    }

    /// Grown before first boot (ADR-0062 Decision 3).
    #[test]
    fn the_os_disk_size_is_in_bytes() {
        assert_eq!(plan().os_disk_bytes, 20 * 1024 * 1024 * 1024);
    }

    #[test]
    fn the_provider_id_names_provider_and_machine_uid() {
        assert_eq!(plan().provider_id, format!("cloudhypervisor://ch-a/{UID}"));
    }

    // ------------------------------------------------------------------
    // The VMM plan
    // ------------------------------------------------------------------

    /// Boot order: OS disk first, then the seed (roadmap 09 Gotchas).
    #[test]
    fn the_guest_boots_the_os_disk_with_the_seed_second_and_read_only() {
        let g = plan().guest;
        assert_eq!(g.disks.len(), 2);
        assert_eq!(g.disks[0].id, "os");
        assert!(!g.disks[0].readonly);
        assert_eq!(g.disks[1].id, "seed");
        assert!(g.disks[1].readonly);
    }

    /// No user-data, no seed disk.
    #[test]
    fn without_user_data_there_is_no_seed_disk() {
        let mut s = spec();
        s.user_data = None;
        let p = plan_machine(UID, MACHINE_NAME, "ch-a", &s, &config(), HOST_UID).unwrap();
        assert_eq!(p.guest.disks.len(), 1);
        assert!(!p.needs_seed());
    }

    #[test]
    fn the_guest_plan_carries_firmware_cpus_memory_and_taps() {
        let p = plan();
        let g = &p.guest;
        assert_eq!(
            g.firmware,
            Path::new("/opt/banlieue/firmware/ch-97eeb7b09/CLOUDHV.fd")
        );
        assert_eq!(g.boot_vcpus, 2);
        assert_eq!(g.max_vcpus, 2);
        assert_eq!(g.memory_mib, 4096);
        assert_eq!(g.nics[0].tap, p.nics[0].tap);
        assert_eq!(g.nics[0].mac, p.nics[0].mac);
        assert_eq!(g.serial_file, p.serial_log);
        assert!(g.tpm_socket.is_none());
    }

    // ------------------------------------------------------------------
    // Refused: cluster input the host must not act on
    // ------------------------------------------------------------------

    /// The UID names units and directories, so it must be exactly a UUID.
    #[test]
    fn a_uid_that_is_not_a_uuid_is_refused() {
        for bad in [
            "",
            "../../etc",
            "0f3c9a1e",
            "0F3C9A1E-5B7D-4E2A-9C11-3F2B7A5D9E01x",
        ] {
            assert!(
                matches!(
                    plan_machine(bad, MACHINE_NAME, "ch-a", &spec(), &config(), HOST_UID),
                    Err(PlanError::InvalidUid(_))
                ),
                "{bad:?}"
            );
        }
    }

    /// The image name is joined onto a host path: it must not escape.
    #[test]
    fn an_image_name_that_could_escape_the_cache_is_refused() {
        for bad in ["../os.raw", "a/b.raw", "", ".hidden", "..", "x\0y"] {
            let mut s = spec();
            s.boot_source.image = bad.into();
            assert!(
                matches!(
                    plan_machine(UID, MACHINE_NAME, "ch-a", &s, &config(), HOST_UID),
                    Err(PlanError::InvalidImage(_))
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn an_undeclared_class_is_refused_by_name() {
        let mut s = spec();
        s.storage_class = "gold".into();
        let e = plan_machine(UID, MACHINE_NAME, "ch-a", &s, &config(), HOST_UID).unwrap_err();
        assert!(
            matches!(e, PlanError::UnknownStorageClass(ref n) if n == "gold"),
            "{e:?}"
        );

        let mut s = spec();
        s.nics[0].network_class = "dmz".into();
        let e = plan_machine(UID, MACHINE_NAME, "ch-a", &s, &config(), HOST_UID).unwrap_err();
        assert!(
            matches!(e, PlanError::UnknownNetworkClass(ref n) if n == "dmz"),
            "{e:?}"
        );
    }

    #[test]
    fn a_host_uid_outside_the_guest_range_is_refused() {
        for bad in [0, 1000, 1_999_999, 2_010_000] {
            assert!(
                matches!(
                    plan_machine(UID, MACHINE_NAME, "ch-a", &spec(), &config(), bad),
                    Err(PlanError::HostUidOutOfRange(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_malformed_explicit_mac_is_refused() {
        let mut s = spec();
        s.nics[0].mac_address = Some("not-a-mac".into());
        assert!(matches!(
            plan_machine(UID, MACHINE_NAME, "ch-a", &s, &config(), HOST_UID),
            Err(PlanError::InvalidMac(_))
        ));
    }

    fn deferred_plan() -> MachinePlan {
        let mut s = spec();
        s.boot_source = ChBootSource {
            kind: ChBootSourceKind::InstallMedia,
            image: "kairos-installer.iso".into(),
        };
        plan_machine(UID, MACHINE_NAME, "ch-a", &s, &config(), HOST_UID).unwrap()
    }

    /// ADR-0065 Decision 3: an empty OS disk first, the installer second
    /// (read-only, id `install`), then the seed. The firmware skips the
    /// empty disk and boots the installer; after the install the first
    /// disk wins.
    #[test]
    fn deferred_install_puts_an_empty_disk_first_and_the_installer_second() {
        let p = deferred_plan();
        assert!(p.empty_os_disk);
        let ids: Vec<&str> = p.guest.disks.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, vec![DISK_ID_OS, DISK_ID_INSTALL, DISK_ID_SEED]);
        let install = &p.guest.disks[1];
        assert!(install.readonly);
        // The guest's own copy, not the cache: guests cannot read the cache
        // (0750, the provider's), and one guest's VMM must never hold another
        // guest's installer open.
        assert_eq!(install.path, p.machine_dir.join("install.iso"));
        assert_eq!(install.path, p.install_media);
        assert_eq!(
            p.image,
            Path::new("/srv/banlieue/ch/images/kairos-installer.iso")
        );
        assert!(!plan().empty_os_disk);
        assert!(!plan().guest.disks.iter().any(|d| d.id == DISK_ID_INSTALL));
    }

    /// Decision 4, second half: once detached, the installer never comes
    /// back on a later start.
    #[test]
    fn a_detached_installer_is_left_out_of_every_later_start() {
        let p = deferred_plan().without_install_media();
        let ids: Vec<&str> = p.guest.disks.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, vec![DISK_ID_OS, DISK_ID_SEED]);
        assert!(!p.has_install_media());
        assert!(deferred_plan().has_install_media());
    }

    /// Decision 5: every guest gets a vsock; the provider listens on one
    /// fixed port, whose host end is `<socket>_<port>` in the run directory.
    #[test]
    fn every_guest_gets_a_vsock_and_a_report_socket_in_its_run_directory() {
        let p = plan();
        let run = Path::new("/run/banlieue/ch").join(HOST_UID.to_string());
        assert_eq!(
            p.guest.vsock_socket.as_deref(),
            Some(run.join("vsock.sock").as_path())
        );
        assert_eq!(
            p.report_socket,
            run.join(format!("vsock.sock_{}", crate::report::REPORT_PORT))
        );
    }

    fn tpm_config() -> HostConfig {
        let mut c = config();
        c.tpm = Some(crate::host_config::TpmSection {
            swtpm: "/usr/bin/swtpm".into(),
            swtpm_setup: "/usr/bin/swtpm_setup".into(),
            setup_config: "/etc/banlieue/swtpm/swtpm_setup.conf".into(),
            ek_ca_certificate: "/var/lib/banlieue/swtpm-localca/issuercert.pem".into(),
        });
        c
    }

    fn tpm_plan() -> MachinePlan {
        let mut s = spec();
        s.tpm_enabled = true;
        plan_machine(UID, MACHINE_NAME, "ch-a", &s, &tpm_config(), HOST_UID).unwrap()
    }

    /// A host without `[tpm]` never advertises `vtpm`, so a `tpmEnabled`
    /// machine reaching it is refused, never booted without a TPM.
    #[test]
    fn a_vtpm_on_a_host_without_swtpm_is_refused() {
        let mut s = spec();
        s.tpm_enabled = true;
        assert!(matches!(
            plan_machine(UID, MACHINE_NAME, "ch-a", &s, &config(), HOST_UID),
            Err(PlanError::Unsupported(_))
        ));
    }

    /// ADR-0065 Decisions 1–2: state in the machine directory, the socket in
    /// state and socket keyed by the guest's host uid (what the templates
    /// derive paths from), the EK certificates by machine UID in the
    /// provider's own directory; the VMM is pointed at the socket.
    #[test]
    fn a_vtpm_plan_keys_state_by_guest_uid_and_the_ek_by_machine_uid() {
        let p = tpm_plan();
        let t = p.tpm.as_ref().expect("tpm plan");
        let n = HOST_UID.to_string();
        assert_eq!(t.state_dir, Path::new("/var/lib/banlieue/tpm").join(&n));
        assert_eq!(t.ek_dir, Path::new("/var/lib/banlieue/ek").join(UID));
        assert_eq!(
            t.socket,
            Path::new("/run/banlieue/ch").join(&n).join("swtpm.sock")
        );
        assert_eq!(
            t.setup_env,
            Path::new("/var/lib/banlieue/units").join(format!("swtpm-setup-{n}.env"))
        );
        assert_eq!(t.vmid, format!("{MACHINE_NAME}:{UID}"));
        assert_eq!(p.guest.tpm_socket.as_deref(), Some(t.socket.as_path()));
        assert!(plan().tpm.is_none());
    }

    /// Manufacture gets this machine's `--vmid` (the EK's CN) and EK
    /// directory through its environment file; user, command and sandbox
    /// are the template's.
    #[test]
    fn the_setup_unit_passes_the_vmid_and_ek_directory_in_its_environment() {
        let p = tpm_plan();
        let t = p.tpm.as_ref().unwrap();
        let u = swtpm_setup_unit(&p).unwrap();
        assert_eq!(u.name, t.setup_unit);
        assert!(u.memory_max.is_none());
        let env = u.environment.expect("an environment file");
        assert_eq!(env.path, t.setup_env);
        assert_eq!(
            env.render().unwrap(),
            format!("VMID={MACHINE_NAME}:{UID}\nEK_DIR=/var/lib/banlieue/ek/{UID}\n")
        );
        assert!(swtpm_setup_unit(&plan()).is_none());
    }

    #[test]
    fn the_swtpm_unit_is_the_guest_instance() {
        let p = tpm_plan();
        let u = swtpm_unit(&p).unwrap();
        assert_eq!(u.name, p.tpm.as_ref().unwrap().unit);
        assert!(u.environment.is_none());
        assert!(swtpm_unit(&plan()).is_none());
    }

    // ------------------------------------------------------------------
    // Guest uid allocation (ADR-0063 Decision 3)
    // ------------------------------------------------------------------

    #[test]
    fn allocation_takes_the_lowest_free_uid() {
        let g = config().guests;
        assert_eq!(allocate_host_uid(&BTreeSet::new(), g), Some(2_000_000));
        let used: BTreeSet<u32> = [2_000_000, 2_000_001, 2_000_003].into();
        assert_eq!(allocate_host_uid(&used, g), Some(2_000_002));
    }

    #[test]
    fn allocation_ignores_uids_outside_the_range() {
        let g = config().guests;
        let used: BTreeSet<u32> = [5, 3_000_000].into();
        assert_eq!(allocate_host_uid(&used, g), Some(2_000_000));
    }

    #[test]
    fn allocation_fails_when_the_range_is_full() {
        let mut g = config().guests;
        g.uid_count = 2;
        let used: BTreeSet<u32> = [2_000_000, 2_000_001].into();
        assert_eq!(allocate_host_uid(&used, g), None);
    }

    // ------------------------------------------------------------------
    // The VMM unit (ADR-0063)
    // ------------------------------------------------------------------

    /// The instance, and its memory ceiling set at runtime; the binary,
    /// user, devices and sandbox are the template's (systemd_tests.rs).
    #[test]
    fn the_vmm_unit_is_the_guest_instance_with_a_memory_ceiling() {
        let p = plan();
        let u = vmm_unit(&p);
        assert_eq!(u.name, p.unit);
        assert!(u.environment.is_none());
        let guest = 4096_u64 * 1024 * 1024;
        assert!(u.memory_max.unwrap() > guest);
        assert!(u.memory_max.unwrap() <= guest + 1024 * 1024 * 1024);
    }
}
