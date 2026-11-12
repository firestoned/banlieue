// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `infrastructure.banlieue.io/v1alpha1` CloudHypervisorMachine CRD (ADR-0062).
//!
//! banlieue's implementation of the CAPI v1beta2 InfraMachine contract for
//! the Cloud Hypervisor backend, the sibling of
//! [`LibvirtMachine`](super::libvirt_machine::LibvirtMachine). Created by
//! banlieue's main controller once a `VirtualMachine` has been scheduled onto
//! a `cloud-hypervisor` `Provider`, and reconciled by the host-resident
//! provider on that one host (ADR-0060).
//!
//! # What differs from libvirt, and why
//!
//! - **Identity comes from the cluster.** A Cloud Hypervisor VM exists only
//!   while its VMM process does, so there is no host-side UUID to adopt.
//!   The machine's UID names everything the provider creates on the host:
//!   units, tap, directories, and [`cloud_hypervisor_provider_id`].
//! - **No host paths.** A machine names a `storageClass` and, per NIC, a
//!   `networkClass`. The host alone resolves them to a directory and a
//!   bridge, from its local config (ADR-0062 Decision 4), so a cluster-side
//!   actor can pick among what the host owner declared and nothing else.
//! - **No disk list.** The provider orders disks itself, because on this VMM
//!   disk order is boot order: OS disk, installer (Deferred only), seed. A
//!   user-supplied order is a way to get that wrong.
//! - **The OS disk size is required.** It is grown before first boot. The
//!   roadmap 09 spike showed a Kairos image left at its payload size never
//!   leaves recovery, so this is not an optimisation.
//!
//! The CAPI contract label is emitted by `crdgen`, as for every
//! `infrastructure.banlieue.io` CRD (ADR-0005).

use crate::common::*;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// URI scheme of a Cloud Hypervisor machine's `providerID`.
pub const PROVIDER_ID_SCHEME: &str = "cloudhypervisor";

/// The `providerID` for a machine: `cloudhypervisor://<provider>/<machine-uid>`
/// (ADR-0062 Decision 2).
///
/// The provider name rather than a hostname, as for libvirt (ADR-0050
/// Decision 1): a hostname is a deployment detail and would put a real
/// host into tracked YAML. The machine UID because nothing on the host
/// outlives the VMM process.
#[must_use]
pub fn cloud_hypervisor_provider_id(provider_name: &str, machine_uid: &str) -> String {
    format!("{PROVIDER_ID_SCHEME}://{provider_name}/{machine_uid}")
}

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "CloudHypervisorMachine",
    plural = "cloudhypervisormachines",
    shortname = "chm",
    namespaced,
    status = "CloudHypervisorMachineStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"Provider","type":"string","jsonPath":".spec.providerRef.name"}"#,
    printcolumn = r#"{"name":"Provisioned","type":"boolean","jsonPath":".status.initialization.provisioned"}"#,
    printcolumn = r#"{"name":"Power","type":"string","jsonPath":".status.observedPowerState"}"#,
    printcolumn = r#"{"name":"Image","type":"string","jsonPath":".spec.bootSource.image","priority":1}"#,
    printcolumn = r#"{"name":"ProviderID","type":"string","jsonPath":".spec.providerID","priority":1}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
// A guest's identity is fixed when its machine is created (ADR-0127 Decision 6):
// adding or removing `identity` later would change a running guest's MAC.
#[schemars(extend("x-kubernetes-validations" = [{
    "rule": "has(self.identity) == has(oldSelf.identity)",
    "message": "identity is set when the machine is created, or never",
}]))]
/// CloudHypervisorMachine — the concrete, scheduled VM request for a Cloud
/// Hypervisor host.
///
/// You normally do not create this by hand: banlieue's controller does, owned
/// by the `VirtualMachine` it was scheduled from, and the host-resident
/// provider on the chosen host runs it as a Cloud Hypervisor guest.
pub struct CloudHypervisorMachineSpec {
    // ------------------------------------------------------------------
    // CAPI v1beta2 contract fields
    // ------------------------------------------------------------------
    /// CAPI contract: Provider ID for the resulting Node, if this VM becomes
    /// a Kubernetes node. Format:
    /// `cloudhypervisor://<provider-name>/<machine-uid>`. Set by the provider
    /// once the guest exists.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "providerID"
    )]
    pub provider_id: Option<String>,

    /// CAPI contract (optional): failure domain placement. One host is one
    /// failure domain on this backend, as on libvirt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    // ------------------------------------------------------------------
    // banlieue / Cloud Hypervisor-specific
    // ------------------------------------------------------------------
    /// The `Provider` for the one host that runs this machine (ADR-0060).
    pub provider_ref: LocalObjectReference,

    /// Virtual CPUs.
    pub cpus: ChCpuSpec,

    /// Guest memory.
    pub memory: ChMemorySpec,

    /// Storage class the machine's disks live in. A name the host declares
    /// in its local config and publishes on its failure domain; never a
    /// path.
    pub storage_class: String,

    /// Where the OS disk comes from.
    pub boot_source: ChBootSource,

    /// OS disk size in GiB. Required, and grown to before first boot; never
    /// shrunk. Must be at least the image's size.
    #[serde(rename = "osDiskSizeGiB")]
    #[schemars(range(min = 1, max = 65_536))]
    pub os_disk_size_gi_b: u32,

    /// Network interfaces.
    #[schemars(length(max = 16))]
    pub nics: Vec<ChNicSpec>,

    /// Attach a vTPM (swtpm), resolved from the VM's `VMClass.spec.tpmEnabled`.
    /// Requires [`ChBootSourceKind::InstallMedia`] (ADR-0040, ADR-0048,
    /// ADR-0065), or an image that seals itself on first boot
    /// ([`Self::first_boot_sealing`], ADR-0088, ADR-0112).
    #[serde(default)]
    pub tpm_enabled: bool,

    /// Guest bootstrap payload, already resolved and placeholder-substituted
    /// by `banlieue-controller` (ADR-0025, ADR-0038). Rendered into a
    /// NoCloud `CIDATA` seed disk; the provider reads no Secret or ConfigMap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_data: Option<String>,

    /// Desired power state, resolved from the parent `VirtualMachine`.
    #[serde(default)]
    pub desired_power_state: PowerState,

    /// First-boot sealing the guest must report before `GuestReady`
    /// (ADR-0088, ADR-0112), copied by `banlieue-controller` from the parent
    /// `VirtualMachine`'s `VMImage.spec.firstBootSealing` when the class is
    /// `tpmEnabled`. The guest reports over its vsock (`sealed=` beside
    /// `phase=installed`). `None` for every other machine, which keeps
    /// today's `GuestReady` meaning unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_boot_sealing: Option<FirstBootSealing>,

    /// The guest identity this machine carries when it is not its own: set by
    /// `banlieue-controller` on a move's destination machine to the source's
    /// identity (ADR-0127 Decision 6). The provider derives the MAC, tap names,
    /// machine directory and EK certificate subject from it instead of from
    /// this machine's name and UID, so a moved guest keeps all of them. Absent
    /// on every machine that was never the destination of a move. Immutable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-kubernetes-validations" = [{
        "rule": "self == oldSelf",
        "message": "identity is immutable",
    }]))]
    pub identity: Option<ChMachineIdentity>,

    /// This machine is a move's destination (ADR-0127 Decision 4). Set by
    /// `banlieue-controller` at creation; the host provider prepares to receive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_in: Option<ChMoveIn>,

    /// This machine is a move's source (ADR-0127 Decision 4). Set by
    /// `banlieue-controller`; the host provider copies the guest out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_out: Option<ChMoveOut>,
}

impl CloudHypervisorMachineSpec {
    /// The `(name, uid)` the provider derives the guest's MAC, taps, directory
    /// and EK subject from: [`Self::identity`] when set, else the machine's own
    /// (ADR-0127 Decision 6).
    #[must_use]
    pub fn derived_identity<'a>(
        &'a self,
        own_name: &'a str,
        own_uid: &'a str,
    ) -> (&'a str, &'a str) {
        match &self.identity {
            Some(id) => (&id.name, &id.uid),
            None => (own_name, own_uid),
        }
    }
}

/// A guest identity carried across a move: the name and UID of the machine
/// the guest was created as.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChMachineIdentity {
    /// The original machine's name.
    pub name: String,
    /// The original machine's UID.
    pub uid: String,
}

/// The other host of a move, as the controller copied it from that host's
/// machine status. Each side accepts only this exact certificate (ADR-0127
/// Decision 10): pinned, never a CA.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChMovePeer {
    /// The peer's per-move certificate, PEM.
    pub certificate: String,
    /// `host:port` the destination listens on. Set only on the source's
    /// `moveOut.peer`: the source connects, the destination listens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

/// The destination side of a move, written by `banlieue-controller`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChMoveIn {
    /// The `VirtualMachineMove` this machine receives. Immutable.
    #[schemars(extend("x-kubernetes-validations" = [{
        "rule": "self == oldSelf",
        "message": "moveIn.moveId is immutable",
    }]))]
    pub move_id: String,
    /// The source host, once it has published its certificate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<ChMovePeer>,
    /// Resume the received guest. Written only after the source reported
    /// `Sent` and its VMM exited (Decision 5); never written with the source's
    /// `abort`. One way.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub commit: bool,
}

/// The source side of a move, written by `banlieue-controller`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChMoveOut {
    /// The `VirtualMachineMove` this machine sends. Immutable.
    #[schemars(extend("x-kubernetes-validations" = [{
        "rule": "self == oldSelf",
        "message": "moveOut.moveId is immutable",
    }]))]
    pub move_id: String,
    /// The destination host, once it has published its certificate and
    /// endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<ChMovePeer>,
    /// Pause the guest and send the disk delta and the VMM state. One way.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cutover: bool,
    /// Give up before commit: resume the guest here (Decision 12). One way;
    /// never written once the destination's `commit` is.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub abort: bool,
}

/// Virtual CPUs for a machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChCpuSpec {
    /// vCPUs the guest boots with.
    #[schemars(range(min = 1, max = 256))]
    pub boot: u32,
    /// Hotplug headroom: the most vCPUs the guest may be resized to. Fixed
    /// when the VM is created. `None` means no headroom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 256))]
    pub max: Option<u32>,
}

impl ChCpuSpec {
    /// The maximum vCPU count to create the VM with: the declared headroom,
    /// or the boot count when there is none, and never below the boot count.
    #[must_use]
    pub fn max_vcpus(&self) -> u32 {
        self.max.unwrap_or(self.boot).max(self.boot)
    }
}

/// Guest memory for a machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChMemorySpec {
    /// Guest memory in MiB.
    #[serde(rename = "sizeMiB")]
    #[schemars(range(min = 128, max = 4_194_304))]
    pub size_mi_b: u32,
    /// Back guest memory with hugepages. The host reports hugepages as their
    /// own capacity, since they are reserved rather than allocated on demand.
    #[serde(default)]
    pub hugepages: bool,
}

/// Where a machine's OS disk comes from: ADR-0040's install modes on this
/// backend.
///
/// An explicit `kind` discriminator, for the structural-schema reason
/// `LibvirtBootSource` gives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChBootSource {
    /// Which provisioning shape this machine uses.
    pub kind: ChBootSourceKind,
    /// The image in the host's image cache: an installed raw disk for
    /// [`Image`](ChBootSourceKind::Image), the installer ISO for
    /// [`InstallMedia`](ChBootSourceKind::InstallMedia).
    pub image: String,
}

/// The two provisioning shapes a [`ChBootSource`] can take.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ChBootSourceKind {
    /// Clone an installed raw image (reflink where the filesystem supports
    /// it) and grow it (`installMode: Immediate`). Cannot be combined with
    /// `tpmEnabled` (ADR-0048).
    #[default]
    Image,
    /// Boot an empty OS disk with the installer ISO attached second, and let
    /// the guest install itself (`installMode: Deferred`, ADR-0065).
    InstallMedia,
}

impl ChBootSource {
    /// Whether the OS disk must be created empty rather than cloned.
    #[must_use]
    pub fn needs_empty_os_disk(&self) -> bool {
        self.kind == ChBootSourceKind::InstallMedia
    }

    /// Whether the installer ISO is attached as the second disk.
    #[must_use]
    pub fn needs_install_media(&self) -> bool {
        self.kind == ChBootSourceKind::InstallMedia
    }
}

/// One network interface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChNicSpec {
    /// Stable NIC name; echoed in status.
    pub name: String,
    /// Network class: a name the host resolves to one of its bridges. Never
    /// a bridge name from the cluster.
    pub network_class: String,
    /// Optional MAC address. The provider derives a stable one from the
    /// machine UID otherwise, so the address survives a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac_address: Option<String>,
    /// IP address management for this interface.
    pub ipam: IpamSpec,
}

// ----------------------------------------------------------------------
// Status — CAPI v1beta2 InfraMachine contract
// ----------------------------------------------------------------------

/// Which source answered the provider's address lookup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ChAddressSource {
    /// Known before boot, from IPAM (ADR-0024, ADR-0033).
    Static,
    /// The host's neighbour table for the NIC's MAC, read over netlink.
    /// Blind until the guest transmits.
    Neighbour,
}

/// Observed state of a CloudHypervisorMachine, shaped to the CAPI v1beta2
/// InfraMachine status contract. The non-contract fields mean what they mean
/// on `LibvirtMachineStatus`, so the controller's status mirror treats both
/// backends alike.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CloudHypervisorMachineStatus {
    /// CAPI contract field.
    #[serde(default)]
    pub initialization: InitializationStatus,

    /// CAPI contract field (optional): observed failure domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    /// CAPI contract field (optional): VM addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<MachineAddress>,

    /// Which source produced [`addresses`](Self::addresses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_source: Option<ChAddressSource>,

    /// The VM's last observed run state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_power_state: Option<PowerState>,

    /// Whether the installed guest has announced itself (ADR-0043). Sticky
    /// once true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_installed: Option<bool>,

    /// Whether the installer has been removed, live and for future starts
    /// (ADR-0044, ADR-0065 Decision 4). Sticky once true; `None` when there
    /// was never an installer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_media_detached: Option<bool>,

    /// Whether a vTPM was attached, when `spec.tpmEnabled` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpm_attached: Option<bool>,

    /// PEM EK certificates, validated against this machine (ADR-0045,
    /// ADR-0065).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tpm_endorsement_certificates: Vec<String>,

    /// Partitions the guest has reported sealed, as LUKS containers, on a
    /// first-boot sealing machine (ADR-0088, ADR-0112): from the `sealed=`
    /// line of its vsock report. **Sticky**: the report is re-sent every boot,
    /// and a powered-off machine has not become unsealed. Empty until
    /// reported, and on every machine without `spec.firstBootSealing`.
    /// Guest-supplied, so it is only ever used to *withhold* `GuestReady`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sealed_partitions: Vec<String>,

    /// The unprivileged host uid this machine's units run as (ADR-0063
    /// Decision 3). Recorded so the provider re-adopts a running guest
    /// without trusting its own memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_uid: Option<u32>,

    /// The destination side of a move, published by the host provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_in: Option<ChMoveInStatus>,

    /// The source side of a move, published by the host provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_out: Option<ChMoveOutStatus>,

    /// Standard Kubernetes conditions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["type"],
    ))]
    pub conditions: Vec<Condition>,

    /// The `metadata.generation` this status was computed from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

/// Where the source host is in a move (ADR-0127 Decision 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ChMoveOutPhase {
    /// Generating the per-move key; the certificate is not published yet.
    Preparing,
    /// Copying the disk while the guest runs.
    Copying,
    /// The running copy is done; waiting for `cutover`.
    Precopied,
    /// Paused, sent the delta and the VMM state, and its VMM has exited. The
    /// controller may now commit the destination.
    Sent,
    /// Aborted before commit: the guest runs here again.
    Resumed,
    /// The move failed on this side; `message` says why.
    Failed,
}

/// Where the destination host is in a move (ADR-0127 Decision 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ChMoveInPhase {
    /// Allocating a guest uid and directories, starting an empty VMM.
    Preparing,
    /// Published its certificate and endpoint; waiting for the source.
    Listening,
    /// Received and restored the guest, which stays paused until `commit`.
    Received,
    /// Committed: the guest runs here.
    Running,
    /// The move failed on this side; `message` says why.
    Failed,
}

/// The source host's report of a move.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChMoveOutStatus {
    /// The move this reports on; a report for another move id is stale.
    pub move_id: String,
    pub phase: ChMoveOutPhase,
    /// This host's per-move certificate, PEM. The private key never leaves
    /// the host and is discarded when the move ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<String>,
    /// Bytes to send, once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_total: Option<u64>,
    /// Bytes sent so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_sent: Option<u64>,
    /// Why the move failed here, for a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// The destination host's report of a move.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChMoveInStatus {
    /// The move this reports on; a report for another move id is stale.
    pub move_id: String,
    pub phase: ChMoveInPhase,
    /// This host's per-move certificate, PEM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<String>,
    /// `host:port` this host listens on for the move, from its `[move] listen`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Why the move failed here, for a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ----------------------------------------------------------------------
// CloudHypervisorMachineTemplate — required by CAPI for MachineDeployment use
// ----------------------------------------------------------------------

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "CloudHypervisorMachineTemplate",
    plural = "cloudhypervisormachinetemplates",
    shortname = "chmt",
    namespaced,
    derive = "PartialEq"
)]
#[serde(rename_all = "camelCase")]
/// CloudHypervisorMachineTemplate — a stamped-out CloudHypervisorMachine spec.
///
/// CAPI requires an InfraMachineTemplate so a MachineSet or MachineDeployment
/// can mint identical machines. A spec template, not a disk template: nothing
/// clones a guest, which is what keeps per-VM vTPMs unique (ADR-0040).
pub struct CloudHypervisorMachineTemplateSpec {
    /// The spec stamped into every machine created from this template.
    pub template: CloudHypervisorMachineTemplateResource,
}

/// Wrapper matching CAPI's `template: { spec: {...} }` shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CloudHypervisorMachineTemplateResource {
    /// The CloudHypervisorMachine spec for machines created from this template.
    pub spec: CloudHypervisorMachineSpec,
}

#[cfg(test)]
#[path = "cloud_hypervisor_machine_tests.rs"]
mod cloud_hypervisor_machine_tests;
