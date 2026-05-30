// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! In-memory `VSphereClient` used by reconciler tests.
//!
//! The fake holds a pre-seeded inventory and answers `list_*` calls from it.
//! No vim_rs, no tokio I/O, no network — tests stay fast and deterministic.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use banlieue_api::banlieue::ProviderConnection;

use crate::error::Result;

use super::{Cluster, Credentials, Datacenter, Template, VSphereClient, VSphereClientFactory};

/// Synthetic inventory used by [`FakeClient`] tests. Build with [`Inventory::builder`].
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub datacenters: Vec<Datacenter>,
    /// Clusters grouped by `Datacenter.moref`.
    pub clusters_by_dc: HashMap<String, Vec<Cluster>>,
    /// Templates grouped by `Datacenter.moref`.
    pub templates_by_dc: HashMap<String, Vec<Template>>,
}

impl Inventory {
    pub fn builder() -> InventoryBuilder {
        InventoryBuilder::default()
    }
}

/// Ergonomic builder so tests read like a sentence: `with_dc("dc").with_cluster("dc", "c1")`.
#[derive(Debug, Default)]
pub struct InventoryBuilder {
    inv: Inventory,
}

impl InventoryBuilder {
    pub fn with_dc(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        let moref = format!("datacenter-{}", name);
        self.inv.datacenters.push(Datacenter { name, moref });
        self
    }

    pub fn with_cluster(mut self, dc_name: &str, cluster_name: impl Into<String>) -> Self {
        let dc_moref = self.lookup_dc(dc_name);
        let cluster_name = cluster_name.into();
        let cluster = Cluster {
            moref: format!("domain-c-{}-{}", dc_name, cluster_name),
            datacenter_moref: dc_moref.clone(),
            name: cluster_name,
        };
        self.inv
            .clusters_by_dc
            .entry(dc_moref)
            .or_default()
            .push(cluster);
        self
    }

    pub fn with_template(mut self, dc_name: &str, template_name: impl Into<String>) -> Self {
        let dc_moref = self.lookup_dc(dc_name);
        let template_name = template_name.into();
        let template = Template {
            moref: format!("vm-template-{}-{}", dc_name, template_name),
            datacenter_moref: dc_moref.clone(),
            name: template_name,
        };
        self.inv
            .templates_by_dc
            .entry(dc_moref)
            .or_default()
            .push(template);
        self
    }

    pub fn build(self) -> Inventory {
        self.inv
    }

    fn lookup_dc(&self, dc_name: &str) -> String {
        self.inv
            .datacenters
            .iter()
            .find(|d| d.name == dc_name)
            .map(|d| d.moref.clone())
            .unwrap_or_else(|| {
                panic!("datacenter {dc_name:?} not seeded — call .with_dc(...) first")
            })
    }
}

/// Factory that hands out [`FakeClient`]s backed by a shared [`Inventory`].
#[derive(Clone)]
pub struct FakeClientFactory {
    inventory: Arc<Inventory>,
}

impl FakeClientFactory {
    pub fn new(inventory: Inventory) -> Self {
        Self {
            inventory: Arc::new(inventory),
        }
    }
}

#[async_trait]
impl VSphereClientFactory for FakeClientFactory {
    async fn build(
        &self,
        _connection: &ProviderConnection,
        _creds: &Credentials,
    ) -> Result<Box<dyn VSphereClient>> {
        Ok(Box::new(FakeClient {
            inventory: self.inventory.clone(),
        }))
    }
}

/// In-memory client. Returns whatever the seeded [`Inventory`] says.
pub struct FakeClient {
    inventory: Arc<Inventory>,
}

impl FakeClient {
    /// Direct constructor — useful in unit tests that want to skip the
    /// factory indirection and call reconciler helpers that take a
    /// `&dyn VSphereClient` parameter.
    pub fn new(inventory: Inventory) -> Self {
        Self {
            inventory: Arc::new(inventory),
        }
    }
}

#[async_trait]
impl VSphereClient for FakeClient {
    async fn list_datacenters(&self) -> Result<Vec<Datacenter>> {
        Ok(self.inventory.datacenters.clone())
    }

    async fn list_clusters(&self, dc: &Datacenter) -> Result<Vec<Cluster>> {
        Ok(self
            .inventory
            .clusters_by_dc
            .get(&dc.moref)
            .cloned()
            .unwrap_or_default())
    }

    async fn find_template(&self, dc: &Datacenter, name: &str) -> Result<Option<Template>> {
        Ok(self
            .inventory
            .templates_by_dc
            .get(&dc.moref)
            .and_then(|tpls| tpls.iter().find(|t| t.name == name).cloned()))
    }
}
