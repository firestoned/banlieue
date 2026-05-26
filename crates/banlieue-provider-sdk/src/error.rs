// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared error type for the SDK.
//!
//! Provider crates should define their own typed errors (typically with a
//! `#[from] banlieue_provider_sdk::Error` variant) so that SDK failures
//! propagate through provider error chains without losing type information.

/// SDK-level errors. Specific operations re-use these where appropriate
/// rather than each module defining its own.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying `kube` client / API error.
    #[error("kube api: {0}")]
    Kube(#[from] kube::Error),

    /// Kubeconfig discovery or parsing failed.
    #[error("kube config: {0}")]
    KubeConfig(#[from] kube::config::KubeconfigError),

    /// In-cluster configuration discovery failed.
    #[error("in-cluster config: {0}")]
    InClusterConfig(#[from] kube::config::InClusterError),

    /// Kubeconfig inference (env var / local path discovery) failed.
    #[error("kubeconfig inference: {0}")]
    InferConfig(#[from] kube::config::InferConfigError),

    /// Serialization or deserialization failure (typically server-side apply).
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    /// A required field on the resource being reconciled was missing —
    /// e.g. namespace on a namespaced resource. Indicates a programming or
    /// API-server bug, not a transient condition.
    #[error("missing required field: {0}")]
    Missing(&'static str),
}

/// Convenient alias used throughout the SDK.
pub type Result<T> = std::result::Result<T, Error>;
