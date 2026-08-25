// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue.io/v1alpha1` VMImage CRD.
//!
//! Cluster-scoped image catalog. Each VMImage has one or more sources, each
//! mapped to a provider class. The image controller maintains per-provider
//! readiness in status by polling each registered Provider and (where
//! supported) importing the image on demand.

use crate::common::{CloudConfigSource, DiskProvisioning, Firmware, LocalObjectReference};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "banlieue.io",
    version = "v1alpha1",
    kind = "VMImage",
    plural = "vmimages",
    shortname = "vmi",
    status = "VMImageStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"OS","type":"string","jsonPath":".spec.osDistribution"}"#,
    printcolumn = r#"{"name":"Version","type":"string","jsonPath":".spec.osVersion"}"#,
    printcolumn = r#"{"name":"Arch","type":"string","jsonPath":".spec.architecture"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type=='Ready')].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
/// VMImage — a cluster-scoped, backend-agnostic catalog entry for a bootable
/// guest image.
///
/// A VMImage names an operating system (family / distribution / version /
/// architecture) once, then lists — per provider class — where that image
/// actually lives on each backend (`spec.sources`). A VirtualMachine
/// references a VMImage by name (`spec.imageRef`); the scheduler and the
/// chosen provider resolve it to a concrete template / backing file / import
/// URL at provisioning time.
///
/// # Why create one
///
/// - **One name, many backends.** "ubuntu-22.04" can map to a vSphere
///   template, a Proxmox template VMID, and a libvirt qcow2 — users reference
///   a single VMImage regardless of where the VM lands.
/// - **Explicit, auditable image sourcing.** Sources (and optional checksums)
///   are declared, not auto-discovered, so what actually boots is reviewable.
/// - **Readiness gating.** The image controller records per-Provider
///   readiness in `status`; the scheduler refuses to place a VM until the
///   image is confirmed available (or importable) on a candidate Provider.
///
/// Cluster-scoped: a VMImage is shared by VirtualMachines in any namespace.
pub struct VMImageSpec {
    /// Broad operating-system family. Coarser than `osDistribution`; lets
    /// providers apply high-level guest handling.
    pub os_family: OsFamily,
    /// Free-form distribution string. Examples: ubuntu, rhel, debian,
    /// fedora-coreos, windows-server.
    pub os_distribution: String,
    /// Free-form version string. Examples: "22.04", "9.4", "2022".
    pub os_version: String,
    /// Guest CPU architecture. Failure domains whose hosts cannot run this
    /// architecture are filtered out by the scheduler.
    pub architecture: Architecture,

    /// Guest agent contract this image is built to support; determines how
    /// `VirtualMachine.spec.userData` is delivered.
    #[serde(default)]
    pub guest_agent: GuestAgent,

    /// Per-provider source mappings — one backend binding for this catalog
    /// entry per `providerClass` you intend to schedule VMs onto ("one
    /// name, many backends", see the type-level doc comment above).
    ///
    /// `x-kubernetes-list-type: map` keyed on `providerClass`: the API
    /// server rejects a second entry for a `providerClass` that already
    /// has one, rather than leaving it to `find_url_source` /
    /// `find_vsphere_source` to silently pick whichever came first.
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["providerClass"],
    ))]
    pub sources: Vec<ImageSource>,

    /// Optional default cloud-config baked into the built artifact for
    /// `Url`-kind sources. Resolved by `banlieue-imagebuilder` and passed to
    /// the kairos-operator `OSArtifact` as `cloudConfigRef`
    /// (`auroraboot build-iso --cloud-config`). SecretRef-first; see
    /// [`CloudConfigSource`] and ADR-0020. Ignored for non-`Url` sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_config: Option<CloudConfigSource>,

    /// How the backend **template** is built from a `Url` source (root
    /// folder, network, disk, CPU / memory / firmware / NIC, force knobs).
    /// Every field
    /// is optional and falls back to a built-in default. Only meaningful for
    /// `Url` sources; ignored for `Template` / `BackingFile`. See
    /// [`VMImageTemplate`] and ADR-0020.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<VMImageTemplate>,

    /// Additional files overlaid onto a built ISO for `Url`-kind vSphere
    /// sources (e.g. a hand-verified `grub.cfg`). Resolved by
    /// `banlieue-imagebuilder` into the kairos-operator `OSArtifact`'s
    /// `spec.volumes[]` + `spec.artifacts.overlayISOVolume` — the same
    /// `auroraboot build-iso --overlay-iso` mechanism a hand-run ISO-build
    /// pipeline would use. See [`IsoOverlaySource`] and ADR-0022. Ignored for
    /// `cloudImage`-kind builds and non-`Url` sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iso_overlay: Option<IsoOverlaySource>,
}

/// Additional files overlaid onto a built ISO, backed by a single Secret.
/// Only the Secret's name and the declared key/path list are read by
/// `banlieue-imagebuilder` — never its content (ADR-0022; mirrors the
/// Secret-free posture established for `autoManageInstall`, ADR-0021).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IsoOverlaySource {
    /// Secret in the imagebuild namespace holding the overlay file contents.
    pub secret_ref: LocalObjectReference,
    /// Explicit key -> ISO-relative-path mapping. At least one entry expected;
    /// an empty list is accepted but wires nothing into the `OSArtifact`.
    #[serde(default)]
    pub files: Vec<IsoOverlayFile>,
}

/// One overlay file: a key within [`IsoOverlaySource::secret_ref`] mapped to
/// a destination path relative to the built ISO's root.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IsoOverlayFile {
    /// Key within the overlay Secret holding this file's content.
    pub key: String,
    /// Destination path, relative to the ISO root (e.g. `boot/grub2/grub.cfg`).
    pub path: String,
}

/// Shape of the backend **template** built from a `Url`-kind [`ImageSource`]
/// (the clone source imported per zone by the owning provider). Actual VMs
/// size their own disk / choose their own network at provision time; these are
/// the template defaults. See ADR-0020.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VMImageTemplate {
    /// Root vCenter inventory folder (path under the datacenter's VM folder,
    /// e.g. `templates/kairos`); created if missing. When unset, the
    /// datacenter's VM-folder root is the root. vSphere-only.
    ///
    /// Named `rootFolder`, not `folder`, because it is never the literal
    /// target: the per-zone import always places the template at
    /// `<rootFolder>/<failure-domain-name>` (ADR-0020). Two failure domains
    /// commonly share a datacenter (only their cluster differs), and
    /// vSphere's VM/Template folder hierarchy is scoped per-datacenter, not
    /// per-cluster — without the per-zone subfolder, every zone's import
    /// raced `CreateVM_Task` against the identical folder + template name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_folder: Option<String>,

    /// The template's network interfaces. Empty means exactly one NIC, using
    /// every per-entry default below — the same behavior this field had
    /// before it became a list (ADR-0031). vSphere-only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network: Vec<VMImageTemplateNic>,

    /// Install disk of the template (the clone source's disk). When unset, a
    /// thin 100 GiB disk on a pvscsi controller is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<VMImageTemplateDisk>,

    /// Virtual CPU count of the template (`govc vm.create -c`). When unset,
    /// defaults to 2. vSphere-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<i32>,

    /// Memory of the template, in MiB (`govc vm.create -m`). When unset,
    /// defaults to 4096. vSphere-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mib: Option<i64>,

    /// Firmware for the template (`govc vm.create -firmware`). Reuses the
    /// backend-agnostic [`Firmware`] hint (`bios` / `efi` / `efi-secure`). When
    /// unset, defaults to `efi`. vSphere maps `efi-secure` to EFI with secure
    /// boot enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<Firmware>,

    /// vCenter `guestId` for the template (`govc vm.create -g`, e.g.
    /// `rhel9_64Guest`, `ubuntu64Guest`). When unset, it is derived from the
    /// VMImage's `osFamily` / `osDistribution` / `osVersion`. vSphere-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_id: Option<String>,

    /// Re-upload the built ISO even if one of that name already exists on the
    /// backend, deleting the existing one first (the vСenter datastore file API
    /// does not overwrite in place). Threaded as `--force-upload`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force_upload: bool,

    /// Recreate the template even if one of that name already exists,
    /// destroying the existing one first. Threaded as `--force-create`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force_create: bool,

    /// Bound, in seconds, on how long the import Job waits for the
    /// unattended Kairos install to finish and the VM to power itself off
    /// (`install.poweroff: true` in the cloud-config) before failing the
    /// Job. When unset, defaults to 1800 (30 min). See ADR-0021: the golden
    /// disk is never rebooted by the build, so this bounds only the
    /// (typically 8-12 min) unattended-install window, not a boot cycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_timeout_seconds: Option<i32>,

    /// Run the install-then-generalize sequence (power on, wait for the
    /// unattended install to finish and the VM to power itself off, remove
    /// the CD-ROM, then mark as template) at all. When unset, defaults to
    /// `true`. `false` reverts to creating the VM, attaching the ISO, and
    /// marking it as a template immediately — no power-on — for a build that
    /// isn't Kairos-driven or whose install/generalize is managed some other
    /// way. See ADR-0021.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_manage_install: Option<bool>,

    /// Keep the per-zone vCenter template(s) this `VMImage` caused to be
    /// built when the `VMImage` itself is deleted. When unset (the default),
    /// deleting a `VMImage` also destroys every per-zone template it owns —
    /// declarative deletion, matching `VirtualMachine`'s own cascade onto its
    /// `VSphereMachine` (ADR-0026). Set `true` to opt out — e.g. the template
    /// is still referenced by another generation, or its lifecycle is
    /// managed by hand outside banlieue. vSphere-only; ignored by any other
    /// provider. See ADR-0028.
    #[serde(default, skip_serializing_if = "is_false")]
    pub retain_on_delete: bool,
}

/// One network interface on a `Url`-source template (ADR-0031).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VMImageTemplateNic {
    /// Port group this NIC attaches to. When unset, the zone's first
    /// reachable network class (ADR-0019) is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,

    /// Virtual NIC adapter type (`govc vm.create -net.adapter`). When
    /// unset, defaults to `vmxnet3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<NicAdapter>,

    /// PCI slot number for this NIC (`ethernetN.pciSlotNumber`). Slot 192
    /// on the first NIC yields a stable `ens192` interface name in the
    /// guest. When unset, defaults to `192 + this NIC's index` in
    /// `VMImageTemplate.network` — so a template with several NICs and no
    /// explicit slots still gets predictable, non-colliding
    /// `ens192`/`ens193`/`ens194`/... naming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pci_slot: Option<i32>,
}

/// Install-disk shape for a `Url`-source template (mirrors the `govc vm.create
/// -disk*` flags used by `create-kairos-template.sh`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VMImageTemplateDisk {
    /// Disk size, in GiB. Defaults to 100 when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,

    /// Provisioning hint: `thin` (default), `thick`, or `eagerZeroed`. Reuses
    /// the backend-agnostic [`DiskProvisioning`] shared with `VMClass` /
    /// `VSphereMachine`; eager-zeroing is the `eagerZeroed` variant, not a
    /// separate flag. Providers honor it on a best-effort basis.
    #[serde(default, rename = "type")]
    pub provisioning: DiskProvisioning,

    /// Disk controller type. Defaults to `pvscsi`.
    #[serde(default)]
    pub controller: DiskController,
}

/// Disk controller type for the template's install disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DiskController {
    /// VMware Paravirtual SCSI (what `create-kairos-template.sh` uses).
    #[default]
    Pvscsi,
    /// LSI Logic Parallel.
    LsiLogic,
    /// LSI Logic SAS.
    LsiLogicSas,
    /// BusLogic Parallel.
    BusLogic,
}

impl DiskController {
    /// Stable token, for CLI args / logs (matches the serde camelCase form).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            DiskController::Pvscsi => "pvscsi",
            DiskController::LsiLogic => "lsiLogic",
            DiskController::LsiLogicSas => "lsiLogicSas",
            DiskController::BusLogic => "busLogic",
        }
    }
}

impl std::str::FromStr for DiskController {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "pvscsi" => Ok(Self::Pvscsi),
            "lsilogic" => Ok(Self::LsiLogic),
            "lsilogicsas" => Ok(Self::LsiLogicSas),
            "buslogic" => Ok(Self::BusLogic),
            other => Err(format!(
                "unknown disk controller {other:?} (expected: pvscsi, lsiLogic, lsiLogicSas, busLogic)"
            )),
        }
    }
}

/// Virtual NIC adapter type for the template (`govc vm.create -net.adapter`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum NicAdapter {
    /// VMware Paravirtual (what `create-kairos-template.sh` uses).
    #[default]
    Vmxnet3,
    /// VMware Vmxnet2.
    Vmxnet2,
    /// Intel E1000.
    E1000,
    /// Intel E1000e.
    E1000e,
}

impl NicAdapter {
    /// Stable token, for CLI args / logs (matches the serde camelCase form).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            NicAdapter::Vmxnet3 => "vmxnet3",
            NicAdapter::Vmxnet2 => "vmxnet2",
            NicAdapter::E1000 => "e1000",
            NicAdapter::E1000e => "e1000e",
        }
    }
}

impl std::str::FromStr for NicAdapter {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "vmxnet3" => Ok(Self::Vmxnet3),
            "vmxnet2" => Ok(Self::Vmxnet2),
            "e1000" => Ok(Self::E1000),
            "e1000e" => Ok(Self::E1000e),
            other => Err(format!(
                "unknown NIC adapter {other:?} (expected: vmxnet3, vmxnet2, e1000, e1000e)"
            )),
        }
    }
}

/// `skip_serializing_if` predicate: omit a `bool` field when it is `false`.
fn is_false(b: &bool) -> bool {
    !*b
}

/// Broad operating-system family of a VMImage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OsFamily {
    Linux,
    Windows,
    Bsd,
    Other,
}

/// Guest CPU architecture a VMImage targets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Architecture {
    Amd64,
    Arm64,
}

/// Guest bootstrap-agent contract an image ships with. Determines how
/// `VirtualMachine.spec.userData` is delivered into the guest.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum GuestAgent {
    #[default]
    CloudInit,
    Ignition,
    Sysprep,
    None,
}

/// One backend's mapping for a VMImage: which provider class it applies to,
/// and how to find (or import) the image there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageSource {
    /// Name of the ProviderClass this source applies to. Conventional
    /// values: `vsphere`, `proxmox`, `libvirt`.
    pub provider_class: String,

    /// What kind of backend artifact `ref` refers to.
    pub kind: ImageSourceKind,

    /// Provider-interpreted reference:
    ///   vsphere + Template:     template name e.g. "ubuntu-22.04-cloudinit"
    ///   proxmox + Template:     template VMID e.g. "9000"
    ///   libvirt + BackingFile:  path e.g. "/var/lib/libvirt/images/ubuntu.qcow2"
    ///   * + Url:                ignored; uses `importFrom`
    #[serde(rename = "ref")]
    pub reference: String,

    /// Optional source URL. When set, providers that support image import
    /// will pull from here if the image isn't already present locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_from: Option<String>,

    /// Optional checksum for imported images. Format: `<alg>:<hex>`,
    /// e.g. `sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b...`.
    /// Supported algorithms: `sha256`, `sha512`. Provider import Jobs verify
    /// the built artifact against this value before writing it to the backend
    /// and fail closed on mismatch or an unsupported algorithm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

/// What kind of backend artifact an [`ImageSource`]'s `ref` points at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ImageSourceKind {
    /// A template/clone source pre-existing on the provider backend.
    Template,
    /// A backing disk file (libvirt-style).
    BackingFile,
    /// A URL-only source. Requires `importFrom` to be set; providers that
    /// can't import will skip this image.
    Url,
}

/// Observed availability of a VMImage across the Providers that can serve it.
/// Maintained by the image controller; read by the scheduler.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VMImageStatus {
    /// Per-Provider readiness. One entry per Provider that supports this
    /// image's providerClass and has reconciled at least once.
    ///
    /// **Merge-keyed, and it must stay that way (ADR-0015).** Several
    /// providers write this list concurrently, each applying only its own
    /// entry. Without `x-kubernetes-list-type: map` server-side apply treats
    /// the array as atomic — one manager owns the whole thing and `force()`
    /// hands it over wholesale, silently discarding every other provider's
    /// row. That was a real, reproduced bug, not a theoretical one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["providerName", "providerNamespace"],
    ))]
    pub per_provider: Vec<ImagePerProviderStatus>,

    /// Progress of the shared, provider-agnostic image build for `Url`-kind
    /// sources — set exclusively by `banlieue-imagebuilder` (field manager
    /// `banlieue.io/imagebuilder`), never by a provider. Typed by `kind`
    /// (`cloudImage` for libvirt, `iso` for vSphere). `None` when no `Url`
    /// source exists on this `VMImage` or the build hasn't started. See
    /// ADR-0010 and ADR-0020.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_artifact: Option<BuildArtifactStatus>,

    /// `Ready` is True iff every per-provider entry is ready.
    ///
    /// Written **only** by `banlieue-controller` (field manager
    /// `banlieue.io/controller`), which is the only component with a
    /// whole-image view. A provider cannot compute "ready everywhere" from
    /// rows it does not own, so it writes its `perProvider` entry and nothing
    /// here (ADR-0015). Merge-keyed on `type`, per Kubernetes convention.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["type"],
    ))]
    pub conditions: Vec<Condition>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

/// Progress of the shared image build driven by `banlieue-imagebuilder` from a
/// `Url`-kind [`ImageSource`], via a kairos-operator `OSArtifact`
/// (`build.kairos.io/v1alpha2`). One build artifact per `VMImage`, regardless
/// of how many provider-class sources reference it — the OCI pull and build are
/// identical no matter which backend eventually imports the result. The
/// artifact is typed by [`BuildArtifactKind`]: a raw cloud image (libvirt) or a
/// bootable ISO (vSphere). See ADR-0010 and ADR-0020.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BuildArtifactStatus {
    /// What kind of artifact was built, aligned with kairos-operator's own
    /// `OSArtifactKind`. Determines the `file` extension and which provider
    /// class consumes it.
    pub kind: BuildArtifactKind,

    /// Current build phase.
    pub phase: BuildArtifactPhase,

    /// Name of the `OSArtifact` CR `banlieue-imagebuilder` created for this
    /// `VMImage` (same namespace as the artifacts PVC below).
    pub os_artifact_ref: String,

    /// `metadata.uid` of the `OSArtifact` named by `os_artifact_ref`, once
    /// observed. Each provider's per-zone import Job sets this as its own
    /// `ownerReference` so a rebuilt (deleted-and-recreated) `OSArtifact`
    /// garbage-collects the stale Job — and the artifacts PVC mount it
    /// holds — instead of the Job outliving it for up to its
    /// `ttlSecondsAfterFinished` (ADR-0027). Absent until the `OSArtifact`
    /// has actually been observed once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_artifact_uid: Option<String>,

    /// Reference to the PVC kairos-operator created holding the built artifact,
    /// once known. Populated no earlier than phase `Building`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pvc_ref: Option<LocalObjectReference>,

    /// File name of the artifact within the artifacts PVC (kairos-operator
    /// convention: `<osArtifactRef>.raw` for `cloudImage`, `<osArtifactRef>.iso`
    /// for `iso`). Populated at phase `Ready`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// Short reason, mirroring the stable-string convention used elsewhere
    /// in this status (e.g. `ImagePerProviderStatus.reason`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,

    /// Long human-readable detail, e.g. the `OSArtifact.status.message` on
    /// failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// Expected checksum (`<alg>:<hex>`) of the built artifact, copied from the
    /// `Url` source the build serves. Consumers that stream the artifact to a
    /// backend MUST verify it against this value and fail closed on mismatch
    /// (security review 2026-07-31, SEC-004) — the value lives here, next to
    /// the PVC reference, so no consumer has to re-derive which source the
    /// shared build came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

/// Kind of build artifact produced for a `VMImage`, aligned 1:1 with
/// kairos-operator's `OSArtifactKind` string values so the vocabulary is not
/// banlieue-invented. `cloudImage` is a raw cloud disk (consumed by libvirt);
/// `iso` is a bootable install ISO from `auroraboot build-iso` (consumed by
/// vSphere). See ADR-0020.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BuildArtifactKind {
    CloudImage,
    Iso,
}

/// Build phase of a [`BuildArtifactStatus`].
///
/// Deliberately a 4-state subset of kairos-operator's own
/// `OSArtifact.status.phase` (`Pending | Building | Exporting | Ready |
/// Error`): `banlieue-imagebuilder` maps `Exporting -> Building` and
/// `Error -> Failed` before writing this field, so consumers never need to
/// know about kairos-operator's own phase model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum BuildArtifactPhase {
    Pending,
    Building,
    Ready,
    Failed,
}

/// Readiness of a VMImage on one specific Provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImagePerProviderStatus {
    /// Name of the Provider.
    pub provider_name: String,
    /// Namespace of the Provider.
    pub provider_namespace: String,
    /// True when the image can be used to clone/create a VM on this provider.
    pub ready: bool,
    /// Resolved concrete reference on the backend.
    /// vSphere: `[datacenter] folder/template-name`. Proxmox: VMID. Libvirt: path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_ref: Option<String>,
    /// Short reason if not ready. Stable values from
    /// `condition_reasons::IMAGE_*`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Long human-readable detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Per-zone (per-`Provider.status.failureDomains[]`) import progress.
    /// Only populated for `Url`-kind sources, where "ready" on this Provider
    /// legitimately means "ready in some zones, still importing in others" —
    /// `Template` sources report readiness as a single vCenter-wide lookup
    /// and leave this empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zones: Vec<ZoneImageStatus>,
}

/// Import readiness of a `VMImage` in one failure domain (zone) of a
/// Provider. Only meaningful for `Url`-kind sources built by
/// `banlieue-imagebuilder` and imported per zone by the owning provider's
/// controller (e.g. `banlieue-provider-vsphere`, one zone == one vSphere
/// compute cluster / datastore / network in the current environment). See
/// ADR-0010.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ZoneImageStatus {
    /// Name of the failure domain, matching `Provider.status.failureDomains[].name`.
    pub name: String,
    /// True once the template/import is usable in this zone.
    pub ready: bool,
    /// The template's bare display name within this zone once ready — the
    /// value a provider passes to a name-based template lookup. NOT a
    /// decorated string (no `[dc]`/folder prefix): folder scoping for a
    /// per-zone (`Url`-kind) import lives in [`Self::template_folder`],
    /// kept separate so a lookup can be built from structured fields
    /// instead of parsing this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_ref: Option<String>,
    /// The vCenter folder path (relative to the datacenter's VM folder,
    /// e.g. `templates/cluster-01`) the template in [`Self::resolved_ref`]
    /// lives in, for a per-zone (`Url`-kind) import (ADR-0020 Decision #5).
    /// `None` for a `Template`-kind image, which has no per-zone folder —
    /// its `resolved_ref` is looked up datacenter-wide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_folder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[cfg(test)]
#[path = "vmimage_tests.rs"]
mod vmimage_tests;
