// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! In-memory `VSphereClient` used by reconciler tests.
//!
//! The fake holds a pre-seeded inventory and answers `list_*` calls from it.
//! No vim_rs, no tokio I/O, no network — tests stay fast and deterministic.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use banlieue_api::banlieue::ProviderConnection;
use banlieue_api::common::PowerState;

use crate::error::{Error, Result};

use super::{
    CloneVmRequest, Cluster, Credentials, Datacenter, Datastore, Network, Template, VSphereClient,
    VSphereClientFactory,
};

/// One recorded [`VSphereClient::clone_vm`] call, kept so reconciler tests
/// can assert what was requested without a real vCenter. See
/// [`FakeClient::cloned_vms`].
#[derive(Debug, Clone)]
pub struct ClonedVm {
    pub request: CloneVmRequest,
    pub moref: String,
}

/// Synthetic inventory used by [`FakeClient`] tests. Build with [`Inventory::builder`].
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub datacenters: Vec<Datacenter>,
    /// Clusters grouped by `Datacenter.moref`.
    pub clusters_by_dc: HashMap<String, Vec<Cluster>>,
    /// Templates grouped by `Datacenter.moref` — matched by
    /// [`VSphereClient::find_template`] when called with `folder: None`
    /// (a `Template`-kind image, no per-zone folder).
    pub templates_by_dc: HashMap<String, Vec<Template>>,
    /// Templates grouped by folder path (e.g. `templates/cluster-01`) —
    /// matched when `find_template` is called with `folder: Some(_)` (a
    /// per-zone `Url`-kind import, ADR-0020 Decision #5). Kept separate
    /// from `templates_by_dc` so a test can seed two zones' identically-
    /// named templates in different folders and assert the *correct*
    /// one is found — the exact bug this folder-scoping fixes.
    pub templates_by_folder: HashMap<String, Vec<Template>>,
    /// Datastores reachable from a cluster, grouped by `Cluster.moref`.
    pub datastores_by_cluster: HashMap<String, Vec<Datastore>>,
    /// Networks reachable from a cluster, grouped by `Cluster.moref`.
    pub networks_by_cluster: HashMap<String, Vec<Network>>,
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

    /// Seed a template in a specific folder path (e.g. `templates/cluster-01`)
    /// — for [`VSphereClient::find_template`]'s `folder: Some(_)` path
    /// (a per-zone `Url`-kind import). Independent of [`Self::with_template`]
    /// (datacenter-wide, `folder: None`) — a test can seed both to prove a
    /// folder-scoped lookup doesn't fall back to a datacenter-wide match.
    pub fn with_template_in_folder(
        mut self,
        dc_name: &str,
        folder: &str,
        template_name: impl Into<String>,
    ) -> Self {
        let dc_moref = self.lookup_dc(dc_name);
        let template_name = template_name.into();
        let template = Template {
            moref: format!("vm-template-{folder}-{template_name}"),
            datacenter_moref: dc_moref,
            name: template_name,
        };
        self.inv
            .templates_by_folder
            .entry(folder.to_string())
            .or_default()
            .push(template);
        self
    }

    /// Seed a datastore reachable from `(dc_name, cluster_name)`. Pass
    /// `datastore_cluster` to mark it a member of that SDRS datastore cluster.
    pub fn with_datastore(
        mut self,
        dc_name: &str,
        cluster_name: &str,
        ds_name: impl Into<String>,
        datastore_cluster: Option<&str>,
    ) -> Self {
        let cmoref = self.lookup_cluster(dc_name, cluster_name);
        let ds_name = ds_name.into();
        let ds = Datastore {
            moref: format!("datastore-{cluster_name}-{ds_name}"),
            name: ds_name,
            datastore_cluster: datastore_cluster.map(str::to_string),
            free_space_bytes: None,
        };
        self.inv
            .datastores_by_cluster
            .entry(cmoref)
            .or_default()
            .push(ds);
        self
    }

    /// Seed a network reachable from `(dc_name, cluster_name)`. `distributed`
    /// marks it a distributed virtual port group (vs a standard port group).
    pub fn with_network(
        mut self,
        dc_name: &str,
        cluster_name: &str,
        net_name: impl Into<String>,
        distributed: bool,
    ) -> Self {
        let cmoref = self.lookup_cluster(dc_name, cluster_name);
        let net_name = net_name.into();
        let net = Network {
            moref: format!("network-{cluster_name}-{net_name}"),
            name: net_name,
            distributed,
        };
        self.inv
            .networks_by_cluster
            .entry(cmoref)
            .or_default()
            .push(net);
        self
    }

    pub fn build(self) -> Inventory {
        self.inv
    }

    fn lookup_cluster(&self, dc_name: &str, cluster_name: &str) -> String {
        let dc_moref = self.lookup_dc(dc_name);
        self.inv
            .clusters_by_dc
            .get(&dc_moref)
            .and_then(|cs| cs.iter().find(|c| c.name == cluster_name))
            .map(|c| c.moref.clone())
            .unwrap_or_else(|| {
                panic!("cluster {cluster_name:?} in {dc_name:?} not seeded — call .with_cluster(...) first")
            })
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
        _ca_bundle_pem: Option<&str>,
    ) -> Result<Box<dyn VSphereClient>> {
        Ok(Box::new(FakeClient::new((*self.inventory).clone())))
    }
}

/// In-memory client. Returns whatever the seeded [`Inventory`] says.
/// `clone_vm`/`set_power_state` calls are recorded (behind a `Mutex`, since
/// `VSphereClient` methods take `&self`) rather than mutating any real
/// backend — see [`ClonedVm`] and [`FakeClient::power_state_of`].
pub struct FakeClient {
    inventory: Arc<Inventory>,
    clones: Mutex<Vec<ClonedVm>>,
    power_states: Mutex<HashMap<String, PowerState>>,
    destroyed: Mutex<Vec<String>>,
}

impl FakeClient {
    /// Direct constructor — useful in unit tests that want to skip the
    /// factory indirection and call reconciler helpers that take a
    /// `&dyn VSphereClient` parameter.
    pub fn new(inventory: Inventory) -> Self {
        Self {
            inventory: Arc::new(inventory),
            clones: Mutex::new(Vec::new()),
            power_states: Mutex::new(HashMap::new()),
            destroyed: Mutex::new(Vec::new()),
        }
    }

    /// Every `clone_vm` call recorded so far, in call order.
    pub fn cloned_vms(&self) -> Vec<ClonedVm> {
        self.clones.lock().expect("fake client lock").clone()
    }

    /// The last power state driven onto `vm_moref` via `set_power_state`
    /// (or the state `clone_vm` set at creation — always `PoweredOff`),
    /// `None` if neither has ever been called for that moref.
    pub fn power_state_of(&self, vm_moref: &str) -> Option<PowerState> {
        self.power_states
            .lock()
            .expect("fake client lock")
            .get(vm_moref)
            .cloned()
    }

    /// Every moref `destroy_vm` was called with, in call order (ADR-0026).
    pub fn destroyed_vms(&self) -> Vec<String> {
        self.destroyed.lock().expect("fake client lock").clone()
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

    async fn find_template(
        &self,
        dc: &Datacenter,
        folder: Option<&str>,
        name: &str,
    ) -> Result<Option<Template>> {
        let templates = match folder {
            Some(f) if !f.is_empty() => self.inventory.templates_by_folder.get(f),
            _ => self.inventory.templates_by_dc.get(&dc.moref),
        };
        Ok(templates.and_then(|tpls| tpls.iter().find(|t| t.name == name).cloned()))
    }

    async fn list_datastores(&self, cluster: &Cluster) -> Result<Vec<Datastore>> {
        Ok(self
            .inventory
            .datastores_by_cluster
            .get(&cluster.moref)
            .cloned()
            .unwrap_or_default())
    }

    async fn list_networks(&self, cluster: &Cluster) -> Result<Vec<Network>> {
        Ok(self
            .inventory
            .networks_by_cluster
            .get(&cluster.moref)
            .cloned()
            .unwrap_or_default())
    }

    async fn import_iso_template(&self, req: &crate::client::IsoImportRequest) -> Result<String> {
        // No vCenter to mutate: return the reference the real client would
        // resolve to, so import-subcommand tests can assert the plan without a
        // backend.
        Ok(format!("[{}] {}", req.datastore, req.template_name))
    }

    async fn ensure_datastore_dir(
        &self,
        _datacenter_moref: &str,
        _datastore: &str,
        _dir: &str,
    ) -> Result<()> {
        Ok(())
    }

    async fn destroy_if_present(
        &self,
        _datacenter_moref: &str,
        _folder: &str,
        _name: &str,
    ) -> Result<()> {
        Ok(())
    }

    async fn clone_vm(&self, req: &CloneVmRequest) -> Result<String> {
        let moref = format!("vm-clone-{}", req.vm_name);
        self.clones
            .lock()
            .expect("fake client lock")
            .push(ClonedVm {
                request: req.clone(),
                moref: moref.clone(),
            });
        // Real clone_vm always clones powered off (ADR-0024) — mirror that
        // so a test asserting power state before any set_power_state call
        // sees the same thing the real client would report.
        self.power_states
            .lock()
            .expect("fake client lock")
            .insert(moref.clone(), PowerState::PoweredOff);
        Ok(moref)
    }

    async fn set_power_state(&self, vm_moref: &str, desired: PowerState) -> Result<()> {
        self.power_states
            .lock()
            .expect("fake client lock")
            .insert(vm_moref.to_string(), desired);
        Ok(())
    }

    async fn power_state(&self, vm_moref: &str) -> Result<PowerState> {
        self.power_state_of(vm_moref)
            .ok_or_else(|| Error::Vsphere(format!("{vm_moref}: no fake power state recorded")))
    }

    async fn destroy_vm(&self, vm_moref: &str) -> Result<()> {
        self.destroyed
            .lock()
            .expect("fake client lock")
            .push(vm_moref.to_string());
        self.power_states
            .lock()
            .expect("fake client lock")
            .remove(vm_moref);
        Ok(())
    }
}
