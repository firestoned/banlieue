// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue.io/v1alpha1` VMImage CRD.
//!
//! Cluster-scoped image catalog. Each VMImage has one or more sources, each
//! mapped to a provider class. The image controller maintains per-provider
//! readiness in status by polling each registered Provider and (where
//! supported) importing the image on demand.

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

    /// Per-provider source mappings. At least one entry per ProviderClass
    /// you intend to schedule VMs onto.
    pub sources: Vec<ImageSource>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_provider: Vec<ImagePerProviderStatus>,

    /// `Ready` is True iff every per-provider entry is ready.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
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
}

#[cfg(test)]
#[path = "vmimage_tests.rs"]
mod vmimage_tests;
