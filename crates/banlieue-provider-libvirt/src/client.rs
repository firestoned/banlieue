// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The libvirt-facing seam.
//!
//! [`LibvirtClient`] is the narrow surface the reconcilers need, and
//! [`LibvirtClientFactory`] builds one from a `Provider`'s connection details.
//! Both are traits so tests can inject [`FakeClient`] and exercise the whole
//! reconcile path with no libvirtd, no TLS, and no network — the same pattern
//! `banlieue-provider-vsphere` uses.
//!
//! The real implementation is a thin wrapper over `banlieue-libvirt`; all the
//! protocol lives there (ADR-0011).

use async_trait::async_trait;
use banlieue_api::banlieue::ProviderConnection;
use banlieue_libvirt::{
    DEFAULT_TLS_PORT, Network, StoragePool, TlsIdentity, connect_open, connect_tls,
    list_all_networks, list_all_storage_pools,
};

use crate::error::{Error, Result};

/// The libvirt driver URI the daemon serves locally.
///
/// Distinct from the `qemu+tls://host/system` endpoint used to *reach* the
/// host: that describes the transport, this names the driver.
pub(crate) const LOCAL_DRIVER_URI: &str = "qemu:///system";

/// What the reconcilers need from a libvirt host.
#[async_trait]
pub trait LibvirtClient: Send + Sync {
    /// Every storage pool, running or merely defined.
    async fn list_pools(&self) -> Result<Vec<StoragePool>>;
    /// Every network, running or merely defined.
    async fn list_networks(&self) -> Result<Vec<Network>>;
}

/// Builds a [`LibvirtClient`] from a `Provider`'s connection details.
#[async_trait]
pub trait LibvirtClientFactory: Send + Sync {
    /// Connect and open a session.
    ///
    /// # Errors
    /// [`Error::Libvirt`] on transport, TLS, or protocol failure.
    async fn build(
        &self,
        connection: &ProviderConnection,
        identity: &TlsIdentity,
    ) -> Result<Box<dyn LibvirtClient>>;
}

/// Split a libvirt endpoint into host and port.
///
/// Accepts `qemu+tls://host[:port]/system`, or a bare `host[:port]`. Any other
/// scheme is rejected: this provider supports mutual TLS only, and silently
/// treating `qemu+ssh://` as TLS would fail far away from the cause.
pub fn parse_endpoint(endpoint: &str) -> Result<(String, u16)> {
    let rest = match endpoint.split_once("://") {
        Some(("qemu+tls", rest)) => rest,
        Some((scheme, _)) => {
            return Err(Error::Invalid {
                what: "connection.endpoint",
                detail: format!(
                    "scheme {scheme:?} is not supported; banlieue speaks libvirt over \
                     mutual TLS only (qemu+tls://host/system)."
                ),
            });
        }
        None => endpoint,
    };
    // Drop any path component (`/system`).
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() {
        return Err(Error::Invalid {
            what: "connection.endpoint",
            detail: format!("no host in {endpoint:?}"),
        });
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => {
            let port = port.parse::<u16>().map_err(|_| Error::Invalid {
                what: "connection.endpoint",
                detail: format!("port {port:?} in {endpoint:?} is not a number"),
            })?;
            Ok((host.to_string(), port))
        }
        None => Ok((authority.to_string(), DEFAULT_TLS_PORT)),
    }
}

/// The production factory: connects over mutual TLS and opens the session.
#[derive(Debug, Default, Clone)]
pub struct TlsClientFactory;

impl TlsClientFactory {
    /// Construct.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl LibvirtClientFactory for TlsClientFactory {
    async fn build(
        &self,
        connection: &ProviderConnection,
        identity: &TlsIdentity,
    ) -> Result<Box<dyn LibvirtClient>> {
        let (host, port) = parse_endpoint(&connection.endpoint)?;
        let mut session = connect_tls(&host, port, identity).await?;
        // AUTH_LIST then CONNECT_OPEN; see banlieue-libvirt's connect_open for
        // why skipping the former hangs rather than errors.
        connect_open(&mut session, Some(LOCAL_DRIVER_URI), false).await?;

        // Everything the reconcilers need is read up-front on one session: the
        // control plane is two cheap list calls, so there is no reason to hold
        // a connection open across reconciles.
        let pools = list_all_storage_pools(&mut session).await?;
        let networks = list_all_networks(&mut session).await?;
        Ok(Box::new(SnapshotClient { pools, networks }))
    }
}

/// A client backed by one session's worth of already-fetched inventory.
#[derive(Debug, Clone)]
struct SnapshotClient {
    pools: Vec<StoragePool>,
    networks: Vec<Network>,
}

#[async_trait]
impl LibvirtClient for SnapshotClient {
    async fn list_pools(&self) -> Result<Vec<StoragePool>> {
        Ok(self.pools.clone())
    }
    async fn list_networks(&self) -> Result<Vec<Network>> {
        Ok(self.networks.clone())
    }
}

/// In-memory client for tests.
#[derive(Debug, Default, Clone)]
pub struct FakeClient {
    /// Pools the fake host reports.
    pub pools: Vec<StoragePool>,
    /// Networks the fake host reports.
    pub networks: Vec<Network>,
    /// When set, every call fails with this message.
    pub fail_with: Option<String>,
}

impl FakeClient {
    /// A fake host exposing the named pools and networks.
    pub fn with(pools: &[&str], networks: &[&str]) -> Self {
        let mk = |names: &[&str], seed: u8| {
            names
                .iter()
                .enumerate()
                .map(|(i, n)| (n.to_string(), [seed.wrapping_add(i as u8); 16]))
                .collect::<Vec<_>>()
        };
        Self {
            pools: mk(pools, 1)
                .into_iter()
                .map(|(name, uuid)| StoragePool { name, uuid })
                .collect(),
            networks: mk(networks, 100)
                .into_iter()
                .map(|(name, uuid)| Network { name, uuid })
                .collect(),
            fail_with: None,
        }
    }

    /// A fake host that fails every call.
    pub fn failing(message: &str) -> Self {
        Self {
            fail_with: Some(message.to_string()),
            ..Self::default()
        }
    }
}

#[async_trait]
impl LibvirtClient for FakeClient {
    async fn list_pools(&self) -> Result<Vec<StoragePool>> {
        match &self.fail_with {
            Some(m) => Err(Error::Libvirt(m.clone())),
            None => Ok(self.pools.clone()),
        }
    }
    async fn list_networks(&self) -> Result<Vec<Network>> {
        match &self.fail_with {
            Some(m) => Err(Error::Libvirt(m.clone())),
            None => Ok(self.networks.clone()),
        }
    }
}

/// Factory returning a preset [`FakeClient`].
#[derive(Debug, Clone)]
pub struct FakeClientFactory {
    client: FakeClient,
}

impl FakeClientFactory {
    /// Wrap a fake client.
    pub fn new(client: FakeClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl LibvirtClientFactory for FakeClientFactory {
    async fn build(
        &self,
        _connection: &ProviderConnection,
        _identity: &TlsIdentity,
    ) -> Result<Box<dyn LibvirtClient>> {
        if let Some(m) = &self.client.fail_with {
            return Err(Error::Libvirt(m.clone()));
        }
        Ok(Box::new(self.client.clone()))
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
