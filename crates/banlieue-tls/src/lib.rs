// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The one rustls crypto provider every banlieue process uses (ADR-0170).
//!
//! banlieue links rustls without a bundled provider in reqwest
//! (`rustls-no-provider`), so each binary installs a process default before
//! its first TLS connection. This crate is the single place that default is
//! built: rustls' `aws-lc-rs` provider, with the hybrid post-quantum group
//! `X25519MLKEM768` first in the key-exchange order. Every client (kube,
//! reqwest, tokio-rustls, the OTLP exporter) builds its `ClientConfig` from
//! the process default, so every handshake offers ML-KEM first and falls back
//! to a classical group only when the peer does not accept it.
//!
//! `ring` is not in the dependency graph: it has no ML-KEM, and `deny.toml`
//! bans it so it cannot come back through a feature default.

use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::crypto::SupportedKxGroup;
use rustls::crypto::aws_lc_rs::{self, kx_group};

/// The key-exchange groups banlieue offers, most preferred first.
///
/// `X25519MLKEM768` (the hybrid of X25519 and ML-KEM-768, the group browsers
/// and most TLS stacks deploy) leads; the classical groups follow only for
/// peers that do not offer ML-KEM yet (Azure Resource Manager and Entra ID,
/// measured 2026-11-26, and some hypervisor APIs).
pub static KX_GROUPS: &[&'static dyn SupportedKxGroup] = &[
    kx_group::X25519MLKEM768,
    kx_group::X25519,
    kx_group::SECP256R1,
    kx_group::SECP384R1,
];

/// Build banlieue's crypto provider: aws-lc-rs with [`KX_GROUPS`] as the
/// key-exchange order. The cipher suites and signature algorithms are
/// aws-lc-rs's defaults.
#[must_use]
pub fn provider() -> CryptoProvider {
    CryptoProvider {
        kx_groups: KX_GROUPS.to_vec(),
        ..aws_lc_rs::default_provider()
    }
}

/// Install [`provider`] as the process-default rustls provider and return
/// the provider now in effect.
///
/// Idempotent: when a provider is already installed it is kept (rustls allows
/// one per process) and returned. With `ring` banned from the graph the only
/// provider anything can have installed is aws-lc-rs, whose default order
/// (rustls' `prefer-post-quantum`) also puts `X25519MLKEM768` first.
pub fn install_default_crypto_provider() -> Arc<CryptoProvider> {
    // An `Err` only means a provider was installed first; it is kept.
    let _ = provider().install_default();
    CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(provider()))
}

#[cfg(test)]
mod lib_tests;
