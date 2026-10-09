// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Errors from the Cloud Hypervisor provider.

use crate::host_config::HostConfigError;
use crate::plan::PlanError;

/// Why a provider operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The machine cannot be planned on this host (bad input, unknown class,
    /// unsupported feature). Not transient: fixed by changing the spec or
    /// the host config.
    #[error(transparent)]
    Plan(#[from] PlanError),

    /// The host config is unusable.
    #[error(transparent)]
    HostConfig(#[from] HostConfigError),

    /// A host file or network operation failed.
    #[error("host: {0}")]
    Io(#[from] std::io::Error),

    /// systemd refused or failed.
    #[error("systemd: {0}")]
    Systemd(String),

    /// The VMM refused or failed.
    #[error("VMM: {0}")]
    Vmm(#[from] banlieue_cloud_hypervisor::Error),

    /// The Kubernetes API failed.
    #[error("kubernetes: {0}")]
    Kube(#[from] kube::Error),

    /// A required field was absent.
    #[error("missing {0}")]
    Missing(&'static str),

    /// The VMM's unit failed; the detail is systemd's result and status.
    #[error("VMM exited: {0}")]
    VmmExited(String),

    /// An image import cannot be set up.
    #[error("image import: {0}")]
    Import(String),

    /// The guest uid range on this host is full.
    #[error("no free guest uid in the configured range")]
    UidRangeFull,
}

impl From<zbus::Error> for Error {
    fn from(e: zbus::Error) -> Self {
        Self::Systemd(e.to_string())
    }
}

/// The `kind` label of `banlieue_reconcile_errors_total` (ADR-0091): the
/// variant name, never the message.
impl banlieue_provider_sdk::metrics::ErrorKind for Error {
    fn kind(&self) -> &'static str {
        match self {
            Self::Plan(_) => "Plan",
            Self::HostConfig(_) => "HostConfig",
            Self::Io(_) => "Io",
            Self::Systemd(_) => "Systemd",
            Self::Vmm(_) => "Vmm",
            Self::Kube(_) => "Kube",
            Self::Missing(_) => "Missing",
            Self::VmmExited(_) => "VmmExited",
            Self::Import(_) => "Import",
            Self::UidRangeFull => "UidRangeFull",
        }
    }
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
