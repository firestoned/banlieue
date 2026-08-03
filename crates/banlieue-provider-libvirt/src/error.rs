// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Typed errors for the libvirt provider's reconcilers.

/// Error returned from `banlieue-provider-libvirt` reconcile loops.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying SDK error (client construction, SSA, ...).
    #[error("sdk: {0}")]
    Sdk(#[from] banlieue_provider_sdk::Error),

    /// Underlying `kube` client / API error not wrapped by the SDK.
    #[error("kube api: {0}")]
    Kube(#[from] kube::Error),

    /// JSON serialization or deserialization failure.
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    /// libvirt transport / protocol failure. Carried as a string because the
    /// reconciler's decisions do not branch on the underlying variant — it is
    /// surfaced on status for an operator to read.
    #[error("libvirt: {0}")]
    Libvirt(String),

    /// A required field on the resource being reconciled was missing.
    #[error("missing required field: {0}")]
    Missing(&'static str),

    /// A configuration value was present but unusable.
    #[error("invalid {what}: {detail}")]
    Invalid { what: &'static str, detail: String },
}

impl From<banlieue_libvirt::TransportError> for Error {
    fn from(e: banlieue_libvirt::TransportError) -> Self {
        Self::Libvirt(e.to_string())
    }
}

/// Convenient alias.
pub type Result<T> = std::result::Result<T, Error>;
