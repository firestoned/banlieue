// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Disk image container formats banlieue writes itself (ADR-0162
//! Decision 2).
//!
//! banlieue's image builder produces a raw disk. A cloud wants that disk in
//! its own container: Azure a fixed VHD, roadmap 15's vSphere path a VMDK.
//! Both are a few hundred bytes of metadata around the raw sectors, so this
//! crate writes them directly instead of shelling out to `qemu-img`.
//!
//! - [`vhd`]: the fixed VHD footer and a streaming reader that yields
//!   `raw disk + zero padding + footer` without loading the disk.
//!
//! Everything here is synchronous and side-effect free: callers own the I/O.

pub mod vhd;

pub use vhd::{
    FixedVhdLayout, FixedVhdReader, Geometry, ParsedFooter, VhdError, chs_geometry, fixed_footer,
    is_all_zero, parse_footer, vhd_timestamp,
};
