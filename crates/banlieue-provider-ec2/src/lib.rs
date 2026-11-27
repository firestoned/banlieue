// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-provider-ec2
//!
//! The banlieue provider for Amazon EC2 (ADR-0174).
//!
//! Watches [`banlieue_api::banlieue::Provider`] CRs whose
//! `spec.providerClassRef.name == "ec2"` and publishes one failure domain per
//! availability zone that offers at least one mapped instance type
//! (ADR-0158 Decision 3, ADR-0160). It realises `EC2Machine`s as hardened
//! instances through the EC2 API, never blocking a worker on a state change,
//! and sweeps tagged instances whose owner is gone (ADR-0158 Decision 5).
//!
//! The only credential is the provider's own ServiceAccount token, exchanged
//! at STS by `AssumeRoleWithWebIdentity` for a fifteen-minute session
//! (ADR-0159, ADR-0174 Decision 6): no Secret is read. All protocol work
//! lives in the first-party `banlieue-aws` crate.
//!
//! Communication with `banlieue-controller` is **CRD-only**.
//!
//! This crate is a library: the unified `banlieue` binary calls [`run`] for
//! the `banlieue provider ec2` subcommand (ADR-0004). It has no `main`.

pub mod app;
pub mod client;
pub mod context;
pub mod error;
pub mod metrics;
pub mod reconciler;

#[cfg(test)]
mod test_support;

pub use app::{Cli, run};
pub use context::Context;
pub use error::{Error, Result};
