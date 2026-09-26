// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Planning: from a `CloudHypervisorMachine` and the host config to every
//! name, path and VMM setting the provider acts on. Pure, no I/O.
//!
//! This is where cluster input meets the host, so it is also where cluster
//! input is checked:
//!
//! - the machine UID names units, taps and directories, so it must be exactly
//!   a UUID;
//! - the image name is joined onto a host path, so it must not be able to
//!   leave the image cache;
//! - classes resolve only through the host config (ADR-0062 Decision 4);
//! - the guest's host uid must fall inside the configured range
//!   (ADR-0063 Decision 3).
//!
//! A vTPM (ADR-0065 Decisions 1–2) is planned as two more units: a one-shot
//! `swtpm_setup` that manufactures the TPM and its EK certificate, and the
//! guest's `swtpm`. `Deferred` install (Decision 3) is an empty OS disk
//! with the installer attached second; every guest also gets a vsock whose
//! host end the provider listens on for the guest's `phase` report
//! (Decision 5).

use crate::host_config::{GuestsSection, HostConfig};
use crate::systemd::{EnvFile, UnitStart};
use banlieue_api::infrastructure::{
    ChBootSourceKind, CloudHypervisorMachineSpec, cloud_hypervisor_provider_id,
};
use banlieue_cloud_hypervisor::{GuestPlan, PlannedDisk, PlannedNic};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Bytes in one GiB.
const BYTES_PER_GIB: u64 = 1024 * 1024 * 1024;
/// Length of a canonical UUID string.
const UUID_LEN: usize = 36;
/// Positions of the dashes in a canonical UUID.
const UUID_DASHES: [usize; 4] = [8, 13, 18, 23];
/// The image cache under each storage class directory (ADR-0064 Decision 4).
pub const IMAGES_DIR: &str = "images";
/// Template of every VMM unit; the instance is the guest's host uid
/// (`banlieue-ch@<uid>.service`, installed by the bootstrap).
pub const VMM_TEMPLATE: &str = "banlieue-ch";
/// Prefix of every tap device.
const TAP_PREFIX: &str = "bch";
/// UID hex digits used in a tap name: `bch` + 10 + one index digit = 14,
/// under the 15-character interface limit.
const TAP_UID_DIGITS: usize = 10;
/// Most NICs a machine can have: one hex digit of tap index.
pub const MAX_NICS: usize = 16;
/// First octet of a derived MAC: locally administered (bit 1), unicast
/// (bit 0 clear).
const MAC_LOCAL_UNICAST: u8 = 0x02;
/// UID hex digits, after those the tap name uses, that seed a derived MAC.
const MAC_UID_DIGITS: std::ops::Range<usize> = 10..18;
/// Longest image name accepted.
const MAX_IMAGE_NAME: usize = 128;
/// Octets in a MAC address.
const MAC_OCTETS: usize = 6;
/// Memory the VMM process itself needs beyond guest RAM, for `MemoryMax`.
const VMM_MEMORY_OVERHEAD_BYTES: u64 = 512 * 1024 * 1024;
/// Bytes in one MiB.
const BYTES_PER_MIB: u64 = 1024 * 1024;

/// Template of a guest's swtpm; instance: its host uid.
pub const SWTPM_TEMPLATE: &str = "banlieue-swtpm";
/// Template of a guest's one-shot TPM manufacture; instance: its host uid.
pub const SWTPM_SETUP_TEMPLATE: &str = "banlieue-swtpm-setup";
/// TPM state, `<state_root>/tpm/<host uid>/`: a template can only name
/// paths it derives from its instance.
pub const TPM_STATE_ROOT: &str = "tpm";
/// Environment files for the templates that read one,
/// `<state_root>/units/`, provider-only.
pub const UNITS_ENV_DIR: &str = "units";
/// The manufacture template's `EnvironmentFile=` is
/// `<state_root>/units/swtpm-setup-%i.env`.
const SWTPM_SETUP_ENV_PREFIX: &str = "swtpm-setup-";

/// `<template>@<instance>.service`.
#[must_use]
pub fn instance_unit(template: &str, instance: &str) -> String {
    format!("{template}@{instance}.service")
}
/// EK certificates `swtpm_setup` writes, under the provider's state root:
/// `<state_root>/ek/<uid>/`. Not in the machine directory, which the guest
/// owns and could fill with a certificate of its own making.
pub const TPM_EK_ROOT: &str = "ek";
/// swtpm's control socket, under the run directory.
const SWTPM_SOCKET: &str = "swtpm.sock";

/// Device id of the OS disk; stable because `vm.remove-device` takes ids.
pub const DISK_ID_OS: &str = "os";
/// Device id of the NoCloud seed disk.
pub const DISK_ID_SEED: &str = "seed";
/// Device id of the installer: what `vm.remove-device` ejects (ADR-0065
/// Decision 4).
pub const DISK_ID_INSTALL: &str = "install";
/// The installer's per-machine copy, in the machine directory.
const INSTALL_MEDIA_FILE: &str = "install.iso";
/// The guest's vsock, host end, in the run directory.
const VSOCK_SOCKET: &str = "vsock.sock";

/// Why a machine could not be planned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// The machine UID is not a canonical lowercase UUID.
    #[error("machine UID {0:?} is not a canonical UUID")]
    InvalidUid(String),
    /// The image name could leave the image cache or is otherwise unusable.
    #[error("image name {0:?} is not a plain file name")]
    InvalidImage(String),
    /// The machine names a storage class this host does not declare.
    #[error("storage class {0:?} is not declared on this host")]
    UnknownStorageClass(String),
    /// A NIC names a network class this host does not declare.
    #[error("network class {0:?} is not declared on this host")]
    UnknownNetworkClass(String),
    /// The host uid is outside the configured guest range.
    #[error("host uid {0} is outside the guest uid range")]
    HostUidOutOfRange(u32),
    /// An explicit MAC address does not parse.
    #[error("MAC address {0:?} is not six hex octets")]
    InvalidMac(String),
    /// The machine asks for something this provider does not do yet.
    #[error("not supported yet: {0}")]
    Unsupported(String),
}

/// One NIC's host-side plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NicPlan {
    /// The NIC's name from the spec.
    pub name: String,
    /// Tap device the provider creates.
    pub tap: String,
    /// Bridge the tap joins, resolved from the host config.
    pub bridge: String,
    /// Guest MAC.
    pub mac: String,
}

/// A machine's vTPM (ADR-0065 Decisions 1–2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TpmPlan {
    /// swtpm state, in the machine directory: one owner, deleted with it.
    pub state_dir: PathBuf,
    /// Where `swtpm_setup` writes the EK certificates (DER), in the
    /// provider's own state root. Their presence is also the record that
    /// this machine's TPM was manufactured: it is never manufactured twice.
    pub ek_dir: PathBuf,
    /// swtpm's control socket, which the VMM is given.
    pub socket: PathBuf,
    /// `banlieue-swtpm@<host uid>.service`.
    pub unit: String,
    /// `banlieue-swtpm-setup@<host uid>.service`.
    pub setup_unit: String,
    /// The environment file the manufacture template reads.
    pub setup_env: PathBuf,
    /// `<machine-name>:<machine-uid>`: the EK certificate's subject CN.
    pub vmid: String,
}

/// Everything the provider does for one machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachinePlan {
    /// The machine's UID, validated.
    pub uid: String,
    /// The unprivileged uid the guest's units run as.
    pub host_uid: u32,
    /// The VMM's unit: `banlieue-ch@<host uid>.service`.
    pub unit: String,
    /// Per-machine directory under the storage class.
    pub machine_dir: PathBuf,
    /// Per-machine run directory, for sockets.
    pub run_dir: PathBuf,
    /// The VMM API socket.
    pub api_socket: PathBuf,
    /// The installed image in the host's cache.
    pub image: PathBuf,
    /// The machine's own read-only copy of the installer (`Deferred`):
    /// what the VMM attaches, since guests cannot read the cache. Unused
    /// for an `Immediate` machine.
    pub install_media: PathBuf,
    /// The machine's OS disk.
    pub os_disk: PathBuf,
    /// Size to grow the OS disk to before first boot.
    pub os_disk_bytes: u64,
    /// The NoCloud seed.
    pub seed: PathBuf,
    /// Serial console log.
    pub serial_log: PathBuf,
    /// Host-side NIC plans, in spec order.
    pub nics: Vec<NicPlan>,
    /// `spec.providerID`.
    pub provider_id: String,
    /// The VMM configuration.
    pub guest: GuestPlan,
    /// The vTPM, when `tpmEnabled`.
    pub tpm: Option<TpmPlan>,
    /// Create the OS disk empty (`Deferred`) rather than from the image.
    pub empty_os_disk: bool,
    /// Where the provider listens for the guest's report: the vsock socket
    /// with `_<port>` appended, which is where a guest's connection to that
    /// host port lands.
    pub report_socket: PathBuf,
}

impl MachinePlan {
    /// Whether the installer is still in the guest's configuration.
    #[must_use]
    pub fn has_install_media(&self) -> bool {
        self.guest.disks.iter().any(|d| d.id == DISK_ID_INSTALL)
    }

    /// The same plan without the installer: once ejected it stays out of
    /// every later start (ADR-0065 Decision 4).
    #[must_use]
    pub fn without_install_media(mut self) -> Self {
        self.guest.disks.retain(|d| d.id != DISK_ID_INSTALL);
        self
    }

    /// Whether the machine has user-data, and so a seed disk.
    #[must_use]
    pub fn needs_seed(&self) -> bool {
        self.guest.disks.iter().any(|d| d.id == DISK_ID_SEED)
    }
}

/// Plan `spec` for the machine with `uid` on this host.
///
/// # Errors
/// A [`PlanError`] naming the first thing that cannot be planned.
pub fn plan_machine(
    uid: &str,
    machine_name: &str,
    provider_name: &str,
    spec: &CloudHypervisorMachineSpec,
    config: &HostConfig,
    host_uid: u32,
) -> Result<MachinePlan, PlanError> {
    require_uuid(uid)?;
    if spec.tpm_enabled && config.tpm.is_none() {
        return Err(PlanError::Unsupported(
            "vTPM: this host has no [tpm] section (ADR-0065)".into(),
        ));
    }
    if spec.nics.len() > MAX_NICS {
        return Err(PlanError::Unsupported(format!("more than {MAX_NICS} NICs")));
    }
    if !config.guests.contains(host_uid) {
        return Err(PlanError::HostUidOutOfRange(host_uid));
    }
    require_image_name(&spec.boot_source.image)?;

    let storage = config
        .storage_path(&spec.storage_class)
        .ok_or_else(|| PlanError::UnknownStorageClass(spec.storage_class.clone()))?;
    let machine_dir = storage.join(uid);
    // Keyed by the guest's host uid, not the machine UID: the templates
    // derive their paths from their instance, which is that uid.
    let host_uid_str = host_uid.to_string();
    let run_dir = config.paths.run_root.join(&host_uid_str);
    let image = storage.join(IMAGES_DIR).join(&spec.boot_source.image);
    let os_disk = machine_dir.join("os.raw");
    let install_media = machine_dir.join(INSTALL_MEDIA_FILE);
    let seed = machine_dir.join("seed.iso");
    let serial_log = machine_dir.join("serial.log");
    let api_socket = run_dir.join("api.sock");
    let vsock_socket = run_dir.join(VSOCK_SOCKET);
    let report_socket = run_dir.join(format!("{VSOCK_SOCKET}_{}", crate::report::REPORT_PORT));
    let tpm = spec.tpm_enabled.then(|| TpmPlan {
        state_dir: config
            .paths
            .state_root
            .join(TPM_STATE_ROOT)
            .join(&host_uid_str),
        ek_dir: config.paths.state_root.join(TPM_EK_ROOT).join(uid),
        socket: run_dir.join(SWTPM_SOCKET),
        unit: instance_unit(SWTPM_TEMPLATE, &host_uid_str),
        setup_unit: instance_unit(SWTPM_SETUP_TEMPLATE, &host_uid_str),
        setup_env: config
            .paths
            .state_root
            .join(UNITS_ENV_DIR)
            .join(format!("{SWTPM_SETUP_ENV_PREFIX}{host_uid_str}.env")),
        vmid: banlieue_provider_sdk::ek::expected_ek_cn(machine_name, uid),
    });

    let hex: String = uid.chars().filter(|c| *c != '-').collect();
    let mut nics = Vec::with_capacity(spec.nics.len());
    for (index, nic) in spec.nics.iter().enumerate() {
        let bridge = config
            .bridge(&nic.network_class)
            .ok_or_else(|| PlanError::UnknownNetworkClass(nic.network_class.clone()))?;
        let mac = match &nic.mac_address {
            Some(m) => require_mac(m)?,
            None => derived_mac(&hex, index),
        };
        nics.push(NicPlan {
            name: nic.name.clone(),
            tap: format!("{TAP_PREFIX}{}{index:x}", &hex[..TAP_UID_DIGITS]),
            bridge: bridge.to_string(),
            mac,
        });
    }

    let empty_os_disk = spec.boot_source.kind == ChBootSourceKind::InstallMedia;
    let mut disks = vec![PlannedDisk {
        id: DISK_ID_OS.into(),
        path: os_disk.clone(),
        readonly: false,
    }];
    if empty_os_disk {
        disks.push(PlannedDisk {
            id: DISK_ID_INSTALL.into(),
            path: install_media.clone(),
            readonly: true,
        });
    }
    if spec.user_data.is_some() {
        disks.push(PlannedDisk {
            id: DISK_ID_SEED.into(),
            path: seed.clone(),
            readonly: true,
        });
    }

    let guest = GuestPlan {
        firmware: config.vmm.firmware.clone(),
        boot_vcpus: spec.cpus.boot,
        max_vcpus: spec.cpus.max_vcpus(),
        memory_mib: u64::from(spec.memory.size_mi_b),
        hugepages: spec.memory.hugepages,
        disks,
        nics: nics
            .iter()
            .map(|n| PlannedNic {
                id: n.name.clone(),
                tap: n.tap.clone(),
                mac: n.mac.clone(),
            })
            .collect(),
        tpm_socket: tpm.as_ref().map(|t| t.socket.clone()),
        vsock_socket: Some(vsock_socket),
        serial_file: serial_log.clone(),
        landlock: true,
    };

    Ok(MachinePlan {
        uid: uid.to_string(),
        host_uid,
        unit: instance_unit(VMM_TEMPLATE, &host_uid_str),
        machine_dir,
        run_dir,
        api_socket,
        image,
        install_media,
        os_disk,
        os_disk_bytes: u64::from(spec.os_disk_size_gi_b).saturating_mul(BYTES_PER_GIB),
        seed,
        serial_log,
        nics,
        provider_id: cloud_hypervisor_provider_id(provider_name, uid),
        guest,
        tpm,
        empty_os_disk,
        report_socket,
    })
}

/// Start this machine's VMM: an instance of `banlieue-ch@.service`
/// (ADR-0063, amended), with its memory ceiling set at runtime.
///
/// The template runs the VMM as the guest's uid and private group, with only
/// its API socket; the provider then creates and boots the VM through the
/// API. The socket takes the provider's group from the setgid run directory,
/// and the provider opens it to that group
/// ([`hostfs::grant_api_socket`](crate::hostfs::grant_api_socket)).
#[must_use]
pub fn vmm_unit(plan: &MachinePlan) -> UnitStart {
    let guest_bytes = plan.guest.memory_mib.saturating_mul(BYTES_PER_MIB);
    UnitStart {
        name: plan.unit.clone(),
        memory_max: Some(guest_bytes.saturating_add(VMM_MEMORY_OVERHEAD_BYTES)),
        environment: None,
    }
}

/// Manufacture the machine's TPM (ADR-0065 Decision 1): an instance of
/// `banlieue-swtpm-setup@.service`, which runs `swtpm_setup` as the
/// provider's user (signing reads the CA key, which no guest uid may) and
/// writes the EK certificates where the provider publishes them from. The
/// machine-specific `--vmid` and EK directory go in its environment file.
/// `None` without a vTPM.
#[must_use]
pub fn swtpm_setup_unit(plan: &MachinePlan) -> Option<UnitStart> {
    let t = plan.tpm.as_ref()?;
    Some(UnitStart {
        name: t.setup_unit.clone(),
        memory_max: None,
        environment: Some(EnvFile {
            path: t.setup_env.clone(),
            vars: vec![
                ("VMID".into(), t.vmid.clone()),
                ("EK_DIR".into(), t.ek_dir.display().to_string()),
            ],
        }),
    })
}

/// The guest's swtpm (ADR-0065 Decision 1): an instance of
/// `banlieue-swtpm@.service`, as the guest's uid. `None` without a vTPM.
#[must_use]
pub fn swtpm_unit(plan: &MachinePlan) -> Option<UnitStart> {
    let t = plan.tpm.as_ref()?;
    Some(UnitStart {
        name: t.unit.clone(),
        memory_max: None,
        environment: None,
    })
}

/// The lowest uid in the guest range not in `used`, or `None` when full.
#[must_use]
pub fn allocate_host_uid(used: &BTreeSet<u32>, guests: GuestsSection) -> Option<u32> {
    (0..guests.uid_count)
        .map(|offset| guests.uid_base + offset)
        .find(|uid| !used.contains(uid))
}

pub(crate) fn require_uuid(uid: &str) -> Result<(), PlanError> {
    let ok = uid.len() == UUID_LEN
        && uid.char_indices().all(|(i, c)| {
            if UUID_DASHES.contains(&i) {
                c == '-'
            } else {
                c.is_ascii_digit() || ('a'..='f').contains(&c)
            }
        });
    if ok {
        return Ok(());
    }
    Err(PlanError::InvalidUid(uid.to_string()))
}

/// A plain file name: no separators, no leading dot, printable ASCII.
pub(crate) fn require_image_name(name: &str) -> Result<(), PlanError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_IMAGE_NAME
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        return Ok(());
    }
    Err(PlanError::InvalidImage(name.to_string()))
}

/// Six colon-separated hex octets, returned lowercased.
fn require_mac(mac: &str) -> Result<String, PlanError> {
    let octets: Vec<&str> = mac.split(':').collect();
    let ok = octets.len() == MAC_OCTETS
        && octets
            .iter()
            .all(|o| o.len() == 2 && o.bytes().all(|b| b.is_ascii_hexdigit()));
    if ok {
        return Ok(mac.to_ascii_lowercase());
    }
    Err(PlanError::InvalidMac(mac.to_string()))
}

/// A stable, locally administered unicast MAC from the UID and NIC index.
fn derived_mac(uid_hex: &str, index: usize) -> String {
    let seed = &uid_hex[MAC_UID_DIGITS];
    let index = u8::try_from(index).unwrap_or(u8::MAX);
    format!(
        "{MAC_LOCAL_UNICAST:02x}:{}:{}:{}:{}:{index:02x}",
        &seed[0..2],
        &seed[2..4],
        &seed[4..6],
        &seed[6..8]
    )
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod plan_tests;
