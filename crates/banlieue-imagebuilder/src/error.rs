// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Typed errors for `banlieue-imagebuilder`'s reconciler.

/// Error returned from `banlieue-imagebuilder` reconcile loops.
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

    /// Cloud-config merge failure (ADR-0037).
    #[error("cloud-config merge: {0}")]
    CloudConfigMerge(#[from] crate::cloud_config_merge::MergeError),
}

/// Convenient alias.
pub type Result<T> = std::result::Result<T, Error>;
