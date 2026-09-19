// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Build libvirt domain XML from a [`LibvirtMachineSpec`] (ADR-0050).
//!
//! Pure: a spec and some already-resolved host paths in, a `String` out. No
//! connection, no filesystem, no clock — so every shape this module can
//! produce is a table test, and the only way to find out whether libvirtd
//! agrees is the live test, which is exactly the split the rest of this
//! project uses.
//!
//! Every value is escaped on the way in ([`super::escape::esc`]); see that
//! module for why that is not negotiable.

use banlieue_api::common::Firmware;
use banlieue_api::infrastructure::{
    LibvirtBootSourceKind, LibvirtDiskBus, LibvirtMachineSpec, LibvirtNicSourceKind,
};

use super::escape::{XmlError, esc};

/// Why a domain could not be rendered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainXmlError {
    /// A value could not be represented in XML.
    #[error(transparent)]
    Xml(#[from] XmlError),

    /// `bootSource.kind: installMedia` but no ISO path was supplied. The
    /// caller has not uploaded or located the installer volume yet.
    #[error("boot source is installMedia but no install ISO path was supplied")]
    MissingInstallMedia,

    /// An explicit OVMF vars template was supplied without its code image.
    /// The two are a pair; a varstore template alone says nothing about
    /// which firmware it belongs to.
    #[error("an EFI nvram template was supplied without a loader path")]
    NvramTemplateWithoutLoader,

    /// `extra_disk_paths` did not have one entry per non-OS disk. Rendering
    /// anyway would define a domain whose disk points at nothing.
    #[error("spec has {disks} disk(s) but {paths} extra disk path(s) were supplied")]
    DiskPathCountMismatch {
        /// Number of disks in the spec.
        disks: usize,
        /// Number of extra paths supplied.
        paths: usize,
    },
}

/// Everything the renderer needs that is not in the spec: host paths the
/// provider has already created or resolved.
#[derive(Debug, Clone)]
pub struct DomainXmlInput<'a> {
    /// The machine being rendered.
    pub spec: &'a LibvirtMachineSpec,
    /// `kvm` on a host with KVM, `qemu` for pure emulation.
    pub domain_type: &'a str,
    /// Absolute host path of the OS disk volume.
    pub os_disk_path: &'a str,
    /// Absolute host paths of the non-OS disks, in `spec.disks` order after
    /// the first. Must have exactly `spec.disks.len() - 1` entries.
    pub extra_disk_paths: &'a [String],
    /// Absolute host path of the installer ISO. Required when
    /// `spec.boot_source.kind` is `InstallMedia`, ignored otherwise.
    pub install_iso_path: Option<&'a str>,
    /// Absolute host path of the NoCloud `cidata` ISO, when there is
    /// user-data to deliver.
    pub cidata_iso_path: Option<&'a str>,
    /// Absolute host path of the OVMF code image.
    ///
    /// Normally `None`: `<os firmware='efi'>` makes libvirt select the
    /// firmware itself, which is portable across distributions in a way a
    /// hardcoded path is not. Set it only to pin a specific build.
    pub efi_loader_path: Option<&'a str>,
    /// Absolute host path of the OVMF vars *template*. libvirt copies it per
    /// domain, which is what keeps one domain's UEFI variables out of
    /// another's.
    pub efi_nvram_template_path: Option<&'a str>,
}

/// First target-device letter. `vda` / `sda` — index 0 of the alphabet.
const FIRST_DEVICE_LETTER: u8 = b'a';

/// Render `input` as a libvirt domain document.
///
/// # Errors
/// [`DomainXmlError`] when a required path is missing, the disk and path
/// lists disagree, or a value cannot be represented in XML.
pub fn build_domain_xml(input: &DomainXmlInput<'_>) -> Result<String, DomainXmlError> {
    let spec = input.spec;

    // Guard clauses first: everything below assumes these hold.
    let expected_extra = spec.disks.len().saturating_sub(1);
    if input.extra_disk_paths.len() != expected_extra {
        return Err(DomainXmlError::DiskPathCountMismatch {
            disks: spec.disks.len(),
            paths: input.extra_disk_paths.len(),
        });
    }
    let install_iso = match spec.boot_source.kind {
        LibvirtBootSourceKind::InstallMedia => Some(
            input
                .install_iso_path
                .ok_or(DomainXmlError::MissingInstallMedia)?,
        ),
        LibvirtBootSourceKind::BackingVolume => None,
    };
    let is_efi = matches!(spec.firmware, Firmware::Efi | Firmware::EfiSecure);
    if input.efi_nvram_template_path.is_some() && input.efi_loader_path.is_none() {
        return Err(DomainXmlError::NvramTemplateWithoutLoader);
    }

    let mut out = String::new();
    out.push_str(&format!("<domain type='{}'>", esc(input.domain_type)?));
    out.push_str(&format!("<name>{}</name>", esc(&spec.domain_name)?));
    out.push_str(&format!(
        "<memory unit='MiB'>{m}</memory><currentMemory unit='MiB'>{m}</currentMemory>",
        m = spec.memory_mi_b
    ));
    out.push_str(&format!("<vcpu placement='static'>{}</vcpu>", spec.vcpus));

    out.push_str(&render_os(input, is_efi, install_iso.is_some())?);
    out.push_str(&render_features(spec.firmware == Firmware::EfiSecure));
    // host-passthrough so the guest sees the host's CPU flags. Nothing in
    // banlieue migrates a libvirt domain today (ADR-0036), which is the one
    // thing this would rule out.
    out.push_str("<cpu mode='host-passthrough'/>");
    out.push_str("<on_poweroff>destroy</on_poweroff>");
    out.push_str("<on_reboot>restart</on_reboot>");
    out.push_str("<on_crash>destroy</on_crash>");

    out.push_str("<devices>");
    out.push_str(&render_disks(input)?);
    out.push_str(&render_cdroms(install_iso, input.cidata_iso_path)?);
    out.push_str(&render_interfaces(spec)?);
    if spec.tpm_enabled {
        out.push_str(render_tpm());
    }
    out.push_str(render_agent_channel());
    out.push_str("<serial type='pty'/><console type='pty'/>");
    out.push_str("<memballoon model='virtio'/>");
    out.push_str("</devices>");

    out.push_str("</domain>");
    Ok(out)
}

/// `<os>`: architecture, optional machine type, firmware, and boot order.
fn render_os(
    input: &DomainXmlInput<'_>,
    is_efi: bool,
    has_install_media: bool,
) -> Result<String, DomainXmlError> {
    let mut os = String::from("<os");
    if is_efi {
        // `firmware='efi'` lets libvirt fill in anything the explicit
        // loader/nvram below leave out, on hosts new enough to support it.
        os.push_str(" firmware='efi'");
    }
    os.push('>');

    os.push_str("<type arch='x86_64'");
    if let Some(machine) = &input.spec.machine_type {
        os.push_str(&format!(" machine='{}'", esc(machine)?));
    }
    os.push_str(">hvm</type>");

    // Explicit loader paths are the exception, not the rule. `firmware='efi'`
    // above makes libvirt pick the loader and varstore template from its own
    // firmware descriptors (`/usr/share/qemu/firmware`), which is both the
    // modern idiom and the only way that works across distributions that put
    // OVMF in different places. An explicit path is the escape hatch for a
    // host with no descriptors, or one where a specific build is wanted.
    if is_efi && let Some(loader) = input.efi_loader_path {
        let secure = if input.spec.firmware == Firmware::EfiSecure {
            " secure='yes'"
        } else {
            ""
        };
        os.push_str(&format!(
            "<loader readonly='yes'{secure} type='pflash'>{}</loader>",
            esc(loader)?
        ));
        // Per-domain varstore, copied from the template. Without this every
        // EFI domain on the host shares one set of UEFI variables — and for
        // a Secure Boot domain that is the enrolled key database.
        if let Some(template) = input.efi_nvram_template_path {
            os.push_str(&format!("<nvram template='{}'/>", esc(template)?));
        } else {
            os.push_str("<nvram/>");
        }
    }

    // A Deferred machine must boot its installer first, and keeps doing so
    // until the provider ejects the media once the guest reports installed —
    // libvirt's analogue of ADR-0044. Listing `hd` after it means the very
    // next boot after that ejection comes off the freshly installed disk,
    // with no second redefine.
    if has_install_media {
        os.push_str("<boot dev='cdrom'/>");
    }
    os.push_str("<boot dev='hd'/>");
    os.push_str("</os>");
    Ok(os)
}

/// `<features>`. SMM is a hard requirement for Secure Boot: OVMF's secboot
/// build stores the authenticated variables in SMRAM.
fn render_features(secure_boot: bool) -> String {
    let mut f = String::from("<features><acpi/><apic/>");
    if secure_boot {
        f.push_str("<smm state='on'/>");
    }
    f.push_str("</features>");
    f
}

/// Target device prefix for a bus. SATA and SCSI both present as `sd*`.
fn device_prefix(bus: LibvirtDiskBus) -> &'static str {
    match bus {
        LibvirtDiskBus::Virtio => "vd",
        LibvirtDiskBus::Scsi | LibvirtDiskBus::Sata => "sd",
    }
}

/// Bus name as libvirt spells it in `<target bus='...'>`.
fn bus_name(bus: LibvirtDiskBus) -> &'static str {
    match bus {
        LibvirtDiskBus::Virtio => "virtio",
        LibvirtDiskBus::Scsi => "scsi",
        LibvirtDiskBus::Sata => "sata",
    }
}

/// `<disk>` for the OS disk and each data disk.
fn render_disks(input: &DomainXmlInput<'_>) -> Result<String, DomainXmlError> {
    let mut out = String::new();
    // Per-bus counters: `vda` and `sda` can coexist, `vda` twice cannot.
    let mut virtio_index = 0usize;
    let mut sd_index = 0usize;

    let paths = std::iter::once(input.os_disk_path)
        .chain(input.extra_disk_paths.iter().map(String::as_str));

    for (disk, path) in input.spec.disks.iter().zip(paths) {
        let index = match disk.bus {
            LibvirtDiskBus::Virtio => {
                let i = virtio_index;
                virtio_index += 1;
                i
            }
            LibvirtDiskBus::Scsi | LibvirtDiskBus::Sata => {
                let i = sd_index;
                sd_index += 1;
                i
            }
        };
        out.push_str(&format!(
            "<disk type='file' device='disk'>\
<driver name='qemu' type='qcow2'/>\
<source file='{path}'/>\
<target dev='{prefix}{letter}' bus='{bus}'/>\
</disk>",
            path = esc(path)?,
            prefix = device_prefix(disk.bus),
            letter = device_letter(index),
            bus = bus_name(disk.bus),
        ));
    }
    Ok(out)
}

/// `<disk device='cdrom'>` for the installer and the cloud-init seed.
///
/// Both land on the SATA bus, sharing one `sd*` sequence so the two can never
/// collide on a target device.
fn render_cdroms(
    install_iso: Option<&str>,
    cidata_iso: Option<&str>,
) -> Result<String, DomainXmlError> {
    let mut out = String::new();
    for (index, path) in install_iso.into_iter().chain(cidata_iso).enumerate() {
        out.push_str(&format!(
            "<disk type='file' device='cdrom'>\
<driver name='qemu' type='raw'/>\
<source file='{path}'/>\
<target dev='sd{letter}' bus='sata'/>\
<readonly/>\
</disk>",
            path = esc(path)?,
            letter = device_letter(index),
        ));
    }
    Ok(out)
}

/// `<interface>` for each NIC.
fn render_interfaces(spec: &LibvirtMachineSpec) -> Result<String, DomainXmlError> {
    let mut out = String::new();
    for nic in &spec.network {
        let (kind, attr) = match nic.source.kind {
            LibvirtNicSourceKind::Network => ("network", "network"),
            LibvirtNicSourceKind::Bridge => ("bridge", "bridge"),
        };
        out.push_str(&format!("<interface type='{kind}'>"));
        out.push_str(&format!("<source {attr}='{}'/>", esc(&nic.source.name)?));
        if let Some(mac) = &nic.mac_address {
            out.push_str(&format!("<mac address='{}'/>", esc(mac)?));
        }
        let model = nic.model.as_deref().unwrap_or("virtio");
        out.push_str(&format!("<model type='{}'/>", esc(model)?));
        out.push_str("</interface>");
    }
    Ok(out)
}

/// The emulated TPM (ADR-0039, ADR-0050).
///
/// `tpm-crb` because that is the interface Kairos's `kcrypt` reaches through
/// the guest's `tpm_crb` module, and 2.0 because 1.2 cannot do the sealing
/// this exists for. swtpm keys its state by domain UUID, so every domain gets
/// its own TPM — and that state must be removed at undefine, which is why
/// `domain_undefine` always passes `VIR_DOMAIN_UNDEFINE_TPM`.
fn render_tpm() -> &'static str {
    "<tpm model='tpm-crb'><backend type='emulator' version='2.0'/></tpm>"
}

/// The `qemu-guest-agent` channel.
///
/// Always present, never conditional. It is how `GuestReady` will be
/// satisfied on libvirt (roadmap 70, A2) and one of the three sources for
/// address discovery — and a domain defined without it cannot grow one
/// without a redefine, so the cheap thing is to always have it.
fn render_agent_channel() -> &'static str {
    "<controller type='virtio-serial' index='0'/>\
<channel type='unix'>\
<target type='virtio' name='org.qemu.guest_agent.0'/>\
</channel>"
}

/// `0 -> 'a'`, `1 -> 'b'`, … Wraps back to `a` past `z`, which is 26 disks on
/// one bus — far past the 32-disk schema cap being reachable in practice on
/// a single bus, and libvirt rejects a duplicate `dev` rather than silently
/// accepting one.
fn device_letter(index: usize) -> char {
    char::from(FIRST_DEVICE_LETTER + u8::try_from(index % 26).unwrap_or(0))
}

#[cfg(test)]
#[path = "domain_tests.rs"]
mod domain_tests;
