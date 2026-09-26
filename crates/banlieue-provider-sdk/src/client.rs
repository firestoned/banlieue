// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Kubernetes client construction with timeouts.
//!
//! Controllers should always build their [`kube::Client`] through this module
//! so timeouts and config-source semantics stay consistent across binaries.

use std::time::Duration;

use std::ffi::OsStr;

use kube::config::{KubeConfigOptions, Kubeconfig};
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
    build_client_with(None).await
}

/// [`build_client`], but an explicit `kubeconfig` wins over every other
/// source, in-cluster config included. `kubeconfig` may be one path or a
/// list in the platform's path-list syntax (`KUBECONFIG`'s), merged as
/// kubectl merges them.
///
/// This is what a `--kubeconfig` flag must call: before it existed the flag
/// was parsed and then ignored, and only the `KUBECONFIG` environment
/// variable (read by inference) had any effect.
///
/// # Errors
/// As [`build_client`], plus [`Error::KubeConfig`](crate::Error::KubeConfig)
/// if an explicit kubeconfig cannot be read or parsed.
pub async fn build_client_with(kubeconfig: Option<&OsStr>) -> Result<Client> {
    let config = match kubeconfig {
        Some(paths) => config_from_kubeconfig(paths).await?,
        None => {
            let mut config = match Config::incluster() {
                Ok(c) => c,
                Err(_) => Config::infer().await?,
            };
            with_timeouts(&mut config);
            config
        }
    };
    Ok(Client::try_from(config)?)
}

/// A [`Config`] from an explicit kubeconfig path or path list, with the
/// standard timeouts. A missing file is an error, never a silent fall-back
/// to another source.
///
/// # Errors
/// [`Error::KubeConfig`](crate::Error::KubeConfig) if a file cannot be read
/// or parsed, or names no usable context.
pub async fn config_from_kubeconfig(paths: &OsStr) -> Result<Config> {
    let mut merged: Option<Kubeconfig> = None;
    for path in std::env::split_paths(paths) {
        let next = Kubeconfig::read_from(&path)?;
        merged = Some(match merged {
            None => next,
            Some(first) => first.merge(next)?,
        });
    }
    let kubeconfig = merged.ok_or_else(|| {
        kube::config::KubeconfigError::ReadConfig(
            std::io::Error::new(std::io::ErrorKind::NotFound, "empty kubeconfig path"),
            std::path::PathBuf::new(),
        )
    })?;
    let mut config =
        Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default()).await?;
    with_timeouts(&mut config);
    Ok(config)
}

/// Timeouts so a stuck apiserver cannot hang reconciliation forever.
fn with_timeouts(config: &mut Config) {
    config.read_timeout = Some(Duration::from_secs(DEFAULT_READ_TIMEOUT_SECS));
    config.write_timeout = Some(Duration::from_secs(DEFAULT_WRITE_TIMEOUT_SECS));
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
