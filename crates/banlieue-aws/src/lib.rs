// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A first-party AWS client for the EC2 provider (ADR-0174).
//!
//! Protocol only: no `kube`, no banlieue API types. The only credential is a
//! Kubernetes ServiceAccount token exchanged at STS by
//! `AssumeRoleWithWebIdentity` for a short session (ADR-0159, ADR-0174
//! Decision 6).
//!
//! This first increment holds the pure pieces, each tested against what AWS
//! did in the roadmap 24 phase 0 spike or against AWS's own published
//! vectors:
//!
//! - [`error`]: AWS faults, from any of its error shapes, as typed errors.
//! - [`sigv4`]: Signature Version 4, on aws-lc-rs's HMAC-SHA-256.
//! - [`query`]: the EC2 Query protocol's form encoding.
//! - [`tags`]: ownership tags under EC2's rules (ADR-0158).
//! - [`retry`]: back-off with full jitter for throttling.
//! - [`instance`]: the `RunInstances` request, hardened (ADR-0174 Decision 3).
//! - [`types`]: what EC2 returns, read into the facts the provider decides on.
//! - [`console`]: the guest's `GuestReady` line on the serial console
//!   (ADR-0175).
//! - [`api`]: [`Ec2Api`], the interface reconcilers depend on, and
//!   [`fake`]: [`FakeEc2`], an in-memory EC2 that refuses what EC2 refuses.
//!
//! TLS is the process's rustls provider, which the binary installs:
//! `banlieue-tls`'s aws-lc-rs build, offering X25519MLKEM768 first
//! (ADR-0170). STS, EC2 and S3 accept it.

pub mod api;
pub mod console;
pub mod error;
pub mod fake;
pub mod instance;
pub mod query;
pub mod retry;
pub mod sigv4;
pub mod tags;
pub mod types;

pub use api::Ec2Api;
pub use error::{AwsFault, Error, Result, classify};
pub use fake::FakeEc2;
