// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! vSphere client surface used by the reconcilers.
//!
//! The reconcilers depend only on the [`VSphereClient`] trait so they can be
//! unit-tested with [`FakeClient`] without compiling against `vim_rs`. The
//! real implementation in [`vim`] wraps `vim_rs::core::client::ClientBuilder`.

use async_trait::async_trait;
use banlieue_api::banlieue::{DiskController, InstallMode, NicAdapter, ProviderConnection};
use banlieue_api::common::{DiskProvisioning, Firmware, PowerState};

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

    /// Find a VM template by display name within `dc`, optionally scoped
    /// to `folder` (a path relative to the datacenter's VM folder, e.g.
    /// `templates/cluster-01`). Returns `None` when no template with that
    /// name exists (in `folder`, if given) or when `folder` itself
    /// doesn't exist; returns `Err` when the lookup itself fails (auth /
    /// network).
    ///
    /// `folder: None` searches the whole datacenter — correct for a
    /// `Template`-kind image, which has no per-zone folder. `folder:
    /// Some(_)` is required for a per-zone (`Url`-kind) import: every
    /// zone's template shares the same display name (ADR-0020 Decision
    /// #5), so a datacenter-wide search can match a *different* zone's
    /// template (found live: a `VirtualMachine` cloned from the wrong
    /// zone's template because the lookup wasn't folder-scoped).
    async fn find_template(
        &self,
        dc: &Datacenter,
        folder: Option<&str>,
        name: &str,
    ) -> Result<Option<Template>>;

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
    /// on `req.network`. When `req.install_mode` is `Immediate` (ADR-0021),
    /// power it on and wait for the cloud-config's unattended Kairos install
    /// to run its `after-install-chroot` identity-wipe stage and power the VM
    /// off itself (`install.poweroff`, no reboot — the disk is never booted
    /// by the build), bounded by `req.install_timeout_seconds`; on success,
    /// remove the CD-ROM device and `MarkAsTemplate`, on timeout fail without
    /// destroying the VM so it can be inspected via console. When
    /// `Deferred`/`Manual`, skip straight to `MarkAsTemplate` with no
    /// power-on and the CD-ROM left attached — ADR-0020's original behavior,
    /// preserved as the sanctioned per-clone-install path for `tpmEnabled`
    /// VMClasses (ADR-0040) as well as for a build that isn't Kairos-driven.
    /// Idempotent: an existing template of `req.template_name` in the
    /// datacenter is left in place. Returns the resolved reference
    /// (`[datastore] template-name`).
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

    /// Destroy any existing VM/template named `name` in `folder` (a path
    /// under the datacenter's VM folder, e.g. `templates/cluster-01`) — a
    /// no-op if absent.
    ///
    /// Scoped to `folder`, not the whole datacenter: every zone's target
    /// shares the same display name (the `VMImage` name), so a
    /// datacenter-wide lookup would risk destroying a *different* zone's
    /// in-flight VM that happens to share the name (ADR-0020 Decision #5).
    ///
    /// Called early in the `image-import` flow, before the datastore
    /// upload/reuse-check, specifically for `--force-create`: a template
    /// whose CD-ROM backing still references the target ISO holds an NFC
    /// lock on that file, which otherwise makes the datastore reuse-check's
    /// HEAD probe fail — indistinguishable from the file genuinely being
    /// absent (an inconclusive check is treated as absent, matching the
    /// existing fail-open-to-reupload posture) — causing an unnecessary
    /// re-upload onto a different datastore member even though the file is
    /// already present. Destroying the stale target first releases the lock
    /// before the reuse-check ever runs.
    async fn destroy_if_present(
        &self,
        datacenter_moref: &str,
        folder: &str,
        name: &str,
    ) -> Result<()>;

    /// Clone a VM from an already-built per-zone template (ADR-0024's
    /// create path): relocate onto `req.datastore` / the cluster's resource
    /// pool / `req.folder`, override CPU/memory, reconfigure the clone's
    /// first NIC onto `req.network_moref`, and set `req.extra_config` as
    /// `extraConfig` in the same clone call — matching this environment's
    /// existing hand-provisioned VM convention (`guestinfo.network.*` /
    /// `guestinfo.userdata`) built by
    /// [`crate::reconciler::vspheremachine::build_guestinfo`]. Always clones
    /// powered off; drive the desired power state afterward with
    /// [`VSphereClient::set_power_state`]. Returns the new VM's moref.
    ///
    /// Deliberately out of scope for this first pass (documented, not
    /// silently dropped): growing the OS disk to `VSphereDiskSpec.size_gi_b`
    /// when larger than the template's own disk, additional (non-OS) disks,
    /// and NICs beyond the first.
    async fn clone_vm(&self, req: &CloneVmRequest) -> Result<String>;

    /// Drive `vm_moref` to `desired` (`PowerOnVM_Task` / `PowerOffVM_Task` /
    /// `SuspendVM_Task`). A VM already in `desired`'s state is a no-op
    /// (vCenter itself rejects a redundant power op with `InvalidState`;
    /// callers should check `VSphereClient`-reported current state first
    /// where that matters, e.g. after `clone_vm`, which always clones
    /// powered off).
    async fn set_power_state(&self, vm_moref: &str, desired: PowerState) -> Result<()>;

    /// Read-only counterpart to [`VSphereClient::set_power_state`]:
    /// `vm_moref`'s current `VirtualMachine.runtime.powerState` (ADR-0034).
    /// The hypervisor's own view — available immediately on power-on, not a
    /// guest-OS-boot signal (VMware Tools / guest state are out of scope).
    async fn power_state(&self, vm_moref: &str) -> Result<PowerState>;

    /// Power off (if not already) and destroy the VM at `vm_moref` — the
    /// backend teardown half of `VSphereMachine`'s deletion finalizer
    /// (ADR-0026). Moref-based, unlike [`VSphereClient::destroy_if_present`]
    /// (name+folder based, for the *template* import path): `vm_moref` is
    /// exactly what `clone_vm` returned and `VSphereMachine.status.vmRef`
    /// already stored, so no name lookup is needed or wanted — a name-based
    /// lookup would reintroduce the same cross-zone same-display-name
    /// collision risk already fixed for templates and VM lookups.
    ///
    /// Idempotent: a moref vCenter no longer recognizes (already destroyed,
    /// e.g. by a prior finalizer attempt that got as far as `Destroy_Task`
    /// but never observed the response) is success, not an error.
    async fn destroy_vm(&self, vm_moref: &str) -> Result<()>;

    /// Attach a virtual TPM (vTPM) device to `vm_moref` via a standalone
    /// `ReconfigVM_Task` (ADR-0039) — the same call the vCenter UI and
    /// PowerCLI's `New-VTpm` make; govc has no wrapping subcommand for it
    /// (confirmed against 0.52.0/0.56.0). Called from [`crate::reconciler::
    /// vspheremachine::ensure_vm`] after `clone_vm` (which always clones
    /// powered off) and before the power-on step, since Kairos's `kcrypt`
    /// seals LUKS keys to the TPM during unattended install and the device
    /// must exist before first boot.
    async fn add_tpm_device(&self, vm_moref: &str) -> Result<()>;
}

/// Everything [`VSphereClient::clone_vm`] needs to clone a per-zone template
/// into a running VM (ADR-0024). Every reference is already resolved to a
/// concrete vCenter moref by the caller — mirrors [`IsoImportRequest`]'s own
/// shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneVmRequest {
    /// Datacenter managed-object id (for `vmFolder` resolution).
    pub datacenter_moref: String,
    /// Compute cluster managed-object id (for `resourcePool`).
    pub cluster_moref: String,
    /// Managed-object id of the source template to clone from.
    pub template_moref: String,
    /// Managed-object id of the concrete datastore (never a
    /// datastore-cluster/SDRS pod — the caller resolves that down to one
    /// concrete member first) to relocate the clone onto.
    pub datastore_moref: String,
    /// Target port group name for the clone's first NIC (used for the
    /// standard, non-distributed backing's `deviceName`).
    pub network: String,
    /// Managed-object id of the target port group for the clone's first NIC.
    pub network_moref: String,
    /// True when `network_moref` is a distributed (vDS) port group.
    pub network_distributed: bool,
    /// Virtual CPU count.
    pub num_cpus: i32,
    /// Memory, in MiB.
    pub memory_mib: i64,
    /// vCenter folder path (under the datacenter VM folder) to place the
    /// clone in, created if missing. `None` — the VM-folder root.
    pub folder: Option<String>,
    /// Display name of the resulting VM.
    pub vm_name: String,
    /// `extraConfig` key/value pairs — `guestinfo.network.*` /
    /// `guestinfo.userdata` (ADR-0024), built by
    /// [`crate::reconciler::vspheremachine::build_guestinfo`].
    pub extra_config: Vec<(String, String)>,
}

/// One template NIC, fully resolved to a concrete port-group moref
/// (ADR-0031). Built by `crate::import::run` from a [`crate::import::ResolvedNic`]
/// once the zone's cluster is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedNic {
    /// Port group name this NIC attaches to.
    pub network: String,
    /// Managed-object id of that port group (for the NIC backing).
    pub network_moref: String,
    /// True when the port group is a distributed (vDS) one — selects the
    /// distributed-port NIC backing over the standard device-name backing.
    pub network_distributed: bool,
    /// Virtual NIC adapter type (vmxnet3 / e1000 / …).
    pub adapter: NicAdapter,
    /// PCI slot number for this NIC (`ethernetN.pciSlotNumber`).
    pub pci_slot: i32,
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
    /// The template's NICs, each already resolved to a concrete port-group
    /// moref (ADR-0031). Never empty — [`crate::import::resolve_nic_networks`]
    /// synthesizes exactly one fully-defaulted entry when no `--nic` flags
    /// were given, preserving the pre-ADR-0031 single-NIC default.
    pub nics: Vec<RequestedNic>,
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
    /// Bound, in seconds, on how long to wait for the created VM to power
    /// itself off after the unattended Kairos install completes
    /// (`install.poweroff: true` in the cloud-config), before failing the
    /// import (ADR-0021). Set from `VMImage.spec.template.installTimeoutSeconds`.
    pub install_timeout_seconds: i32,
    /// How the install step is driven. `Immediate` runs the
    /// install-then-generalize sequence (power on, wait for self-poweroff,
    /// remove the CD-ROM) before `MarkAsTemplate`; `Deferred`/`Manual`
    /// revert to ADR-0020's original behavior: create the VM, attach the
    /// ISO, `MarkAsTemplate` immediately, no power-on. Set from
    /// `VMImage.spec.template.installMode` (ADR-0021, ADR-0040).
    pub install_mode: InstallMode,
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
