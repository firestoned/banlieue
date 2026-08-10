// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! vSphere client surface used by the reconcilers.
//!
//! The reconcilers depend only on the [`VSphereClient`] trait so they can be
//! unit-tested with [`FakeClient`] without compiling against `vim_rs`. The
//! real implementation in [`vim`] wraps `vim_rs::core::client::ClientBuilder`.

use async_trait::async_trait;
use banlieue_api::banlieue::{DiskController, NicAdapter, ProviderConnection};
use banlieue_api::common::{DiskProvisioning, Firmware};

use crate::error::Result;

pub mod fake;
pub mod vim;

pub use fake::{FakeClient, FakeClientFactory, Inventory, InventoryBuilder};
pub use vim::{VimClientFactory, install_default_crypto_provider};

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

/// Slim local projection of a vCenter Datastore reachable from a cluster.
/// `datastore_cluster` is the name of the containing SDRS datastore cluster
/// (`StoragePod`) when the datastore belongs to one, else `None` — that is how
/// a `storageClasses[].target.datastoreCluster` mapping is matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datastore {
    /// Display name (e.g. `ds-fast-01`).
    pub name: String,
    /// vCenter managed-object reference (e.g. `datastore-42`). Opaque.
    pub moref: String,
    /// Name of the containing SDRS datastore cluster, if any.
    pub datastore_cluster: Option<String>,
    /// Free space in bytes (`summary.freeSpace`), when known. Used to pick the
    /// emptiest member when a datastore-cluster is the import target (ADR-0020).
    pub free_space_bytes: Option<i64>,
}

/// Slim local projection of a vCenter network reachable from a cluster — a
/// standard port group or a distributed virtual port group. `distributed`
/// selects which of `target.portGroup` / `target.distributedPortGroup` a
/// `networkClasses[]` mapping matches against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    /// Display name (e.g. `vmnet-prod`).
    pub name: String,
    /// vCenter managed-object reference. Opaque.
    pub moref: String,
    /// True for a `DistributedVirtualPortgroup` (vDS); false for a standard PG.
    pub distributed: bool,
}

/// Backend-agnostic credential bundle resolved from the Provider's
/// `credentialsRef` Secret. Plain strings — interpreted by the factory.
///
/// `Debug` is hand-implemented to redact the password (security review
/// 2026-07-31 SEC-013): one stray `debug!(?creds)` must never put a vCenter
/// password in the logs.
#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
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
    ///
    /// `ca_bundle_pem` is the **already-resolved** PEM trust bundle (inline, or
    /// read from the ConfigMap/Secret named by `connection.ca_bundle`) or `None`
    /// to use the system trust roots. Resolution happens in the reconciler,
    /// where the kube client lives; the factory only consumes the PEM so it can
    /// stay free of cluster access (and trivially faked in tests). The
    /// `insecureSkipTLSVerify` flag is read from `connection`. See ADR-0008.
    async fn build(
        &self,
        connection: &ProviderConnection,
        creds: &Credentials,
        ca_bundle_pem: Option<&str>,
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

    /// Datastores reachable from `cluster` (the cluster's `datastore` set),
    /// each tagged with its SDRS datastore-cluster name when it belongs to one.
    /// Used for `storageClasses` reachability (ADR-0019).
    async fn list_datastores(&self, cluster: &Cluster) -> Result<Vec<Datastore>>;

    /// Networks reachable from `cluster` (the cluster's `network` set),
    /// distinguishing standard port groups from distributed ones. Used for
    /// `networkClasses` reachability (ADR-0019).
    async fn list_networks(&self, cluster: &Cluster) -> Result<Vec<Network>>;

    /// Import a bootable ISO into one failure domain as a vCenter template
    /// (ADR-0020): upload the ISO to `req.datastore`, create an empty EFI VM in
    /// `req.cluster`'s resource pool with the ISO attached as a CD-ROM and a NIC
    /// on `req.network`, then `MarkAsTemplate`. Idempotent: an existing template
    /// of `req.template_name` in the datacenter is left in place. Returns the
    /// resolved reference (`[datastore] template-name`).
    ///
    /// This is the one operation that mutates vCenter for image import; it is
    /// exercised only by the `image-import` Job (never the reconciler) and
    /// verified against a live vCenter (like the ADR-0019 introspection walk),
    /// so it is deliberately absent from the reconciler's own test surface.
    async fn import_iso_template(&self, req: &IsoImportRequest) -> Result<String>;

    /// Ensure a directory exists on a datastore (`FileManager.MakeDirectory`,
    /// `createParentDirectories`), so the datastore HTTP upload has somewhere to
    /// PUT the ISO. Idempotent: an already-present directory is not an error.
    /// `datacenter_moref` is the containing datacenter's managed-object id.
    async fn ensure_datastore_dir(
        &self,
        datacenter_moref: &str,
        datastore: &str,
        dir: &str,
    ) -> Result<()>;
}

/// Everything [`VSphereClient::import_iso_template`] needs to turn a local ISO
/// into a per-zone vCenter template. Resolved by the `image-import` subcommand
/// from the `Provider` + the target failure domain (ADR-0020).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsoImportRequest {
    /// Datacenter name the target cluster lives in.
    pub datacenter: String,
    /// Datacenter managed-object id (for `vmFolder` / VM lookup).
    pub datacenter_moref: String,
    /// Compute cluster name whose resource pool hosts the created VM.
    pub cluster: String,
    /// Compute cluster managed-object id (for `resourcePool`).
    pub cluster_moref: String,
    /// Datastore name the ISO is uploaded to and the template is created on.
    pub datastore: String,
    /// Port group name the template's NIC attaches to.
    pub network: String,
    /// Managed-object id of that port group (for the NIC backing).
    pub network_moref: String,
    /// True when the port group is a distributed (vDS) one — selects the
    /// distributed-port NIC backing over the standard device-name backing.
    pub network_distributed: bool,
    /// Template install-disk size, in GiB.
    pub disk_gib: i64,
    /// Disk provisioning (thin / thick / eagerZeroed).
    pub disk_provisioning: DiskProvisioning,
    /// Disk controller type (pvscsi / lsiLogic / …).
    pub disk_controller: DiskController,
    /// Virtual CPU count of the template.
    pub cpus: i32,
    /// Memory of the template, in MiB.
    pub memory_mib: i64,
    /// Firmware (bios / efi / efi-secure).
    pub firmware: Firmware,
    /// Virtual NIC adapter type (vmxnet3 / e1000 / …).
    pub network_adapter: NicAdapter,
    /// PCI slot number for the NIC (`ethernet0.pciSlotNumber`).
    pub nic_pci_slot: i32,
    /// vCenter folder path (under the datacenter VM folder) to place the
    /// template in, created if missing. `None` → the VM-folder root.
    pub folder: Option<String>,
    /// Datastore path of the already-uploaded ISO, in vСenter `[datastore]
    /// folder/file.iso` form. The `image-import` subcommand uploads the ISO to
    /// the datastore first, then passes this so the created VM's CD-ROM backing
    /// can reference it.
    pub iso_datastore_path: String,
    /// Template (and created-VM) display name.
    pub template_name: String,
    /// vCenter `guestId` for the OS (e.g. `rhel9_64Guest`, `ubuntu64Guest`).
    pub guest_id: String,
    /// When true, destroy any existing template of `template_name` in the
    /// datacenter before creating the new one. When false, an existing template
    /// short-circuits to a no-op.
    pub force_create: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SEC-013: a formatted `Credentials` must show the username (useful in
    /// logs) but never the password.
    #[test]
    fn credentials_debug_redacts_the_password() {
        let creds = Credentials {
            username: "administrator@vsphere.local".to_string(),
            password: "s3cret-hunter2".to_string(),
        };
        let rendered = format!("{creds:?}");
        assert!(
            rendered.contains("administrator@vsphere.local"),
            "{rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains("s3cret-hunter2"), "{rendered}");
    }
}
