// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared pieces of a failure domain's published capacity (ADR-0107,
//! ADR-0128): unit conversion, the rounding that keeps a measured figure from
//! changing the Provider status on every pass, and the per-VM memory a QEMU
//! process costs.
//!
//! A Provider status change requeues every VirtualMachine, so measured
//! figures are published in whole GiB and change only when a GiB is gained
//! or lost.

/// Bytes in one MiB.
pub const BYTES_PER_MIB: u64 = 1024 * 1024;
/// Bytes in one GiB.
pub const BYTES_PER_GIB: u64 = BYTES_PER_MIB * MIB_PER_GIB;
/// MiB in one GiB.
pub const MIB_PER_GIB: u64 = 1024;

/// Memory each QEMU-backed VM (libvirt, Proxmox) costs beyond its guest
/// RAM, MiB: the QEMU process's own heap, device emulation and page tables.
/// An estimate, not a measurement; the scheduler adds it to every VM so a
/// host is not filled to the last MiB of guest RAM. Revisit with a sampled
/// figure, as ADR-0106 did for Cloud Hypervisor.
pub const QEMU_MEMORY_OVERHEAD_MIB: u64 = 256;

/// `bytes` in whole MiB, rounded down.
#[must_use]
pub fn bytes_to_mib(bytes: u64) -> u64 {
    bytes / BYTES_PER_MIB
}

/// `bytes` in whole GiB, rounded down.
#[must_use]
pub fn bytes_to_gib(bytes: u64) -> u64 {
    bytes / BYTES_PER_GIB
}

/// `mib` rounded down to a whole GiB, still in MiB.
#[must_use]
pub fn round_down_to_gib(mib: u64) -> u64 {
    mib / MIB_PER_GIB * MIB_PER_GIB
}

/// The `memoryAvailableMiB` to publish: the measured figure, capped at the
/// memory guests may use when that is known, rounded down to a whole GiB.
/// `None` when nothing was measured.
#[must_use]
pub fn available_memory_mib(
    measured_mib: Option<u64>,
    guest_limit_mib: Option<u64>,
) -> Option<u64> {
    measured_mib
        .map(|m| guest_limit_mib.map_or(m, |limit| m.min(limit)))
        .map(round_down_to_gib)
}

#[cfg(test)]
#[path = "capacity_tests.rs"]
mod capacity_tests;
