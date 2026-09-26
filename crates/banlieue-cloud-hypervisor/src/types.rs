// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Request and response types for the endpoints banlieue uses.
//!
//! Hand-written, not generated, and limited to what is actually sent or read
//! (ADR-0061 Decision 1). `spec_tests.rs` checks every field sent against the
//! pinned upstream document.
//!
//! # Requests are built from a plan, not by hand
//!
//! A `vm.create` body is only obtainable through [`VmConfigRequest::for_guest`],
//! which fills in three things the caller cannot turn off
//! (ADR-0061 Decision 4):
//!
//! - `image_type: Raw` on every disk. Left to auto-detection, v53 disables
//!   sector-0 writes on a raw disk, which breaks partitioning.
//! - `nested: false`. v53 turns nested virtualization on by default, and this
//!   provider targets bare-metal hosts that must not offer it.
//! - a virtio-rng device fed from `/dev/urandom`.
//!
//! # Responses are tolerant
//!
//! Decoded types ignore fields they do not know, and unknown VM states map to
//! [`VmState::Unknown`], so a newer VMM does not break decoding.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Bytes in one MiB, for converting the plan's memory size to the API's.
const BYTES_PER_MIB: u64 = 1024 * 1024;
/// Entropy source for the guest's virtio-rng device.
const RNG_SOURCE: &str = "/dev/urandom";

/// Everything the provider decides about one guest, before it is encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestPlan {
    /// Firmware image to boot (`CLOUDHV.fd`).
    pub firmware: PathBuf,
    /// vCPUs at boot.
    pub boot_vcpus: u32,
    /// Hotplug ceiling. Raised to `boot_vcpus` if lower.
    pub max_vcpus: u32,
    /// Guest memory in MiB.
    pub memory_mib: u64,
    /// Back guest memory with hugepages.
    pub hugepages: bool,
    /// Disks in boot order: OS disk first (roadmap 09 Gotchas).
    pub disks: Vec<PlannedDisk>,
    /// Network interfaces.
    pub nics: Vec<PlannedNic>,
    /// swtpm control socket, when the guest has a vTPM.
    pub tpm_socket: Option<PathBuf>,
    /// File the serial console is written to.
    pub serial_file: PathBuf,
    /// Enable the VMM's Landlock sandbox (ADR-0063 Decision 3).
    pub landlock: bool,
}

/// One disk in a [`GuestPlan`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedDisk {
    /// Device id. `vm.remove-device` takes it, so it must be stable.
    pub id: String,
    /// Path of the raw image on the host.
    pub path: PathBuf,
    /// Attach read-only.
    pub readonly: bool,
}

/// One network interface in a [`GuestPlan`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedNic {
    /// Device id.
    pub id: String,
    /// Existing tap device, created by the provider (ADR-0063 Decision 4).
    pub tap: String,
    /// Guest MAC address.
    pub mac: String,
}

// ----------------------------------------------------------------------
// vm.create
// ----------------------------------------------------------------------

/// A `vm.create` body. Constructed only by [`VmConfigRequest::for_guest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VmConfigRequest {
    cpus: CpusRequest,
    memory: MemoryRequest,
    payload: PayloadRequest,
    disks: Vec<DiskRequest>,
    net: Vec<NetRequest>,
    rng: RngRequest,
    serial: ConsoleRequest,
    console: ConsoleRequest,
    #[serde(skip_serializing_if = "Option::is_none")]
    tpm: Option<TpmRequest>,
    landlock_enable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct CpusRequest {
    boot_vcpus: u32,
    max_vcpus: u32,
    nested: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct MemoryRequest {
    size: u64,
    hugepages: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct PayloadRequest {
    firmware: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct DiskRequest {
    id: String,
    path: PathBuf,
    readonly: bool,
    image_type: ImageType,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct NetRequest {
    id: String,
    tap: String,
    mac: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct RngRequest {
    src: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ConsoleRequest {
    mode: ConsoleMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct TpmRequest {
    socket: PathBuf,
}

/// Where a console or serial port goes. Only the modes banlieue uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
enum ConsoleMode {
    Off,
    File,
}

impl VmConfigRequest {
    /// The `vm.create` body for `plan`, with the invariants in the module
    /// docs applied.
    #[must_use]
    pub fn for_guest(plan: &GuestPlan) -> Self {
        Self {
            cpus: CpusRequest {
                boot_vcpus: plan.boot_vcpus,
                max_vcpus: plan.max_vcpus.max(plan.boot_vcpus),
                nested: false,
            },
            memory: MemoryRequest {
                size: plan.memory_mib.saturating_mul(BYTES_PER_MIB),
                hugepages: plan.hugepages,
            },
            payload: PayloadRequest {
                firmware: plan.firmware.clone(),
            },
            disks: plan
                .disks
                .iter()
                .map(|d| DiskRequest {
                    id: d.id.clone(),
                    path: d.path.clone(),
                    readonly: d.readonly,
                    image_type: ImageType::Raw,
                })
                .collect(),
            net: plan
                .nics
                .iter()
                .map(|n| NetRequest {
                    id: n.id.clone(),
                    tap: n.tap.clone(),
                    mac: n.mac.clone(),
                })
                .collect(),
            rng: RngRequest { src: RNG_SOURCE },
            serial: ConsoleRequest {
                mode: ConsoleMode::File,
                file: Some(plan.serial_file.clone()),
            },
            console: ConsoleRequest {
                mode: ConsoleMode::Off,
                file: None,
            },
            tpm: plan
                .tpm_socket
                .as_ref()
                .map(|s| TpmRequest { socket: s.clone() }),
            landlock_enable: plan.landlock,
        }
    }
}

// ----------------------------------------------------------------------
// Responses
// ----------------------------------------------------------------------

/// Disk image format, as the VMM names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageType {
    /// Raw image. The only type banlieue sends.
    Raw,
    /// Fixed VHD.
    FixedVhd,
    /// QCOW2.
    Qcow2,
    /// VHDX.
    Vhdx,
    /// The VMM could not tell.
    Unknown,
}

/// The run state of a VM.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum VmState {
    /// Created, not booted.
    Created,
    /// Running.
    Running,
    /// Shut down; the VMM process is still up.
    Shutdown,
    /// Paused.
    Paused,
    /// A state this client does not know. A newer VMM may add some.
    #[serde(other)]
    Unknown,
}

/// `vmm.ping` response.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct VmmPing {
    /// Semantic version, for example `53.0.0`.
    pub version: String,
    /// Build tag, for example `v53.0`.
    #[serde(default)]
    pub build_version: Option<String>,
}

/// `vm.info` response.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct VmInfo {
    /// The configuration the VM is running with.
    pub config: VmConfig,
    /// Its run state.
    pub state: VmState,
}

/// The parts of a VM's configuration banlieue reads back.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct VmConfig {
    /// vCPU configuration.
    #[serde(default)]
    pub cpus: Option<CpusConfig>,
    /// Disks, in their current order.
    #[serde(default)]
    pub disks: Vec<DiskConfig>,
}

/// vCPU configuration as the VMM reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct CpusConfig {
    /// vCPUs at boot.
    pub boot_vcpus: u32,
    /// Hotplug ceiling.
    pub max_vcpus: u32,
    /// Nested virtualization.
    #[serde(default)]
    pub nested: bool,
}

/// A disk as the VMM reports it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DiskConfig {
    /// Device id.
    #[serde(default)]
    pub id: Option<String>,
    /// Image path.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Attached read-only.
    #[serde(default)]
    pub readonly: bool,
    /// Image type.
    #[serde(default)]
    pub image_type: Option<ImageType>,
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod types_tests;
