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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub enum PowerState {
    #[default]
    On,
    Off,
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
