// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The Proxmox-facing seam.
//!
//! [`ProxmoxClientFactory`] builds an `Arc<dyn ProxmoxApi>` from a
//! `Provider`'s connection details, so tests inject
//! [`banlieue_proxmox::FakeProxmox`] and exercise the whole reconcile path
//! with no Proxmox node, no TLS and no network — the same pattern the sibling
//! providers use.

use std::sync::Arc;

use async_trait::async_trait;
use banlieue_api::banlieue::ProviderConnection;
use banlieue_proxmox::{ApiToken, Client, ClientConfig, ProxmoxApi};

use crate::error::Result;

/// What a Provider's credentials Secret and `caBundle` resolve to.
#[derive(Clone, Debug)]
pub struct Credentials {
    /// The API token (`user@realm!id` + secret). Redacts itself in `Debug`.
    pub token: ApiToken,
    /// PEM CA bundle to trust in addition to the system roots, if the
    /// Provider names one.
    pub ca_pem: Option<String>,
}

/// Builds Proxmox API clients.
#[async_trait]
pub trait ProxmoxClientFactory: Send + Sync {
    /// Build a client for `connection` using `credentials`.
    ///
    /// # Errors
    /// [`crate::Error::Proxmox`] for a bad endpoint or CA bundle.
    async fn build(
        &self,
        connection: &ProviderConnection,
        credentials: &Credentials,
    ) -> Result<Arc<dyn ProxmoxApi>>;
}

/// Install the process-default rustls crypto provider (ring), which reqwest's
/// `rustls-no-provider` needs before its first TLS use (ADR-0009). Idempotent:
/// an already-installed provider is not an error.
pub fn install_default_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// The real factory: HTTPS with an API token.
///
/// Stateless by design (ADR-0095): every reconcile builds a fresh client from
/// credentials it has just read, so a rotated Secret is used by the next one.
/// Any connection reuse must key on the Secret's `resourceVersion`.
#[derive(Debug, Default, Clone, Copy)]
pub struct HttpClientFactory;

#[async_trait]
impl ProxmoxClientFactory for HttpClientFactory {
    async fn build(
        &self,
        connection: &ProviderConnection,
        credentials: &Credentials,
    ) -> Result<Arc<dyn ProxmoxApi>> {
        let mut cfg = ClientConfig::new(&connection.endpoint, credentials.token.clone());
        cfg.ca_bundle_pem.clone_from(&credentials.ca_pem);
        cfg.insecure_skip_tls_verify = connection.insecure_skip_tls_verify;
        Ok(Arc::new(Client::new(cfg)?))
    }
}

/// A factory that always returns the API it was given: for tests and for
/// running the reconcilers against `FakeProxmox`.
#[derive(Clone)]
pub struct StaticClientFactory {
    api: Arc<dyn ProxmoxApi>,
}

impl StaticClientFactory {
    /// Wrap `api`.
    pub fn new(api: Arc<dyn ProxmoxApi>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl ProxmoxClientFactory for StaticClientFactory {
    async fn build(
        &self,
        _connection: &ProviderConnection,
        _credentials: &Credentials,
    ) -> Result<Arc<dyn ProxmoxApi>> {
        Ok(self.api.clone())
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
