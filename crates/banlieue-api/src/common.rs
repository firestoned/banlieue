// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Common types shared across banlieue API groups.
//!
//! Most of these mirror CAPI shapes intentionally so that the
//! `infrastructure.banlieue.io` CRDs can satisfy the CAPI v1beta2 InfraMachine
//! contract while remaining usable standalone via `banlieue.io/VirtualMachine`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// CAPI v1beta2 initialization status block.
///
/// Replaces the deprecated v1beta1 `status.ready` field. Once
/// `provisioned == true`, the parent controller (CAPI Machine or banlieue
/// VirtualMachine) will surface `providerID`, `addresses`, and `failureDomain`
/// from the InfraMachine.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializationStatus {
    /// True when the infrastructure provider reports that the resource's
    /// infrastructure is fully provisioned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioned: Option<bool>,
}

/// CAPI v1beta2 `APIEndpoint` — the reachable address of a cluster's
/// Kubernetes API server.
///
/// Used as `VSphereCluster.spec.controlPlaneEndpoint` (operator-supplied
/// control-plane VIP) and echoed in `status.controlPlaneEndpoint`. The CAPI
/// contract marks the enclosing field optional; when present, both `host`
/// and `port` are meaningful.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiEndpoint {
    /// Hostname or IP on which the API server is serving.
    pub host: String,
    /// Port on which the API server is serving.
    pub port: i32,
}

/// CAPI v1beta2 `clusterv1.FailureDomain` — one element of an InfraCluster's
/// `status.failureDomains` list.
///
/// In v1beta2 failure domains are a **list** (the v1beta1 map was retired).
/// banlieue's `VSphereCluster` reconciler translates each selected
/// `Provider.status.failureDomains[]` entry into one of these, carrying the
/// banlieue FD `name` through, flattening provider attributes into
/// `attributes`, and setting `control_plane` from the cluster's
/// control-plane FD selector.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterFailureDomain {
    /// Unique failure-domain name (one of the Provider's
    /// `status.failureDomains[].name`).
    pub name: String,

    /// Whether this failure domain is eligible to run control-plane nodes.
    /// `None` is treated by CAPI as "not control-plane eligible".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_plane: Option<bool>,

    /// Arbitrary attributes for consumers. banlieue flattens the Provider FD's
    /// `attributes.raw` plus `dc`/`cluster` labels into this map.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}

/// A typed machine address. Mirrors CAPI's `clusterv1.MachineAddress`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MachineAddress {
    /// Address type. Accepted: Hostname, ExternalIP, InternalIP, ExternalDNS, InternalDNS.
    #[serde(rename = "type")]
    pub address_type: MachineAddressType,
    /// The address itself.
    pub address: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum MachineAddressType {
    Hostname,
    ExternalIP,
    InternalIP,
    ExternalDNS,
    InternalDNS,
}

/// Reference to an object in the same namespace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LocalObjectReference {
    pub name: String,
}

/// Typed reference (apiGroup + kind + name + optional namespace).
///
/// Used wherever the referenced kind is pluggable — e.g. IPAM pools, where we
/// want to accept either `ipam.cluster.x-k8s.io/IPAddressClaim` (CAPI's
/// default) or future banlieue-native pool types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TypedObjectReference {
    pub api_group: String,
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

/// Default key read from a ConfigMap / Secret referenced by a [`KeySelector`]
/// when `key` is omitted. Matches Kubernetes' own convention (`kube-root-ca.crt`
/// ConfigMap, service-account CA, webhook `caBundle` all key on `ca.crt`).
pub const DEFAULT_CA_BUNDLE_KEY: &str = "ca.crt";

/// Reference to a single key within a named object (ConfigMap or Secret) in the
/// same namespace as the referrer.
///
/// `key` is optional; callers that have a well-known default (e.g.
/// [`CABundleSource`], which defaults to [`DEFAULT_CA_BUNDLE_KEY`]) resolve it
/// via [`KeySelector::key_or`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KeySelector {
    /// Name of the ConfigMap / Secret in the referrer's namespace.
    pub name: String,
    /// Key within the object's `data`. Defaults are caller-defined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl KeySelector {
    /// The configured `key`, or `default` when omitted.
    pub fn key_or<'a>(&'a self, default: &'a str) -> &'a str {
        self.key.as_deref().unwrap_or(default)
    }
}

/// Source of a PEM-encoded CA bundle used to validate a backend's TLS
/// certificate. Exactly one of the three fields must be set.
///
/// - `inline` — PEM text directly in the spec (one or more concatenated certs).
/// - `config_map_ref` — a key in a ConfigMap in the referrer's namespace; the
///   common case for a centrally-managed, non-secret corporate trust bundle.
///   Key defaults to [`DEFAULT_CA_BUNDLE_KEY`].
/// - `secret_ref` — a key in a Secret in the referrer's namespace, for CA
///   material treated as sensitive. Key defaults to [`DEFAULT_CA_BUNDLE_KEY`].
///
/// Resolving the ConfigMap/Secret variants requires cluster access and lives in
/// the consuming controller; this type only models the spec and validates the
/// "exactly one" invariant via [`CABundleSource::validate`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CABundleSource {
    /// Inline PEM (one or more concatenated certificates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<String>,
    /// Key in a ConfigMap in the referrer's namespace (key defaults to `ca.crt`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_map_ref: Option<KeySelector>,
    /// Key in a Secret in the referrer's namespace (key defaults to `ca.crt`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<KeySelector>,
}

impl CABundleSource {
    /// Number of sources set. The "exactly one" invariant means a valid source
    /// has a count of `1`.
    pub fn source_count(&self) -> usize {
        usize::from(self.inline.is_some())
            + usize::from(self.config_map_ref.is_some())
            + usize::from(self.secret_ref.is_some())
    }

    /// Validate the "exactly one of inline / configMapRef / secretRef" invariant.
    ///
    /// # Errors
    /// Returns a static message when zero or more than one source is set, so the
    /// caller can surface it on status (controller-side) — the same rule a
    /// `ValidatingAdmissionPolicy` enforces at admission.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self.source_count() {
            1 => Ok(()),
            0 => Err(
                "caBundle: exactly one of inline, configMapRef, secretRef must be set (none were)",
            ),
            _ => Err(
                "caBundle: exactly one of inline, configMapRef, secretRef must be set (more than one was)",
            ),
        }
    }
}

/// Default key read from a Secret referenced by a [`CloudConfigSource`] when
/// `key` is omitted. Matches kairos-operator's own `cloudConfigRef` convention.
pub const DEFAULT_CLOUD_CONFIG_KEY: &str = "cloud-config.yaml";

/// Source of a default cloud-config baked into a built image artifact.
///
/// Mirrors [`CABundleSource`]'s value-or-source shape, but is **secretRef-first**
/// (ADR-0020): only `secret_ref` is implemented today, because kairos-operator's
/// `OSArtifact.spec.cloudConfigRef` is itself Secret-only and
/// `banlieue-imagebuilder` passes this Secret straight through. The `inline` and
/// `config_map_ref` variants are reserved for a later change (they will resolve
/// by materialising an imagebuilder-owned derived Secret); adding them is an
/// additive, non-breaking CRD change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CloudConfigSource {
    /// Key in a Secret in the imagebuild namespace holding the cloud-config
    /// YAML (key defaults to [`DEFAULT_CLOUD_CONFIG_KEY`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<KeySelector>,
}

impl CloudConfigSource {
    /// Number of sources set. The "exactly one" invariant means a valid source
    /// has a count of `1`.
    pub fn source_count(&self) -> usize {
        usize::from(self.secret_ref.is_some())
    }

    /// Validate the "exactly one source" invariant.
    ///
    /// # Errors
    /// Returns a static message when no source is set, so the caller can surface
    /// it on status. Today only `secretRef` exists; the message names the future
    /// variants so it stays accurate once they land.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self.source_count() {
            1 => Ok(()),
            0 => Err("cloudConfig: secretRef must be set (none was)"),
            _ => Err("cloudConfig: exactly one source must be set (more than one was)"),
        }
    }
}

/// Minimal LabelSelector mirroring the k8s `metav1.LabelSelector` shape.
///
/// We re-declare it here rather than re-exporting `k8s_openapi`'s type because
/// `kube-derive`'s schema generation produces slightly cleaner output for
/// hand-rolled types; functionally identical from a CRD consumer's point of
/// view.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelSelector {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub match_labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub match_expressions: Vec<LabelSelectorRequirement>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelSelectorRequirement {
    pub key: String,
    pub operator: LabelSelectorOperator,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum LabelSelectorOperator {
    In,
    NotIn,
    Exists,
    DoesNotExist,
}

/// Disk provisioning hint. Providers honor on a best-effort basis.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DiskProvisioning {
    #[default]
    Thin,
    Thick,
    EagerZeroed,
}

impl DiskProvisioning {
    /// Stable token (matches the serde camelCase wire form), for CLI args/logs.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            DiskProvisioning::Thin => "thin",
            DiskProvisioning::Thick => "thick",
            DiskProvisioning::EagerZeroed => "eagerZeroed",
        }
    }
}

impl std::str::FromStr for DiskProvisioning {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "thin" => Ok(Self::Thin),
            "thick" => Ok(Self::Thick),
            "eagerzeroed" => Ok(Self::EagerZeroed),
            other => Err(format!(
                "unknown disk type {other:?} (expected: thin, thick, eagerZeroed)"
            )),
        }
    }
}

/// Firmware type. Providers that don't support EFI fall back to BIOS with a
/// `PlacementValid=False` condition.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Firmware {
    Bios,
    #[default]
    Efi,
    EfiSecure,
}

impl Firmware {
    /// Stable token (matches the serde kebab-case wire form), for CLI args/logs.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Firmware::Bios => "bios",
            Firmware::Efi => "efi",
            Firmware::EfiSecure => "efi-secure",
        }
    }
}

impl std::str::FromStr for Firmware {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "bios" => Ok(Self::Bios),
            "efi" => Ok(Self::Efi),
            "efi-secure" | "efisecure" => Ok(Self::EfiSecure),
            other => Err(format!(
                "unknown firmware {other:?} (expected: bios, efi, efi-secure)"
            )),
        }
    }
}

/// Power state, used both for desired and observed.
///
/// Wire values are `PoweredOn` / `PoweredOff` / `Suspended` rather than
/// `On` / `Off` / `Suspended` to dodge YAML 1.1's implicit-boolean rule (Go's
/// YAML parser, used by the kube apiserver, otherwise reads bare `On`/`Off`
/// tokens as booleans and rejects the CRD schema).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum PowerState {
    #[default]
    PoweredOn,
    PoweredOff,
    Suspended,
}

/// Resolved IPAM configuration for a network interface (used in infra CRs
/// like `VSphereMachine`).
///
/// The IPAM mode is inferred from which optional field is set:
///
/// | Field present | Mode |
/// |---|---|
/// | `static` | Static |
/// | `pool` | Pool |
/// | neither | DHCP (default) |
///
/// Setting both `static` and `pool` is invalid — the controller rejects it.
///
/// ```yaml
/// ipam: {}                             # DHCP — nothing else needed
/// ipam:
///   static:
///     address: 10.0.0.5
///     prefix: 24
///     gateway: 10.0.0.1
/// ipam:
///   pool:
///     poolRef:
///       apiGroup: ipam.cluster.x-k8s.io
///       kind: IPAddressClaim
///       name: prod-pool
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IpamSpec {
    /// Static IPAM parameters (address, prefix, gateway, nameservers, domain).
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "static")]
    pub static_: Option<StaticIpamConfig>,

    /// Pool-based IPAM parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<PoolIpamConfig>,
}

impl IpamSpec {
    /// Derive the IPAM source from which optional field is set.
    ///
    /// Precedence: `static` > `pool` > DHCP.
    #[must_use]
    pub fn source(&self) -> IpamSource {
        if self.static_.is_some() {
            IpamSource::Static
        } else if self.pool.is_some() {
            IpamSource::Pool
        } else {
            IpamSource::Dhcp
        }
    }
}

/// IPAM source, derived from the presence of `static` or `pool` on
/// [`IpamSpec`] / [`IpamShape`].  Not serialized as a field — call
/// `.source()` to obtain.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum IpamSource {
    #[default]
    Dhcp,
    Static,
    Pool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StaticIpamConfig {
    pub address: String,
    pub prefix: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nameservers: Vec<String>,
    /// DNS domain, used both as a DNS search domain and (by a
    /// `VirtualMachine.spec.networkOverrides` consumer, ADR-0024) to build
    /// an FQDN as `<vm-name>.<domain>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

/// Shared subnet shape for a `VMClass`-level static IPAM declaration.
///
/// **Does NOT include a per-VM address.** A `VMClass` is shared by many VMs,
/// so a concrete address can only be expressed per-VM via
/// `VirtualMachine.spec.networkOverrides`. This struct captures the common
/// subnet parameters that all VMs on this interface share.
///
/// Every field is optional — the class only needs to declare the parameters
/// that are shared; per-VM overrides fill in the rest.
///
/// See also [`StaticIpamConfig`], which adds the per-VM `address` field
/// and is used in `NetworkInterfaceOverride` and the resolved infra CRs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StaticNetworkShape {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nameservers: Vec<String>,
    /// DNS domain, used both as a DNS search domain and (by a
    /// `VirtualMachine.spec.networkOverrides` consumer, ADR-0024) to build
    /// an FQDN as `<vm-name>.<domain>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

/// IPAM configuration for a `VMClass` network interface.
///
/// Like [`IpamSpec`] but its `static` variant uses [`StaticNetworkShape`]
/// (no per-VM address), since a class is shared by many VMs. The per-VM
/// address is provided via `VirtualMachine.spec.networkOverrides`.
///
/// The IPAM mode is inferred — see [`IpamSpec`] for the precedence table.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IpamShape {
    /// Shared subnet parameters (prefix, gateway, nameservers, domain) —
    /// **not** a per-VM address.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "static")]
    pub static_: Option<StaticNetworkShape>,

    /// Pool-based IPAM parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<PoolIpamConfig>,
}

impl IpamShape {
    /// Derive the IPAM source from which optional field is set.
    ///
    /// Precedence: `static` > `pool` > DHCP.
    #[must_use]
    pub fn source(&self) -> IpamSource {
        if self.static_.is_some() {
            IpamSource::Static
        } else if self.pool.is_some() {
            IpamSource::Pool
        } else {
            IpamSource::Dhcp
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PoolIpamConfig {
    pub pool_ref: TypedObjectReference,
}

/// Standard condition reasons used across banlieue CRDs. Centralized so
/// downstream tooling can match on stable strings.
pub mod condition_reasons {
    pub const VM_CREATED: &str = "VMCreated";
    pub const VM_RUNNING: &str = "VMRunning";
    pub const VM_STOPPED: &str = "VMStopped";
    pub const CLONING: &str = "Cloning";
    pub const POWERED_ON: &str = "PoweredOn";
    pub const POWERED_OFF: &str = "PoweredOff";
    pub const SCHEDULED: &str = "Scheduled";
    pub const SCHEDULING_FAILED: &str = "SchedulingFailed";
    pub const PLACEMENT_DRIFT: &str = "PlacementDrift";
    pub const PLACEMENT_VALID: &str = "PlacementValid";
    pub const MIGRATING: &str = "Migrating";
    pub const IMAGE_PENDING: &str = "ImagePending";
    pub const IMAGE_READY: &str = "ImageReady";
    pub const IMAGE_IMPORT_FAILED: &str = "ImageImportFailed";
    pub const IPAM_PENDING: &str = "IPAMPending";
    pub const IPAM_BOUND: &str = "IPAMBound";
}

/// Standard condition types used across banlieue CRDs.
pub mod condition_types {
    pub const READY: &str = "Ready";
    pub const INFRASTRUCTURE_READY: &str = "InfrastructureReady";
    pub const SCHEDULED: &str = "Scheduled";
    pub const PLACEMENT_VALID: &str = "PlacementValid";
    pub const MIGRATING: &str = "Migrating";
    pub const POWER_STATE: &str = "PowerState";
    pub const IMAGE_READY: &str = "ImageReady";
    pub const PROVIDER_REACHABLE: &str = "ProviderReachable";
}

#[cfg(test)]
#[path = "common_tests.rs"]
mod common_tests;
