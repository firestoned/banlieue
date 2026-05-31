// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! vSphere client surface used by the reconcilers.
//!
//! The reconcilers depend only on the [`VSphereClient`] trait so they can be
//! unit-tested with [`FakeClient`] without compiling against `vim_rs`. The
//! real implementation in [`vim`] wraps `vim_rs::core::client::ClientBuilder`.

use async_trait::async_trait;
use banlieue_api::banlieue::ProviderConnection;

use crate::error::Result;

pub mod fake;
pub mod vim;

pub use fake::{FakeClient, FakeClientFactory, Inventory, InventoryBuilder};
pub use vim::VimClientFactory;

// `Template` is re-exported via the module path `crate::client::Template`
// (declared above) — listed here as an anchor so future readers see the
// full surface in one place.

/// Slim local projection of a vCenter Datacenter. The full vim_rs type carries
/// many fields we don't need and isn't `Clone`/`Eq`; projecting at the
/// boundary keeps the reconciler types small and testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datacenter {
    /// Display name (e.g. `dc-east`).
    pub name: String,
    /// vCenter managed-object reference (e.g. `datacenter-2`). Opaque to us.
    pub moref: String,
}

/// Slim local projection of a vCenter Cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    /// Display name (e.g. `cluster-prod`).
    pub name: String,
    /// vCenter managed-object reference (e.g. `domain-c10`). Opaque to us.
    pub moref: String,
    /// `moref` of the Datacenter this cluster belongs to.
    pub datacenter_moref: String,
}

/// Slim local projection of a vCenter VM template (a VirtualMachine MO
/// with `config.template == true`). Iteration 2a only needs name + moref +
/// containing datacenter for the [`VSphereClient::find_template`] lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    /// Display name (e.g. `ubuntu-22.04-cloudinit`).
    pub name: String,
    /// vCenter managed-object reference (e.g. `vm-101`). Opaque.
    pub moref: String,
    /// `moref` of the Datacenter this template lives in.
    pub datacenter_moref: String,
}

/// Backend-agnostic credential bundle resolved from the Provider's
/// `credentialsRef` Secret. Plain strings — interpreted by the factory.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// Construct a [`VSphereClient`] from a Provider connection spec + creds.
///
/// Implemented twice:
///
/// - [`VimClientFactory`] (production) — uses `vim_rs` to log into vCenter.
/// - [`FakeClientFactory`] (tests) — returns a [`FakeClient`] driven by
///   pre-seeded fixtures.
#[async_trait]
pub trait VSphereClientFactory: Send + Sync {
    /// Build a client by connecting to `connection.endpoint` with `creds`.
    /// `ca_bundle` / `insecure_skip_tls_verify` are taken from `connection`.
    async fn build(
        &self,
        connection: &ProviderConnection,
        creds: &Credentials,
    ) -> Result<Box<dyn VSphereClient>>;
}

/// A connected vSphere client. The reconciler only uses what's on this trait,
/// so the production wrapper around `vim_rs` and the in-memory fake share an
/// interface.
#[async_trait]
pub trait VSphereClient: Send + Sync {
    /// All datacenters reachable under the vCenter root folder.
    async fn list_datacenters(&self) -> Result<Vec<Datacenter>>;

    /// All compute clusters under `dc`.
    async fn list_clusters(&self, dc: &Datacenter) -> Result<Vec<Cluster>>;

    /// Find a VM template by display name within `dc`. Returns `None` when
    /// no template with that name exists; returns `Err` when the lookup
    /// itself fails (auth / network).
    async fn find_template(&self, dc: &Datacenter, name: &str) -> Result<Option<Template>>;
}
