// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Proxmox provider reconcilers.
//!
//! - [`proxmoxmachine`]: a scheduled VM becomes a real Proxmox VM (ADR-0075).
//! - [`provider`]: capability probing against a real cluster.
//! - [`image`]: verify a `VMImage`'s template VMID and publish it.

pub mod image;
pub mod provider;
pub mod proxmoxmachine;
