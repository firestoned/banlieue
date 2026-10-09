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

    /// The resource's spec is internally inconsistent in a way no CRD schema
    /// rule can express (e.g. two `Provider.spec.failureDomainNameOverrides[]`
    /// entries resolving to the same name, ADR-0023).
    #[error("invalid spec: {0}")]
    InvalidSpec(String),
}

/// The `kind` label of `banlieue_reconcile_errors_total` (ADR-0091): the
/// variant name, never the message.
impl banlieue_provider_sdk::metrics::ErrorKind for Error {
    fn kind(&self) -> &'static str {
        match self {
            Self::Sdk(_) => "Sdk",
            Self::Kube(_) => "Kube",
            Self::Serde(_) => "Serde",
            Self::Vsphere(_) => "Vsphere",
            Self::Missing(_) => "Missing",
            Self::InvalidSpec(_) => "InvalidSpec",
        }
    }
}

/// Convenient alias.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
