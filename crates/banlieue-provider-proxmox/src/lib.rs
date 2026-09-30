// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-provider-proxmox
//!
//! The banlieue provider for Proxmox VE.
//!
//! Watches [`banlieue_api::banlieue::Provider`] CRs whose
//! `spec.providerClassRef.name == "proxmox"`, verifies the storages and
//! bridges the admin declared on each online node, and publishes one failure
//! domain per node. It realises `ProxmoxMachine`s (ADR-0075) and verifies
//! the template VMIDs `VMImage`s name.
//!
//! All protocol work lives in the first-party `banlieue-proxmox` crate
//! (ADR-0074): API tokens only, every mutation awaited as a bounded task.
//! Cloud-init reaches the guest as a NoCloud seed ISO built with the shared
//! writer in `banlieue-provider-sdk` (ADR-0054).
//!
//! Communication with `banlieue-controller` is **CRD-only**.
//!
//! This crate is a library: the unified `banlieue` binary calls [`run`] for
//! the `banlieue provider proxmox` subcommand (ADR-0004). It has no `main`.

pub mod app;
pub mod client;
pub mod config;
pub mod context;
pub mod credentials;
pub mod error;
pub mod network;
pub mod reconciler;

pub use app::{Cli, run};
pub use context::Context;
pub use error::{Error, Result};
