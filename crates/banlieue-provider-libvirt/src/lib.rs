// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-provider-libvirt
//!
//! The banlieue provider for libvirt/KVM hosts.
//!
//! Watches [`banlieue_api::banlieue::Provider`] CRs whose
//! `spec.providerClassRef.name == "libvirt"`, connects to the host over mutual
//! TLS, verifies the storage pools and networks the admin declared, and
//! publishes `status.failureDomains[]`.
//!
//! All libvirt protocol work lives in the first-party `banlieue-libvirt`
//! crate: no native library, no third-party libvirt client, and TLS only
//! (ADR-0011). Control-plane calls run in-process; bulk image transfer will
//! run in a Job so gigabytes never flow through a reconcile loop.
//!
//! Communication with the main `banlieue-controller` is **CRD-only** — both
//! talk to the Kubernetes API server, never to each other.
//!
//! This crate is a library: the unified `banlieue` binary calls [`run`] for
//! the `banlieue provider libvirt` subcommand (ADR-0004). It has no `main`.

pub mod app;
pub mod client;
pub mod context;
pub mod credentials;
pub mod error;
pub mod import;
pub mod reconciler;

pub use app::{Cli, run};
pub use context::Context;
pub use error::{Error, Result};
