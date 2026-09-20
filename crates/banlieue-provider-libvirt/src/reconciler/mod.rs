// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! libvirt provider reconcilers.
//!
//! - [`libvirtmachine`] — a scheduled VM becomes a real domain (ADR-0050).
//! - [`provider`] — capability verification against a real host.
//! - [`vmimage`] — the libvirt half of ADR-0010's pipeline: gate on the shared
//!   raw disk, then one import Job per storage pool.

pub mod libvirtmachine;
pub mod provider;
pub mod vmimage;
