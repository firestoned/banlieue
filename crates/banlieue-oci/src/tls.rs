// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The HTTP client's TLS trust (ADR-0110).
//!
//! A registry served by a private CA is unreachable from the distroless
//! banlieue image, which carries only the public roots. The operator names a
//! CA bundle; [`http_client`] adds every certificate in it as a root,
//! alongside the public ones. banlieue owns the transport, so it owns trust:
//! the certificates are added explicitly rather than through an environment
//! variable whose meaning depends on the verifier a dependency selects.
//!
//! There is deliberately no way to skip verification here.

use std::path::Path;

use crate::error::{Error, Result};

/// Build the HTTP client the registry calls use.
///
/// With `ca_bundle` the file is read, parsed, and each certificate in it is
/// added as a root certificate; without it the client trusts what reqwest
/// trusts by default.
///
/// # Errors
/// [`Error::Tls`] naming `ca_bundle` when it cannot be read, or holds no
/// certificate (an empty bundle would silently leave the client on the public
/// roots while the operator believes a CA is pinned), and when the client
/// cannot be built.
pub fn http_client(ca_bundle: Option<&Path>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder();
    if let Some(path) = ca_bundle {
        let pem = std::fs::read(path)
            .map_err(|e| Error::Tls(format!("reading CA bundle {}: {e}", path.display())))?;
        let roots = root_certificates(&pem)
            .map_err(|e| Error::Tls(format!("CA bundle {}: {e}", path.display())))?;
        for root in roots {
            builder = builder.add_root_certificate(root);
        }
    }
    builder
        .build()
        .map_err(|e| Error::Tls(format!("building the HTTP client: {e}")))
}

/// Parse a PEM bundle into root certificates.
///
/// Uses `Certificate::from_pem_bundle` (not `from_pem`) so a bundle holding a
/// chain, or several CAs, contributes every certificate rather than the
/// first.
///
/// # Errors
/// [`Error::Tls`] when the PEM cannot be parsed or holds no certificate.
pub fn root_certificates(pem: &[u8]) -> Result<Vec<reqwest::Certificate>> {
    let roots = reqwest::Certificate::from_pem_bundle(pem)
        .map_err(|e| Error::Tls(format!("invalid PEM: {e}")))?;
    if roots.is_empty() {
        return Err(Error::Tls(
            "no certificates found (expected at least one BEGIN CERTIFICATE block)".to_string(),
        ));
    }
    Ok(roots)
}

#[cfg(test)]
#[path = "tls_tests.rs"]
mod tls_tests;
