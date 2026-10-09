// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Typed errors for the main controller.
//!
//! Provider-specific reconcilers live in separate crates and define their own
//! error types; the variants here are scoped to the controller's
//! scheduler / status mirror / migration logic.

/// Error returned from controller reconcile loops.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying SDK error (client construction, finalizer patch, SSA, ...).
    #[error("sdk: {0}")]
    Sdk(#[from] banlieue_provider_sdk::Error),

    /// Underlying `kube` client / API error not wrapped by the SDK.
    #[error("kube api: {0}")]
    Kube(#[from] kube::Error),

    /// JSON serialization or deserialization failure.
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    /// A required field on the resource being reconciled was missing.
    #[error("missing required field: {0}")]
    Missing(&'static str),
}

/// The `kind` label of `banlieue_reconcile_errors_total` (ADR-0091): the
/// variant name, never the message.
impl banlieue_provider_sdk::metrics::ErrorKind for Error {
    fn kind(&self) -> &'static str {
        match self {
            Self::Sdk(_) => "Sdk",
            Self::Kube(_) => "Kube",
            Self::Serde(_) => "Serde",
            Self::Missing(_) => "Missing",
        }
    }
}

/// Convenient alias.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
