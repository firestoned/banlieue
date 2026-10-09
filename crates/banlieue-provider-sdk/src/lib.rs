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
//! - [`bootstrap`]: shared process startup: `tracing` init (with opt-in
//!   OTLP trace export, ADR-0092) and the SIGTERM / Ctrl-C shutdown future.
//! - [`health`]: path-aware `/livez` and `/readyz`, with readiness fed by
//!   leader election (ADR-0093).
//! - [`metrics`]: the process Prometheus registry served at `/metrics`
//!   (ADR-0091).
//! - [`runner`]: the instrumented controller runner every role uses, which
//!   records reconcile metrics and spans.
//! - [`httpd`]: the minimal HTTP/1.1 listener behind `health` and `metrics`.
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
pub mod cloudinit;
pub mod ek;
pub mod error;
pub mod finalizer;
pub mod guestdata;
pub mod health;
pub mod httpd;
pub mod leader;
pub mod metrics;
pub mod naming;
pub mod osartifact;
pub mod pem;
pub mod reconciler;
pub mod runner;
pub mod scheduling;
pub mod ssa;
pub mod status;

pub use error::{Error, Result};
