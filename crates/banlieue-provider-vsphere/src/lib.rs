// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-provider-vsphere
//!
//! The banlieue provider for VMware vSphere / vCenter.
//!
//! This binary watches two kinds of resources:
//!
//! 1. [`banlieue_api::banlieue::Provider`] CRs whose
//!    `spec.provider_class_ref.name == "vsphere"`. For each it connects to the
//!    configured vCenter, walks the inventory, and patches
//!    `Provider.status.failureDomains[]` plus health conditions.
//! 2. [`banlieue_api::infrastructure::VSphereMachine`] CRs (Phase 1B
//!    iteration 2+) — drives each VM toward its desired state on vCenter.
//!
//! Communication with the main `banlieue-controller` is **CRD-only** — both
//! controllers talk to the Kubernetes API server, never to each other.
//!
//! This crate is a library: the unified `banlieue` binary calls [`run`] for the
//! `banlieue provider vsphere` subcommand (see ADR-0004). It has no `main`.

pub mod app;
pub mod client;
pub mod context;
pub mod error;
pub mod reconciler;

pub use app::{Cli, run};
pub use context::Context;
pub use error::{Error, Result};
