// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-provider-azure
//!
//! The banlieue provider for Microsoft Azure (ADR-0164).
//!
//! Watches [`banlieue_api::banlieue::Provider`] CRs whose
//! `spec.providerClassRef.name == "azure"` and publishes one failure domain
//! per availability zone that offers at least one mapped instance size
//! (ADR-0158 Decision 3, ADR-0160), with quota headroom as capacity
//! (ADR-0161). It realises `AzureVirtualMachine`s through Azure Resource
//! Manager, never blocking a worker on a long-running operation, and sweeps
//! tagged resources whose owner is gone (ADR-0158 Decision 5).
//!
//! The only credential is the provider's own ServiceAccount token, exchanged
//! at Entra ID (ADR-0159): no Secret is read. All protocol work lives in the
//! first-party `banlieue-azure` crate.
//!
//! Communication with `banlieue-controller` is **CRD-only**.
//!
//! This crate is a library: the unified `banlieue` binary calls [`run`] for
//! the `banlieue provider azure` subcommand (ADR-0004). It has no `main`.

pub mod app;
pub mod client;
pub mod cloud;
pub mod context;
pub mod error;
pub mod publish;
pub mod reconciler;
pub mod sshkey;

#[cfg(test)]
mod test_support;

pub use app::{Cli, run};
pub use context::Context;
pub use error::{Error, Result};
