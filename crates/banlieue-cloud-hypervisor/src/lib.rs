// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A first-party client for Cloud Hypervisor's REST API (ADR-0061).
//!
//! Cloud Hypervisor runs one VMM process per guest, each serving a REST API
//! on a local Unix socket. This crate speaks that API for the host-resident
//! provider (ADR-0060): HTTP/1.1 over a `UnixStream`, with hand-written types
//! for only the endpoints banlieue uses. No `ch-remote` subprocess, no code
//! generation, and no `kube`.
//!
//! - [`types`]: request and response types. A `vm.create` body can only be
//!   built from a [`GuestPlan`], which forces raw image types, nested
//!   virtualization off and a virtio-rng device.
//! - [`wire`]: pure encode/decode halves and the version gate.
//! - [`socket`]: the ownership check made before connecting.
//! - [`client`]: the async client.
//!
//! The types are checked against a vendored copy of the upstream API
//! document for the pinned release (`spec/`, ADR-0061 Decision 2).

pub mod client;
pub mod error;
pub mod socket;
pub mod types;
pub mod wire;

pub use client::Client;
pub use error::{Error, Result};
pub use socket::ExpectedSocket;
pub use types::{GuestPlan, PlannedDisk, PlannedNic, VmInfo, VmState, VmmPing};
pub use wire::{PINNED_VERSION, VmmVersion, check_version};

#[cfg(test)]
#[path = "spec_tests.rs"]
mod spec_tests;
