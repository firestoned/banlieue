// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Errors.

/// Why an OCI operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A reference or digest is malformed.
    #[error("reference: {0}")]
    Reference(String),
    /// The registry answered with an unexpected status.
    #[error("registry {status} for {url}: {body}")]
    Registry {
        /// HTTP status.
        status: u16,
        /// The URL requested.
        url: String,
        /// The start of the response body, for diagnosis.
        body: String,
    },
    /// Content did not hash to the digest it was addressed by. Nothing is
    /// written in its place (fail closed, SEC-004).
    #[error("digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch {
        /// The digest asked for.
        expected: String,
        /// What the bytes hashed to.
        actual: String,
    },
    /// A manifest is not the shape this client pushes.
    #[error("manifest: {0}")]
    Manifest(String),
    /// An authentication challenge could not be answered.
    #[error("auth: {0}")]
    Auth(String),
    /// Transport failure.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    /// Local file failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;
