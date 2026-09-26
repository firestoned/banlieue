// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-provider-cloud-hypervisor
//!
//! banlieue's host-resident provider for Cloud Hypervisor (ADR-0060). It
//! runs on the KVM host itself, watches its own `Provider` and the
//! `CloudHypervisorMachine`s placed on it, and turns each machine into a
//! Cloud Hypervisor guest supervised by systemd (ADR-0063).
//!
//! - [`app`]: the `banlieue provider cloud-hypervisor` entry point, [`run`].
//! - [`host`]: the [`host::HostOps`] seam the reconciler is tested through,
//!   with its real implementation; [`fake`] is the strict in-memory one.
//! - [`host_config`]: the host-local configuration; the only place host
//!   paths and bridges come from (ADR-0062 Decision 4).
//! - [`hostfs`]: a machine's directories, OS disk and seed on the host.
//! - [`machine`]: converge one machine a step at a time, and tear it down.
//! - [`neigh`]: guest IPv4 addresses from the host's neighbour table.
//! - [`plan`]: pure planning from a machine and the host config to every
//!   name, path and VMM setting, with no I/O.
//! - [`systemd`]: guests as instances of root-owned template units, over
//!   D-Bus (ADR-0063, amended).
//! - [`provider`]: this host's `Provider.status`.
//! - [`reconciler`]: the `CloudHypervisorMachine` controller loop.
//! - [`token`]: renewing the provider's own bound token (ADR-0060 D5).
//! - [`vmimage`]: this host's `VMImage.status.perProvider[]` row
//!   (`BackingFile` and registry `Url` sources, ADR-0064).
//! - [`import`]: the `import` subcommand an import unit runs.
//! - [`report`]: the guest's `phase` report over vsock (ADR-0065).
//! - [`sys`]: tap, bridge and reflink syscalls; the crate's only `unsafe`.

// Every `unsafe` block carries a `// SAFETY:` justification (this lint), and
// each carries a per-line `nosemgrep` for Semgrep's blanket `unsafe-usage`
// note, placed only after that block was audited. The SAST job itself
// excludes nothing.
#![deny(clippy::undocumented_unsafe_blocks)]
// `unsafe` is denied everywhere but `sys` (which allows it for exactly two
// audited `ioctl` blocks), so the claim above is checked by the compiler.
#![deny(unsafe_code)]

pub mod app;
pub mod error;
pub mod fake;
pub mod host;
pub mod host_config;
pub mod hostfs;
pub mod import;
pub mod machine;
pub mod neigh;
pub mod plan;
pub mod provider;
pub mod reconciler;
pub mod report;
pub mod sys;
pub mod systemd;
pub mod token;
pub mod vmimage;

pub use app::{Cli, run};
pub use error::{Error, Result};
