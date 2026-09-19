// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `infrastructure.banlieue.io/v1alpha1` LibvirtMachine CRD (ADR-0050).
//!
//! banlieue's implementation of the CAPI v1beta2 InfraMachine contract for
//! the libvirt/KVM backend, and the sibling of
//! [`VSphereMachine`](super::vsphere_machine::VSphereMachine). It is created
//! by banlieue's main controller once a VirtualMachine has been scheduled:
//! the `pool`, `bootSource` and per-NIC `source` fields here are all
//! concrete, already resolved from VMClass / VMImage / Provider capabilities.
//!
//! Because this CRD complies with the CAPI InfraMachine contract, it can also
//! be used directly as a CAPI infrastructure provider — a `clusterv1.Machine`
//! with `infrastructureRef.kind: LibvirtMachine` works the same way. The CAPI
//! contract label `cluster.x-k8s.io/v1beta2: v1alpha1` is emitted onto the
//! generated CRD by `crdgen` (`crdgen_support::add_capi_contract_label`),
//! since `kube-derive` cannot set CRD-level labels. See ADR-0005.
//!
//! # What differs from vSphere, and why
//!
//! libvirt has no datacenter, cluster, resource pool or folder hierarchy, so
//! none of those fields appear. What replaces them is a single storage
//! `pool` and a `bootSource` that says which of the two provisioning shapes
//! this machine uses — see [`LibvirtBootSource`]. That split is the libvirt
//! expression of ADR-0040's `Immediate` / `Deferred` install modes, and it
//! is the reason a TPM-sealed VM is *simpler* to provision here than on
//! vSphere: there is no template clone at all, just an empty disk and an ISO.

use crate::common::*;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "LibvirtMachine",
    plural = "libvirtmachines",
    shortname = "lvm",
    namespaced,
    status = "LibvirtMachineStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"Provider","type":"string","jsonPath":".spec.providerRef.name"}"#,
    printcolumn = r#"{"name":"Provisioned","type":"boolean","jsonPath":".status.initialization.provisioned"}"#,
    printcolumn = r#"{"name":"Power","type":"string","jsonPath":".status.observedPowerState"}"#,
    printcolumn = r#"{"name":"Domain","type":"string","jsonPath":".spec.domainName","priority":1}"#,
    printcolumn = r#"{"name":"ProviderID","type":"string","jsonPath":".spec.providerID","priority":1}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
/// LibvirtMachine — the concrete, scheduled VM request for a libvirt/KVM host.
///
/// You normally do not create this by hand: banlieue's controller does, owned
/// by the `VirtualMachine` it was scheduled from, and the libvirt provider
/// reconciles it into a real domain.
pub struct LibvirtMachineSpec {
    // ------------------------------------------------------------------
    // CAPI v1beta2 contract fields
    // ------------------------------------------------------------------
    /// CAPI contract: Provider ID for the resulting Node, if this VM becomes
    /// a Kubernetes node. Format: `libvirt://<provider-name>/<domain-uuid>`.
    /// Set by the provider controller after the domain is defined.
    ///
    /// The *provider name*, not a hostname: the connection URI is a
    /// deployment detail that can change without the VM changing, and a real
    /// hostname here would end up in tracked YAML (see
    /// `rules/no-real-infrastructure.md`). The domain UUID is libvirt's own
    /// stable identity for the domain, unaffected by rename or restart.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "providerID"
    )]
    pub provider_id: Option<String>,

    /// CAPI contract (optional): failure domain placement. The banlieue
    /// scheduler writes the chosen failure domain here.
    ///
    /// libvirt has no cluster or datacenter hierarchy, so a failure domain is
    /// simply one host (see roadmap 13) — which makes this field carry more
    /// weight than on vSphere, not less: it is the only placement signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    // ------------------------------------------------------------------
    // banlieue / libvirt-specific
    // ------------------------------------------------------------------
    /// Reference to the banlieue `Provider` whose connection details describe
    /// the target libvirt host.
    pub provider_ref: LocalObjectReference,

    /// Storage pool that holds this machine's volumes, resolved from the
    /// storage class by the scheduler.
    pub pool: String,

    /// libvirt domain name. Unique per host, and the handle every domain
    /// procedure takes.
    ///
    /// Distinct from `metadata.name` on purpose: two namespaces can hold a
    /// `LibvirtMachine` called `db-01`, but one libvirt host cannot hold two
    /// domains by that name.
    pub domain_name: String,

    /// How this machine's OS disk comes into being.
    pub boot_source: LibvirtBootSource,

    /// Number of virtual CPUs.
    #[schemars(range(min = 1, max = 256))]
    pub vcpus: u32,

    /// Memory in MiB.
    #[schemars(range(min = 128, max = 4_194_304))]
    pub memory_mi_b: u32,

    /// Firmware. EFI requires OVMF on the host; `EfiSecure` additionally
    /// requires a `.secboot.fd` variant and pre-enrolled keys.
    pub firmware: Firmware,

    /// QEMU machine type (`q35`, `pc`, …). `None` lets libvirt pick its
    /// default for the host's architecture, which is the right answer unless
    /// an image needs a specific chipset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_type: Option<String>,

    /// Attach an emulated TPM 2.0 device (swtpm), resolved from the VM's
    /// `VMClass.spec.tpmEnabled` (ADR-0039).
    ///
    /// The device must exist before first boot, because Kairos's `kcrypt`
    /// seals LUKS keys to it during the unattended install — which is why
    /// `tpmEnabled` only makes sense paired with
    /// [`LibvirtBootSource::InstallMedia`] (ADR-0040, ADR-0048).
    ///
    /// swtpm keys its state by domain UUID, so a new domain always gets a
    /// fresh TPM. The corollary is that a domain carrying TPM state must
    /// never be cloned, and that `undefine` must pass
    /// `VIR_DOMAIN_UNDEFINE_TPM` or that state outlives the VM (ADR-0050
    /// Decision 4).
    #[serde(default)]
    pub tpm_enabled: bool,

    /// Disks. The first is the OS disk; the rest are blank data disks.
    #[schemars(length(min = 1, max = 32))]
    pub disks: Vec<LibvirtDiskSpec>,

    /// Network interfaces.
    #[schemars(length(max = 16))]
    pub network: Vec<LibvirtNicSpec>,

    /// Guest bootstrap payload, already resolved from the parent
    /// `VirtualMachine`'s `spec.userData` Secret or ConfigMap (ADR-0038) and
    /// placeholder-substituted by `banlieue-controller` (ADR-0025). The
    /// provider renders it into a NoCloud `cidata` volume — it never reads a
    /// Secret or ConfigMap itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_data: Option<String>,

    /// Desired power state, resolved from the parent `VirtualMachine`'s
    /// `spec.desiredPowerState` (ADR-0024).
    #[serde(default)]
    pub desired_power_state: PowerState,
}

/// Where this machine's OS disk comes from — the libvirt expression of
/// ADR-0040's install modes.
///
/// An explicit `kind` discriminator rather than a serde-tagged enum, for two
/// reasons. The structural-schema rules a CRD must satisfy reject an
/// internally-tagged enum whose tag carries a different `enum` per variant,
/// so `#[serde(tag = "type")]` cannot be used here at all. And an explicit
/// discriminator is what Non-Negotiable #4 asks for anyway: the shape is
/// declared, not inferred from which optional field happens to be set.
///
/// ```yaml
/// bootSource:
///   kind: installMedia
///   volume: kairos-sandbox-installer.iso
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibvirtBootSource {
    /// Which of the two provisioning shapes this machine uses.
    pub kind: LibvirtBootSourceKind,
    /// Volume name within [`LibvirtMachineSpec::pool`]: the backing image
    /// for [`BackingVolume`](LibvirtBootSourceKind::BackingVolume), or the
    /// installer ISO for
    /// [`InstallMedia`](LibvirtBootSourceKind::InstallMedia).
    pub volume: String,
}

/// The two provisioning shapes a [`LibvirtBootSource`] can take.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LibvirtBootSourceKind {
    /// Lay a copy-on-write overlay over an already-installed volume
    /// (`VMImage.spec.installMode: Immediate`). Fast, and right for anything
    /// that does not need per-VM disk encryption.
    ///
    /// Cannot be combined with `tpmEnabled`: the disk was installed before
    /// any per-VM TPM existed, so there was nothing to seal to (ADR-0040,
    /// enforced by ADR-0048).
    #[default]
    BackingVolume,
    /// Attach an installer ISO as a CD-ROM and boot onto an **empty** OS
    /// disk, letting the guest install itself (`installMode: Deferred`).
    ///
    /// Slower — a full unattended install per VM — but the only shape that
    /// can produce a disk sealed to that VM's own vTPM, and the shape
    /// roadmap 70's pool members use. On libvirt this path is *simpler* than
    /// `BackingVolume`: no template, no backing chain, nothing to copy.
    InstallMedia,
}

impl LibvirtBootSource {
    /// Whether the OS disk must be created empty rather than as an overlay.
    ///
    /// The one question the volume-creation step needs answered, kept here
    /// so the provider never re-derives it from the discriminator by hand.
    #[must_use]
    pub fn needs_empty_os_disk(&self) -> bool {
        self.kind == LibvirtBootSourceKind::InstallMedia
    }

    /// Whether this shape attaches an installer CD-ROM to the domain.
    #[must_use]
    pub fn needs_install_cdrom(&self) -> bool {
        self.kind == LibvirtBootSourceKind::InstallMedia
    }
}

/// Disk controller bus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LibvirtDiskBus {
    /// Paravirtualised, and the fastest. Needs `virtio_blk` in the guest,
    /// which every image banlieue builds has.
    #[default]
    Virtio,
    /// Emulated SCSI, via `virtio-scsi`.
    Scsi,
    /// Emulated SATA. Slowest, and only worth it for a guest with no virtio
    /// drivers at all.
    Sata,
}

/// One virtual disk on the resulting domain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibvirtDiskSpec {
    /// Stable disk name; echoed in status and used to name the volume.
    pub name: String,
    /// Disk size in GiB. For an overlay OS disk this is a floor — the
    /// overlay is created at least this large.
    #[schemars(range(min = 1, max = 65_536))]
    pub size_gi_b: u32,
    /// Controller bus.
    #[serde(default)]
    pub bus: LibvirtDiskBus,
}

/// What a NIC attaches to.
///
/// libvirt offers two genuinely different things, and which one is in use
/// changes where the guest's address can be read from: a managed network
/// usually runs dnsmasq and therefore has a DHCP lease file, a raw bridge
/// has nothing. Same explicit-discriminator shape as [`LibvirtBootSource`],
/// and for the same schema reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibvirtNicSource {
    /// Whether `name` names a host bridge or a libvirt network.
    pub kind: LibvirtNicSourceKind,
    /// The bridge interface (`br0`) or libvirt network (`default`).
    pub name: String,
}

/// The two things a [`LibvirtNicSource`] can name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LibvirtNicSourceKind {
    /// A libvirt-managed network. Usually has DHCP leases to read.
    #[default]
    Network,
    /// A host bridge, attached directly. No lease file, so addresses come
    /// from the guest agent or ARP.
    Bridge,
}

impl LibvirtNicSource {
    /// Whether a DHCP lease file could plausibly answer an address lookup
    /// for this interface. False for a raw bridge, where
    /// [`LibvirtAddressSource::DhcpLease`] will never return anything.
    #[must_use]
    pub fn has_dhcp_leases(&self) -> bool {
        self.kind == LibvirtNicSourceKind::Network
    }
}

/// One virtual network interface on the resulting domain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibvirtNicSpec {
    /// Stable NIC name; echoed in status.
    pub name: String,
    /// Resolved bridge or libvirt network.
    pub source: LibvirtNicSource,
    /// Device model. `None` means `virtio`, which is what every image
    /// banlieue builds expects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional MAC address (otherwise libvirt generates one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac_address: Option<String>,
    /// IP address management for this interface.
    pub ipam: IpamSpec,
}

// ----------------------------------------------------------------------
// Status — CAPI v1beta2 InfraMachine contract
// ----------------------------------------------------------------------

/// Which source answered when the provider looked up the guest's addresses
/// (ADR-0050 Decision 7).
///
/// Recorded because the three sources disagree in ways that matter: the
/// agent reports what the guest actually configured, a lease reports what
/// DHCP offered (which the guest may have ignored), and ARP reports what was
/// seen on the wire. An address that looks wrong is only diagnosable if you
/// know which of those produced it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LibvirtAddressSource {
    /// `qemu-guest-agent` inside the guest. Authoritative.
    GuestAgent,
    /// The libvirt network's DHCP lease file. Managed networks only.
    DhcpLease,
    /// The host's ARP table. Best effort, and blind until the guest
    /// transmits.
    ArpTable,
}

/// Observed state of a LibvirtMachine, shaped to the CAPI v1beta2
/// InfraMachine status contract (plus libvirt-specific diagnostics).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibvirtMachineStatus {
    /// CAPI contract field: replaces the deprecated v1beta1 `status.ready`.
    #[serde(default)]
    pub initialization: InitializationStatus,

    /// CAPI contract field (optional): observed failure domain — the host
    /// the domain actually landed on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    /// CAPI contract field (optional): VM addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<MachineAddress>,

    /// libvirt's UUID for the domain, in its 36-character textual form. The
    /// domain's real identity — stable across rename and host restart — and
    /// the source for `spec.providerID`. Not part of the CAPI contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_uuid: Option<String>,

    /// The domain's last observed run state, mapped onto banlieue's
    /// backend-neutral [`PowerState`] (ADR-0034). The hypervisor's view, not
    /// a guest-OS-boot signal. Not part of the CAPI contract; mirrored onto
    /// the parent `VirtualMachine`'s `status.observedPowerState`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_power_state: Option<PowerState>,

    /// Which source produced [`addresses`](Self::addresses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_source: Option<LibvirtAddressSource>,

    /// Whether an emulated TPM was attached, when `spec.tpmEnabled` is set
    /// (ADR-0039). `None` when `tpmEnabled` is `false` or the attach has not
    /// run yet. A failed attach surfaces through the conditions rather than
    /// a dedicated `VirtualMachine`-level mirror.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpm_attached: Option<bool>,

    /// CAPI-compatible conditions. The `Ready` condition is mirrored as
    /// `InfrastructureReady` on the parent per contract.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["type"],
    ))]
    pub conditions: Vec<Condition>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

// ----------------------------------------------------------------------
// LibvirtMachineTemplate — required by CAPI for MachineDeployment use
// ----------------------------------------------------------------------

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "LibvirtMachineTemplate",
    plural = "libvirtmachinetemplates",
    shortname = "lvmt",
    namespaced,
    derive = "PartialEq"
)]
#[serde(rename_all = "camelCase")]
/// LibvirtMachineTemplate — a stamped-out LibvirtMachine spec.
///
/// CAPI requires an InfraMachineTemplate so higher-level controllers (a
/// MachineSet / MachineDeployment) can mint many identical machines from one
/// template. banlieue ships it for CAPI compatibility; standalone
/// VirtualMachine users do not need it.
///
/// Note that a template is a *spec* template, not a disk template: nothing
/// here clones a domain. That distinction matters on libvirt, because
/// cloning a domain that carries TPM state would copy the state too, which
/// is precisely ADR-0040's shared-vTPM problem.
pub struct LibvirtMachineTemplateSpec {
    /// The LibvirtMachine spec stamped into every machine created from this
    /// template.
    pub template: LibvirtMachineTemplateResource,
}

/// Wrapper matching CAPI's `template: { spec: {...} }` InfraMachineTemplate
/// shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibvirtMachineTemplateResource {
    /// The LibvirtMachine spec for machines created from this template.
    pub spec: LibvirtMachineSpec,
}

#[cfg(test)]
#[path = "libvirt_machine_tests.rs"]
mod libvirt_machine_tests;
