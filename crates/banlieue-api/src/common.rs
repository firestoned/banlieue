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

/// IPAM configuration for a network interface.
///
/// `source` selects the strategy and the matching sibling field
/// (`static` / `pool`) carries its parameters. `Dhcp` needs no parameters.
///
/// Wire shape (Kubernetes-idiomatic; chosen over a serde-tagged enum
/// because kube-derive's CRD schema flattener does not support per-variant
/// discriminator subschemas — see `.wolf/buglog.json` bug-006):
///
/// ```yaml
/// ipam:
///   source: dhcp                       # nothing else needed
/// ipam:
///   source: static
///   static:
///     address: 10.0.0.5
///     prefix: 24
///     gateway: 10.0.0.1
/// ipam:
///   source: pool
///   pool:
///     poolRef:
///       apiGroup: ipam.cluster.x-k8s.io
///       kind: IPAddressClaim
///       name: prod-pool
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IpamSpec {
    pub source: IpamSource,

    /// Required when `source == Static`; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "static")]
    pub static_: Option<StaticIpamConfig>,

    /// Required when `source == Pool`; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<PoolIpamConfig>,
}

/// IPAM source. `Dhcp` is the default so a freshly-constructed `IpamSpec`
/// is a valid one.
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
