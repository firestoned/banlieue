// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-oci
//!
//! Just enough of the OCI distribution protocol to move one disk image to a
//! host that cannot mount cluster storage (ADR-0064): push a file as a
//! single-layer artifact, and pull it back **by digest** into a sparse file,
//! verified before it is renamed into place.
//!
//! First-party, like `banlieue-libvirt` and `banlieue-cloud-hypervisor`:
//! protocol only, no `kube`, no banlieue API types, no subprocess.
//!
//! - [`reference`]: `registry/repository[:tag][@sha256:…]`, registry always
//!   explicit.
//! - [`auth`]: anonymous, Basic, and Bearer token challenges.
//! - [`manifest`]: the one manifest shape pushed and accepted.
//! - [`sparse`]: zero runs become holes.
//! - [`client`]: push and pull.

pub mod auth;
pub mod client;
pub mod error;
pub mod manifest;
pub mod reference;
pub mod sparse;

pub use auth::Credentials;

/// Install rustls' `ring` provider as the process default.
///
/// The workspace's reqwest links rustls without a provider
/// (`rustls-no-provider`) and panics on first TLS use if none is set.
/// Idempotent: a provider already installed is kept.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
pub use client::{Client, Pulled, Pushed};
pub use error::{Error, Result};
pub use reference::Reference;
