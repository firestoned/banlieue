// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-imagebuilder
//!
//! The provider-agnostic half of banlieue's `VMImage` build pipeline: turns
//! an OCI/Kairos-referenced image (`VMImage.spec.sources[].kind == Url`)
//! into a raw disk via kairos-operator's `OSArtifact` CRD, and mirrors
//! progress into `VMImage.status.buildArtifact`.
//!
//! This crate has **no knowledge of vSphere, Proxmox, or libvirt** — that
//! backend-specific work (converting the raw disk, importing it into
//! zone-scoped storage) belongs to each provider's own crate, which reads
//! `status.buildArtifact` once it reports `phase: Ready`. See
//! `docs/adr/0010-vmimage-build-pipeline-imagebuilder.md`.
//!
//! Communication with every provider is **CRD-only** — this controller and
//! provider controllers never call each other; both only ever talk to the
//! Kubernetes API server.
//!
//! This crate is a library: the unified `banlieue` binary calls [`run`] for
//! the `banlieue imagebuilder` subcommand (see ADR-0004). It has no `main`.

pub mod app;
pub mod context;
pub mod error;
pub mod importer_image;
pub mod reconciler;

pub use app::{Cli, run};
pub use context::Context;
pub use error::{Error, Result};
pub use importer_image::ImporterImage;
