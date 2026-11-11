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

use std::collections::BTreeMap;

use async_trait::async_trait;
use banlieue_api::banlieue::ProviderConnection;
use banlieue_libvirt::{
    DEFAULT_TLS_PORT, Network, Session, StoragePool, TlsIdentity, connect_open, connect_tls,
    list_all_networks, list_all_storage_pools, node_get_info, node_get_memory_stats,
    node_memory_available_mib, storage_pool_get_info, storage_pool_list_all_volumes,
    storage_pool_refresh,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::warn;

use crate::error::{Error, Result};

/// The libvirt driver URI the daemon serves locally.
///
/// Distinct from the `qemu+tls://host/system` endpoint used to *reach* the
/// host: that describes the transport, this names the driver.
pub const LOCAL_DRIVER_URI: &str = "qemu:///system";

/// What the reconcilers need from a libvirt host.
#[async_trait]
pub trait LibvirtClient: Send + Sync {
    /// Every storage pool, running or merely defined.
    async fn list_pools(&self) -> Result<Vec<StoragePool>>;
    /// Every network, running or merely defined.
    async fn list_networks(&self) -> Result<Vec<Network>>;
    /// The names of the volumes in `pool`.
    ///
    /// # Errors
    /// [`Error::Libvirt`] when the host has no such pool, or could not list
    /// it (an inactive pool cannot be listed).
    async fn volumes(&self, pool: &str) -> Result<Vec<String>>;
    /// What the host has for guests (ADR-0128). Best effort: a figure the
    /// host would not give is `None`, never an error.
    async fn host_capacity(&self) -> Result<HostCapacity>;
}

/// A host's capacity as libvirt reports it (ADR-0128).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HostCapacity {
    /// Active logical CPUs (`virNodeGetInfo`).
    pub cpus: Option<u32>,
    /// Host memory, MiB (`virNodeGetInfo`).
    pub memory_mib: Option<u64>,
    /// Free plus reclaimable memory, MiB (`virNodeGetMemoryStats`).
    pub available_mib: Option<u64>,
    /// Each pool's size and free space, by pool name, after a refresh.
    pub pools: BTreeMap<String, PoolSpace>,
}

/// One storage pool's size and free space (`virStoragePoolGetInfo`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PoolSpace {
    /// Size, bytes.
    pub capacity_bytes: u64,
    /// Free for new volumes, bytes.
    pub available_bytes: u64,
}

/// KiB in one MiB, for `virNodeGetInfo`'s memory figure.
const KIB_PER_MIB: u64 = 1024;

/// Read the host's capacity on an open session. Every call is read-only
/// (the pool refresh re-scans, it changes nothing) and best effort: a
/// failure is logged and leaves that figure out, so capacity never blocks
/// the rest of the status.
async fn read_host_capacity<S>(session: &mut Session<S>, pools: &[StoragePool]) -> HostCapacity
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut capacity = HostCapacity::default();
    match node_get_info(session).await {
        Ok(info) => {
            capacity.cpus = Some(info.cpus).filter(|c| *c > 0);
            capacity.memory_mib = Some(info.memory_kib / KIB_PER_MIB);
        }
        Err(e) => warn!(error = %e, "node info unreadable; no CPU or memory figure"),
    }
    match node_get_memory_stats(session).await {
        Ok(stats) => capacity.available_mib = node_memory_available_mib(&stats),
        Err(e) => warn!(error = %e, "node memory stats unreadable; no free-memory figure"),
    }
    for pool in pools {
        if let Err(e) = storage_pool_refresh(session, pool).await {
            warn!(pool = %pool.name, error = %e, "pool refresh failed; no space figure");
            continue;
        }
        match storage_pool_get_info(session, pool).await {
            Ok(info) => {
                capacity.pools.insert(
                    pool.name.clone(),
                    PoolSpace {
                        capacity_bytes: info.capacity_bytes,
                        available_bytes: info.available_bytes,
                    },
                );
            }
            Err(e) => warn!(pool = %pool.name, error = %e, "pool info unreadable; no space figure"),
        }
    }
    capacity
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
///
/// Stateless by design (ADR-0095): every reconcile builds a fresh client from
/// credentials it has just read, so a rotated Secret is used by the next one.
/// Any connection reuse must key on the Secret's `resourceVersion`.
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
        // One more call per pool, so a `BackingFile` image is reported ready
        // only where its volume really is. A pool that cannot be listed (not
        // running) is remembered as such, not as empty.
        let mut volumes = BTreeMap::new();
        for pool in &pools {
            let listed = storage_pool_list_all_volumes(&mut session, pool)
                .await
                .map(|vols| vols.into_iter().map(|v| v.name).collect())
                .map_err(|e| e.to_string());
            volumes.insert(pool.name.clone(), listed);
        }
        let capacity = read_host_capacity(&mut session, &pools).await;
        Ok(Box::new(SnapshotClient {
            pools,
            networks,
            volumes,
            capacity,
        }))
    }
}

/// A client backed by one session's worth of already-fetched inventory.
#[derive(Debug, Clone)]
struct SnapshotClient {
    pools: Vec<StoragePool>,
    networks: Vec<Network>,
    /// Each pool's volume names, or why they could not be listed.
    volumes: BTreeMap<String, std::result::Result<Vec<String>, String>>,
    /// What the host has for guests.
    capacity: HostCapacity,
}

#[async_trait]
impl LibvirtClient for SnapshotClient {
    async fn list_pools(&self) -> Result<Vec<StoragePool>> {
        Ok(self.pools.clone())
    }
    async fn list_networks(&self) -> Result<Vec<Network>> {
        Ok(self.networks.clone())
    }
    async fn volumes(&self, pool: &str) -> Result<Vec<String>> {
        match self.volumes.get(pool) {
            Some(Ok(names)) => Ok(names.clone()),
            Some(Err(why)) => Err(Error::Libvirt(format!("listing pool {pool}: {why}"))),
            None => Err(Error::Libvirt(format!("no storage pool named {pool}"))),
        }
    }
    async fn host_capacity(&self) -> Result<HostCapacity> {
        Ok(self.capacity.clone())
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
    /// Volume names per pool; a pool absent here has none.
    pub volumes: BTreeMap<String, Vec<String>>,
    /// What the fake host reports for guests; empty by default.
    pub capacity: HostCapacity,
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
            volumes: BTreeMap::new(),
            capacity: HostCapacity::default(),
        }
    }

    /// The same fake host, with `volume` in `pool`.
    #[must_use]
    pub fn with_volume(mut self, pool: &str, volume: &str) -> Self {
        self.volumes
            .entry(pool.to_string())
            .or_default()
            .push(volume.to_string());
        self
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
    async fn volumes(&self, pool: &str) -> Result<Vec<String>> {
        if let Some(m) = &self.fail_with {
            return Err(Error::Libvirt(m.clone()));
        }
        // As a host does: an unknown pool is an error, not an empty list.
        if !self.pools.iter().any(|p| p.name == pool) {
            return Err(Error::Libvirt(format!("no storage pool named {pool}")));
        }
        Ok(self.volumes.get(pool).cloned().unwrap_or_default())
    }
    async fn host_capacity(&self) -> Result<HostCapacity> {
        match &self.fail_with {
            Some(m) => Err(Error::Libvirt(m.clone())),
            None => Ok(self.capacity.clone()),
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
