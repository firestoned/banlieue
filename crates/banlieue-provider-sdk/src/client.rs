// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Kubernetes client construction with timeouts.
//!
//! Controllers should always build their [`kube::Client`] through this module
//! so timeouts and config-source semantics stay consistent across binaries.

use std::time::Duration;

use kube::{Client, Config};

use crate::error::Result;

/// Default read timeout for K8s API calls (matches kube-rs/controller-rs).
pub const DEFAULT_READ_TIMEOUT_SECS: u64 = 295;

/// Default write timeout for K8s API calls.
pub const DEFAULT_WRITE_TIMEOUT_SECS: u64 = 30;

/// Build a [`kube::Client`] using the standard config-source order:
///
/// 1. In-cluster config (when the binary is running as a pod).
/// 2. Local kubeconfig (`KUBECONFIG` env var or `~/.kube/config`).
///
/// Timeouts are set so a stuck apiserver cannot hang reconciliation forever.
///
/// # Errors
/// Returns [`Error::KubeConfig`](crate::Error::KubeConfig) or
/// [`Error::InClusterConfig`](crate::Error::InClusterConfig) if no configuration
/// source can be resolved.
pub async fn build_client() -> Result<Client> {
    let mut config = match Config::incluster() {
        Ok(c) => c,
        Err(_) => Config::infer().await?,
    };
    config.read_timeout = Some(Duration::from_secs(DEFAULT_READ_TIMEOUT_SECS));
    config.write_timeout = Some(Duration::from_secs(DEFAULT_WRITE_TIMEOUT_SECS));
    Ok(Client::try_from(config)?)
}
