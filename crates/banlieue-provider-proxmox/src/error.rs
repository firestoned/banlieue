// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Typed errors for the Proxmox provider's reconcilers.

/// Error returned from `banlieue-provider-proxmox` reconcile loops.
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

    /// The Proxmox API refused, failed, or a task did not finish.
    #[error("proxmox: {0}")]
    Proxmox(#[from] banlieue_proxmox::Error),

    /// A required field on the resource being reconciled was missing.
    #[error("missing required field: {0}")]
    Missing(&'static str),

    /// A configuration value or spec was present but unusable.
    #[error("invalid {what}: {detail}")]
    Invalid {
        /// What was invalid.
        what: &'static str,
        /// Why.
        detail: String,
    },
}

/// Convenient alias.
pub type Result<T> = std::result::Result<T, Error>;
