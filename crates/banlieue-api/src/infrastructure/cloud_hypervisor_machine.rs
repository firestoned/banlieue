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
    /// ADR-0065).
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

    /// The unprivileged host uid this machine's units run as (ADR-0063
    /// Decision 3). Recorded so the provider re-adopts a running guest
    /// without trusting its own memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_uid: Option<u32>,

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
