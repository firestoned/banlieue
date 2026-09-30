// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Rendering a `ProxmoxMachineSpec` into Proxmox VM config keys.

use banlieue_api::common::Firmware;
use banlieue_api::infrastructure::{ProxmoxMachineSpec, ProxmoxNicSpec};
use banlieue_proxmox::{Params, VmConfig};

/// Config key of the OS disk.
///
/// **Assumption:** the template's boot disk is `scsi0`. Proxmox clones keep
/// the template's disk keys, and banlieue's template recipe
/// (`scripts/bootstrap-proxmox-host.sh`) attaches the imported cloud image as
/// `scsi0` on a virtio-scsi controller. A template that boots from another
/// key (`virtio0`, `sata0`) is not supported: resizing it would fail loudly
/// with "scsi0 is absent" rather than grow the wrong disk.
pub const OS_DISK_KEY: &str = "scsi0";
/// Config key of the seed CD-ROM (ADR-0075 Decision 5).
pub const SEED_DRIVE_KEY: &str = "ide2";
/// Data disks start at `scsi1`; `scsi0` is the OS disk.
const FIRST_DATA_DISK_INDEX: usize = 1;
const EFI_DISK_KEY: &str = "efidisk0";
const TPM_STATE_KEY: &str = "tpmstate0";
/// SCSI controller that gives each disk its own I/O thread.
const SCSI_HW_IOTHREAD: &str = "virtio-scsi-single";
/// Proxmox allocates a 1 GiB volume for an EFI or TPM state disk and ignores
/// the size for both; `1` is the documented placeholder.
const STATE_DISK_SIZE: u32 = 1;
const MIB_PER_GIB: u64 = 1024;
const GIB_PER_TIB: u64 = 1024;
const KIB_PER_GIB: u64 = 1024 * 1024;
const BYTES_PER_GIB: u64 = KIB_PER_GIB * 1024;

/// Prefix of the ownership line in a VM's description.
const OWNERSHIP_PREFIX: &str = "banlieue-machine-uid=";
/// Config key holding the VM's description.
const DESCRIPTION_KEY: &str = "description";

/// The line that marks a VM as belonging to the machine with \`uid\`.
///
/// Written into the VM's description **atomically with the clone**, so a VM
/// is never visible without it (ADR-0075 Decision 4). A VM *name* is not an
/// ownership test: names are unique only per namespace, and Proxmox allows
/// duplicates and admin-created VMs, so trusting a name would adopt, configure
/// and later destroy somebody else's guest.
#[must_use]
pub fn ownership_marker(uid: &str) -> String {
    format!("{OWNERSHIP_PREFIX}{uid}")
}

/// Whether \`config\` belongs to the machine with \`uid\`.
///
/// True when some line of the description, trimmed, **equals** the marker:
/// not a substring match (so a uid that prefixes another does not collide) and
/// not the whole description (so an admin's notes on other lines do not orphan
/// the VM). Deleting the marker line does orphan it, by design.
#[must_use]
pub fn is_ours(config: &VmConfig, uid: &str) -> bool {
    let marker = ownership_marker(uid);
    config
        .get(DESCRIPTION_KEY)
        .is_some_and(|d| d.lines().any(|l| l.trim() == marker))
}

/// File name of a machine's seed ISO.
#[must_use]
pub fn seed_filename(uid: &str) -> String {
    format!("banlieue-{uid}.iso")
}

/// Volume id of a machine's seed ISO on `iso_storage`.
#[must_use]
pub fn seed_volid(iso_storage: &str, uid: &str) -> String {
    format!("{iso_storage}:iso/{}", seed_filename(uid))
}

/// The config keys to write for `spec`.
///
/// Additive against `existing`: an EFI or TPM state disk and each data disk
/// are only requested when absent, because writing the key again would replace
/// the disk and orphan its volume. `nics` should come from
/// [`crate::network::effective_nics`].
#[must_use]
pub fn desired_config(
    spec: &ProxmoxMachineSpec,
    nics: &[ProxmoxNicSpec],
    existing: &VmConfig,
    seed_volid: Option<&str>,
) -> Params {
    let mut p = Params::new()
        .set("cores", spec.cores)
        .set("sockets", spec.sockets)
        .set("memory", spec.memory_mi_b)
        .set_opt("cpu", spec.cpu_type.as_deref())
        .set("bios", spec.bios())
        .flag("agent", true);

    if spec.firmware != Firmware::Bios && existing.get(EFI_DISK_KEY).is_none() {
        p = p.set(
            EFI_DISK_KEY,
            format!(
                "{}:{STATE_DISK_SIZE},efitype=4m,pre-enrolled-keys={}",
                spec.storage,
                u8::from(spec.secure_boot())
            ),
        );
    }
    if spec.tpm_enabled && existing.get(TPM_STATE_KEY).is_none() {
        p = p.set(
            TPM_STATE_KEY,
            format!("{}:{STATE_DISK_SIZE},version=v2.0", spec.storage),
        );
    }
    for (i, nic) in nics.iter().enumerate() {
        p = p.set(&format!("net{i}"), nic.config_value());
    }
    for (i, disk) in spec.data_disks.iter().enumerate() {
        let key = format!("scsi{}", FIRST_DATA_DISK_INDEX + i);
        if existing.get(&key).is_some() {
            continue;
        }
        let storage = disk.storage.as_deref().unwrap_or(&spec.storage);
        let mut value = format!("{storage}:{}", disk.size_gi_b);
        if disk.iothread {
            value.push_str(",iothread=1");
        }
        if disk.discard {
            value.push_str(",discard=on");
        }
        if disk.ssd {
            value.push_str(",ssd=1");
        }
        p = p.set(&key, value);
    }
    if spec.data_disks.iter().any(|d| d.iothread) {
        p = p.set("scsihw", SCSI_HW_IOTHREAD);
    }
    if let Some(volid) = seed_volid {
        p = p.set(SEED_DRIVE_KEY, format!("{volid},media=cdrom"));
    }
    p
}

/// The OS disk's current size in whole GiB (rounded up), read from its
/// `size=` option. `None` when the disk or its size is absent or unparseable.
#[must_use]
pub fn os_disk_size_gib(config: &VmConfig) -> Option<u64> {
    let value = config.get(OS_DISK_KEY)?;
    let size = value.split(',').find_map(|kv| kv.strip_prefix("size="))?;
    let split = size
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(size.len());
    let (digits, unit) = size.split_at(split);
    let n: u64 = digits.parse().ok()?;
    match unit {
        "" => Some(n.div_ceil(BYTES_PER_GIB)),
        "K" => Some(n.div_ceil(KIB_PER_GIB)),
        "M" => Some(n.div_ceil(MIB_PER_GIB)),
        "G" => Some(n),
        "T" => Some(n * GIB_PER_TIB),
        _ => None,
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
