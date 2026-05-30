// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Typed errors for the vSphere provider's reconcilers.

/// Error returned from `banlieue-provider-vsphere` reconcile loops.
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

    /// vSphere client transport / authentication / inventory failure. Always
    /// `Display`-able with the underlying vim_rs error message; we don't
    /// preserve the structured type because vim_rs's error type is large and
    /// adds little for our reconciler's decision logic.
    #[error("vsphere: {0}")]
    Vsphere(String),

    /// A required field on the resource being reconciled was missing.
    #[error("missing required field: {0}")]
    Missing(&'static str),
}

/// Convenient alias.
pub type Result<T> = std::result::Result<T, Error>;
