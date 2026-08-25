// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-provider-sdk
//!
//! Shared runtime helpers used by `banlieue-controller` and every
//! `banlieue-provider-*` crate.
//!
//! The SDK is intentionally small — it captures the parts of writing a
//! kube-rs controller that every banlieue controller needs (client setup,
//! condition helpers, finalizer add/remove, server-side apply) without
//! pulling in a heavyweight framework.
//!
//! Modules:
//!
//! - [`bootstrap`] — shared process startup: `tracing` init, the health
//!   server, and the SIGTERM / Ctrl-C shutdown future.
//! - [`client`] — build a [`kube::Client`] from kubeconfig or in-cluster
//!   config, with explicit timeouts.
//! - [`status`] — typed helpers for `metav1.Condition` lists.
//! - [`finalizer`] — patch-based finalizer add / remove.
//! - [`ssa`] — server-side apply helper.
//! - [`reconciler`] — small constants and helpers around
//!   [`kube::runtime::controller::Action`].
//! - [`leader`] — lease-based leader election so only one controller
//!   replica runs reconcilers at a time.
//! - [`error`] — shared error type re-exported by the rest of the SDK.
//! - [`guestdata`] — placeholder substitution for a `VirtualMachine.spec.
//!   userData` cloud-config before a provider delivers it to the guest
//!   (ADR-0024).
//! - [`osartifact`] — `ownerReference` helper binding a per-zone import
//!   Job's lifecycle to the `OSArtifact` whose PVC it mounts (ADR-0027).

pub mod bootstrap;
pub mod ca_bundle;
pub mod client;
pub mod error;
pub mod finalizer;
pub mod guestdata;
pub mod leader;
pub mod osartifact;
pub mod reconciler;
pub mod scheduling;
pub mod ssa;
pub mod status;

pub use error::{Error, Result};
