// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `xml/domain.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::common::{Firmware, IpamSpec, LocalObjectReference, PowerState};
    use banlieue_api::infrastructure::{
        LibvirtBootSource, LibvirtBootSourceKind, LibvirtDiskBus, LibvirtDiskSpec,
        LibvirtMachineSpec, LibvirtNicSource, LibvirtNicSourceKind, LibvirtNicSpec,
    };

    fn spec_with(
        kind: LibvirtBootSourceKind,
        firmware: Firmware,
        tpm_enabled: bool,
    ) -> LibvirtMachineSpec {
        LibvirtMachineSpec {
            provider_id: None,
            failure_domain: None,
            provider_ref: LocalObjectReference {
                name: "libvirt-host-a".to_string(),
            },
            pool: "default".to_string(),
            domain_name: "sandbox-01".to_string(),
            boot_source: LibvirtBootSource {
                kind,
                volume: "source.img".to_string(),
            },
            vcpus: 4,
            memory_mi_b: 8192,
            firmware,
            machine_type: None,
            tpm_enabled,
            disks: vec![LibvirtDiskSpec {
                name: "os".to_string(),
                size_gi_b: 40,
                bus: LibvirtDiskBus::Virtio,
            }],
            network: vec![LibvirtNicSpec {
                name: "eth0".to_string(),
                source: LibvirtNicSource {
                    kind: LibvirtNicSourceKind::Network,
                    name: "default".to_string(),
                },
                model: None,
                mac_address: None,
                ipam: IpamSpec::default(),
            }],
            user_data: None,
            desired_power_state: PowerState::PoweredOn,
        }
    }

    fn bios_spec() -> LibvirtMachineSpec {
        spec_with(LibvirtBootSourceKind::BackingVolume, Firmware::Bios, false)
    }

    fn input<'a>(spec: &'a LibvirtMachineSpec) -> DomainXmlInput<'a> {
        DomainXmlInput {
            spec,
            domain_type: "kvm",
            os_disk_path: "/var/lib/libvirt/images/sandbox-01-os.qcow2",
            extra_disk_paths: &[],
            install_iso_path: None,
            cidata_iso_path: None,
            efi_loader_path: None,
            efi_nvram_template_path: None,
        }
    }

    // ------------------------------------------------------------------
    // Shape
    // ------------------------------------------------------------------

    #[test]
    fn renders_the_basic_domain_shape() {
        let spec = bios_spec();
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(xml.starts_with("<domain type='kvm'>"), "{xml}");
        assert!(xml.ends_with("</domain>"), "{xml}");
        assert!(xml.contains("<name>sandbox-01</name>"), "{xml}");
        assert!(xml.contains("<memory unit='MiB'>8192</memory>"), "{xml}");
        assert!(
            xml.contains("<currentMemory unit='MiB'>8192</currentMemory>"),
            "{xml}"
        );
        assert!(xml.contains("<vcpu placement='static'>4</vcpu>"), "{xml}");
    }

    /// The guest agent channel is not optional decoration: `GuestReady`
    /// (roadmap 70 A2) and address discovery both depend on it, and a domain
    /// defined without the channel cannot grow one without a redefine.
    #[test]
    fn always_includes_the_qemu_guest_agent_channel() {
        let spec = bios_spec();
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(xml.contains("<controller type='virtio-serial'"), "{xml}");
        assert!(
            xml.contains("name='org.qemu.guest_agent.0'"),
            "guest agent channel missing: {xml}"
        );
    }

    #[test]
    fn honours_an_explicit_machine_type() {
        let mut spec = bios_spec();
        spec.machine_type = Some("q35".to_string());
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(xml.contains("machine='q35'"), "{xml}");
    }

    #[test]
    fn omits_the_machine_attribute_when_unset() {
        let spec = bios_spec();
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(!xml.contains("machine='"), "{xml}");
    }

    // ------------------------------------------------------------------
    // Disks
    // ------------------------------------------------------------------

    #[test]
    fn renders_the_os_disk_on_its_bus() {
        let spec = bios_spec();
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(
            xml.contains("<source file='/var/lib/libvirt/images/sandbox-01-os.qcow2'/>"),
            "{xml}"
        );
        assert!(xml.contains("<target dev='vda' bus='virtio'/>"), "{xml}");
    }

    /// Target device names are per-bus and sequential. Two disks sharing a
    /// `dev` is a define-time error from libvirt, so the naming is pinned.
    #[test]
    fn assigns_sequential_target_devices() {
        let mut spec = bios_spec();
        spec.disks.push(LibvirtDiskSpec {
            name: "data".to_string(),
            size_gi_b: 100,
            bus: LibvirtDiskBus::Virtio,
        });
        let mut i = input(&spec);
        let extra = vec!["/var/lib/libvirt/images/sandbox-01-data.qcow2".to_string()];
        i.extra_disk_paths = &extra;
        let xml = build_domain_xml(&i).unwrap();
        assert!(xml.contains("dev='vda'"), "{xml}");
        assert!(xml.contains("dev='vdb'"), "{xml}");
    }

    #[test]
    fn maps_each_bus_to_its_device_prefix() {
        for (bus, dev) in [
            (LibvirtDiskBus::Virtio, "vda"),
            (LibvirtDiskBus::Scsi, "sda"),
            (LibvirtDiskBus::Sata, "sda"),
        ] {
            let mut spec = bios_spec();
            spec.disks[0].bus = bus;
            let xml = build_domain_xml(&input(&spec)).unwrap();
            assert!(xml.contains(&format!("dev='{dev}'")), "bus {bus:?}: {xml}");
        }
    }

    /// A disk list and a path list of different lengths means the caller
    /// created the wrong number of volumes. Rendering anyway would define a
    /// domain with a disk pointing at nothing.
    #[test]
    fn rejects_a_path_list_that_does_not_match_the_disk_list() {
        let spec = bios_spec(); // one disk, so zero extra paths expected
        let mut i = input(&spec);
        let extra = vec!["/unexpected.qcow2".to_string()];
        i.extra_disk_paths = &extra;
        assert!(matches!(
            build_domain_xml(&i),
            Err(DomainXmlError::DiskPathCountMismatch { .. })
        ));
    }

    // ------------------------------------------------------------------
    // Boot source
    // ------------------------------------------------------------------

    /// An `Immediate` machine has no installer ISO and boots straight off
    /// the disk.
    #[test]
    fn backing_volume_boots_from_disk_with_no_cdrom() {
        let spec = bios_spec();
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(xml.contains("<boot dev='hd'/>"), "{xml}");
        assert!(!xml.contains("device='cdrom'"), "{xml}");
    }

    /// A `Deferred` machine boots the installer first. It keeps booting the
    /// installer until the provider ejects the media once the guest reports
    /// installed — which is libvirt's analogue of ADR-0044, and the reason
    /// the CD-ROM is listed before the disk rather than after.
    #[test]
    fn install_media_boots_cdrom_before_disk() {
        let spec = spec_with(LibvirtBootSourceKind::InstallMedia, Firmware::Bios, false);
        let mut i = input(&spec);
        i.install_iso_path = Some("/var/lib/libvirt/images/installer.iso");
        let xml = build_domain_xml(&i).unwrap();

        let cdrom = xml.find("<boot dev='cdrom'/>").expect("cdrom boot entry");
        let hd = xml.find("<boot dev='hd'/>").expect("hd boot entry");
        assert!(cdrom < hd, "cdrom must precede hd: {xml}");
        assert!(xml.contains("device='cdrom'"), "{xml}");
        assert!(
            xml.contains("<source file='/var/lib/libvirt/images/installer.iso'/>"),
            "{xml}"
        );
    }

    #[test]
    fn install_media_without_an_iso_path_is_an_error() {
        let spec = spec_with(LibvirtBootSourceKind::InstallMedia, Firmware::Bios, false);
        assert!(matches!(
            build_domain_xml(&input(&spec)),
            Err(DomainXmlError::MissingInstallMedia)
        ));
    }

    // ------------------------------------------------------------------
    // Firmware
    // ------------------------------------------------------------------

    #[test]
    fn bios_needs_no_loader() {
        let spec = bios_spec();
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(!xml.contains("<loader"), "{xml}");
        assert!(!xml.contains("<nvram"), "{xml}");
    }

    /// An EFI domain needs a loader *and* a per-domain varstore. Omitting
    /// the `<nvram>` makes libvirt share one varstore, and omitting the
    /// `readonly`/`pflash` attributes makes OVMF fail to start.
    #[test]
    fn efi_renders_a_pflash_loader_and_its_own_nvram() {
        let spec = spec_with(LibvirtBootSourceKind::BackingVolume, Firmware::Efi, false);
        let mut i = input(&spec);
        i.efi_loader_path = Some("/usr/share/OVMF/OVMF_CODE.fd");
        i.efi_nvram_template_path = Some("/usr/share/OVMF/OVMF_VARS.fd");
        let xml = build_domain_xml(&i).unwrap();
        assert!(xml.contains("readonly='yes'"), "{xml}");
        assert!(xml.contains("type='pflash'"), "{xml}");
        assert!(xml.contains("/usr/share/OVMF/OVMF_CODE.fd"), "{xml}");
        assert!(xml.contains("<nvram template="), "{xml}");
    }

    /// The normal case: no explicit paths, and libvirt picks the firmware
    /// from its own descriptors. Hardcoding an OVMF path is not portable —
    /// Debian, Fedora and Arch all put it somewhere different — so the
    /// autoselected form is what a machine gets unless someone pins one.
    #[test]
    fn efi_without_an_explicit_loader_uses_libvirt_autoselection() {
        let spec = spec_with(LibvirtBootSourceKind::BackingVolume, Firmware::Efi, false);
        let xml = build_domain_xml(&input(&spec)).expect("EFI needs no explicit loader");
        assert!(xml.contains("<os firmware='efi'>"), "{xml}");
        assert!(!xml.contains("<loader"), "{xml}");
    }

    /// A varstore template without its code image is a half-specified pair,
    /// and libvirt would reject it far from the cause.
    #[test]
    fn an_nvram_template_without_a_loader_is_an_error() {
        let spec = spec_with(LibvirtBootSourceKind::BackingVolume, Firmware::Efi, false);
        let mut i = input(&spec);
        i.efi_nvram_template_path = Some("/usr/share/OVMF/OVMF_VARS.fd");
        assert!(matches!(
            build_domain_xml(&i),
            Err(DomainXmlError::NvramTemplateWithoutLoader)
        ));
    }

    #[test]
    fn efi_secure_requests_secure_boot() {
        let spec = spec_with(
            LibvirtBootSourceKind::BackingVolume,
            Firmware::EfiSecure,
            false,
        );
        let mut i = input(&spec);
        i.efi_loader_path = Some("/usr/share/OVMF/OVMF_CODE.secboot.fd");
        i.efi_nvram_template_path = Some("/usr/share/OVMF/OVMF_VARS.secboot.fd");
        let xml = build_domain_xml(&i).unwrap();
        assert!(xml.contains("secure='yes'"), "{xml}");
        assert!(xml.contains("<smm state='on'/>"), "SMM is required: {xml}");
    }

    // ------------------------------------------------------------------
    // TPM
    // ------------------------------------------------------------------

    #[test]
    fn no_tpm_device_unless_requested() {
        let spec = bios_spec();
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(!xml.contains("<tpm"), "{xml}");
    }

    /// swtpm 2.0, keyed by domain UUID so every domain gets its own. The
    /// model must be `tpm-crb`: Kairos's `kcrypt` reaches it through the
    /// `tpm_crb` kernel module.
    #[test]
    fn tpm_renders_an_emulated_crb_device() {
        let spec = spec_with(LibvirtBootSourceKind::InstallMedia, Firmware::Efi, true);
        let mut i = input(&spec);
        i.install_iso_path = Some("/installer.iso");
        i.efi_loader_path = Some("/usr/share/OVMF/OVMF_CODE.fd");
        i.efi_nvram_template_path = Some("/usr/share/OVMF/OVMF_VARS.fd");
        let xml = build_domain_xml(&i).unwrap();
        assert!(xml.contains("<tpm model='tpm-crb'>"), "{xml}");
        assert!(
            xml.contains("<backend type='emulator' version='2.0'/>"),
            "{xml}"
        );
    }

    // ------------------------------------------------------------------
    // Network
    // ------------------------------------------------------------------

    #[test]
    fn renders_a_libvirt_network_interface() {
        let spec = bios_spec();
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(xml.contains("<interface type='network'>"), "{xml}");
        assert!(xml.contains("<source network='default'/>"), "{xml}");
        assert!(xml.contains("<model type='virtio'/>"), "{xml}");
    }

    #[test]
    fn renders_a_bridge_interface() {
        let mut spec = bios_spec();
        spec.network[0].source = LibvirtNicSource {
            kind: LibvirtNicSourceKind::Bridge,
            name: "br0".to_string(),
        };
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(xml.contains("<interface type='bridge'>"), "{xml}");
        assert!(xml.contains("<source bridge='br0'/>"), "{xml}");
    }

    #[test]
    fn renders_an_explicit_mac_address() {
        let mut spec = bios_spec();
        spec.network[0].mac_address = Some("52:54:00:aa:bb:cc".to_string());
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(xml.contains("<mac address='52:54:00:aa:bb:cc'/>"), "{xml}");
    }

    #[test]
    fn omits_mac_when_libvirt_should_generate_one() {
        let spec = bios_spec();
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(!xml.contains("<mac"), "{xml}");
    }

    // ------------------------------------------------------------------
    // cloud-init
    // ------------------------------------------------------------------

    #[test]
    fn attaches_the_cidata_iso_when_present() {
        let spec = bios_spec();
        let mut i = input(&spec);
        i.cidata_iso_path = Some("/var/lib/libvirt/images/sandbox-01-cidata.iso");
        let xml = build_domain_xml(&i).unwrap();
        assert!(
            xml.contains("<source file='/var/lib/libvirt/images/sandbox-01-cidata.iso'/>"),
            "{xml}"
        );
    }

    /// An installer ISO and a cidata ISO are two CD-ROMs on one domain, so
    /// they must not collide on a target device.
    #[test]
    fn install_and_cidata_isos_get_distinct_targets() {
        let spec = spec_with(LibvirtBootSourceKind::InstallMedia, Firmware::Bios, false);
        let mut i = input(&spec);
        i.install_iso_path = Some("/installer.iso");
        i.cidata_iso_path = Some("/cidata.iso");
        let xml = build_domain_xml(&i).unwrap();
        assert!(xml.contains("dev='sda'"), "{xml}");
        assert!(xml.contains("dev='sdb'"), "{xml}");
    }

    // ------------------------------------------------------------------
    // Escaping — the reason this module exists
    // ------------------------------------------------------------------

    /// Every string that reaches the document goes through `esc`. A domain
    /// name is the most obviously user-influenced of them.
    #[test]
    fn escapes_a_hostile_domain_name() {
        let mut spec = bios_spec();
        spec.domain_name = "evil'/><devices><disk/></devices><x a='".to_string();
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(!xml.contains("<devices><disk/></devices>"), "{xml}");
        assert!(xml.contains("&lt;devices&gt;"), "{xml}");
    }

    #[test]
    fn escapes_a_hostile_bridge_name() {
        let mut spec = bios_spec();
        spec.network[0].source = LibvirtNicSource {
            kind: LibvirtNicSourceKind::Bridge,
            name: "br0'/><disk type='file'/><source bridge='".to_string(),
        };
        let xml = build_domain_xml(&input(&spec)).unwrap();
        assert!(!xml.contains("<disk type='file'/>"), "{xml}");
    }

    #[test]
    fn escapes_a_hostile_disk_path() {
        let spec = bios_spec();
        let mut i = input(&spec);
        i.os_disk_path = "/img/x.qcow2'/><disk><source file='/etc/shadow";
        let xml = build_domain_xml(&i).unwrap();

        // The payload's markup is escaped, so it is inert text...
        assert!(xml.contains("&apos;/&gt;&lt;disk&gt;"), "{xml}");
        // ...and no second <disk> element was created. Counting elements is
        // the assertion that means something: a substring check can match
        // the legitimate element's own terminator and pass for the wrong
        // reason.
        assert_eq!(xml.matches("<disk ").count(), 1, "{xml}");
        assert_eq!(xml.matches("<source file=").count(), 1, "{xml}");
    }

    /// An unrepresentable character propagates as an error rather than
    /// producing a document libvirtd will reject with something opaque.
    #[test]
    fn an_illegal_character_surfaces_as_an_xml_error() {
        let mut spec = bios_spec();
        spec.domain_name = "bad\u{0}name".to_string();
        assert!(matches!(
            build_domain_xml(&input(&spec)),
            Err(DomainXmlError::Xml(XmlError::IllegalChar { .. }))
        ));
    }
}
