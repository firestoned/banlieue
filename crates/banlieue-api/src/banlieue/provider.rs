// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue.io/v1alpha1` Provider CRD.
//!
//! A Provider represents one backend instance: one vCenter, one Proxmox
//! cluster, one libvirt host (or libvirtd endpoint). It carries the
//! connection details and the admin-curated list of storage and network
//! classes that this backend exposes.
//!
//! Capability discovery is explicit by design: the admin lists every
//! storage class and network class along with the concrete backend target,
//! and the provider's controller verifies them and reports per-failure-domain
//! availability in status.

use crate::common::*;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "banlieue.io",
    version = "v1alpha1",
    kind = "Provider",
    plural = "providers",
    shortname = "prov",
    namespaced,
    status = "ProviderStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"Class","type":"string","jsonPath":".spec.providerClassRef.name"}"#,
    printcolumn = r#"{"name":"Endpoint","type":"string","jsonPath":".spec.connection.endpoint","priority":1}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type=='Ready')].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSpec {
    /// Reference to a ProviderClass that identifies the backend type.
    ///
    /// For v1alpha1 the ProviderClass CRD is deferred; treat this as a name
    /// drawn from a well-known set: `vsphere`, `proxmox`, `libvirt`. A future
    /// ProviderClass CRD will provide install metadata (image, RBAC) without
    /// changing this reference.
    pub provider_class_ref: LocalObjectReference,

    /// Connection details for the backend.
    pub connection: ProviderConnection,

    /// Admin-defined capability mappings. Every storage / network class that
    /// VMClass and VMImage may request MUST be listed here for this provider
    /// to be considered by the scheduler.
    #[serde(default, skip_serializing_if = "ProviderCapabilities::is_empty")]
    pub capabilities: ProviderCapabilities,

    /// Suspend reconciliation. Equivalent to setting the
    /// `cluster.x-k8s.io/paused` annotation but in-band.
    #[serde(default, skip_serializing_if = "is_false")]
    pub paused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConnection {
    /// Endpoint URL or URI. Format depends on provider class:
    ///   vsphere:  https://vcenter.example.com/sdk
    ///   proxmox:  https://pve.example.com:8006
    ///   libvirt:  qemu+ssh://kvm-host.example.com/system
    pub endpoint: String,

    /// Reference to a Secret in the Provider's namespace containing the
    /// credentials. Required keys depend on provider class:
    ///   vsphere:  username, password
    ///   proxmox:  username (root@pam!token-id), tokenValue  OR  username, password
    ///   libvirt:  optional sshPrivateKey for SSH transports
    pub credentials_ref: LocalObjectReference,

    /// Skip TLS verification. Applies to vsphere and proxmox.
    ///
    /// Serialized as `insecureSkipTLSVerify` (matching CAPI convention with
    /// uppercase `TLS`), not the auto-derived `insecureSkipTlsVerify`.
    #[serde(
        default,
        skip_serializing_if = "is_false",
        rename = "insecureSkipTLSVerify"
    )]
    pub insecure_skip_tls_verify: bool,

    /// Optional PEM-encoded CA bundle to validate the endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_bundle: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    /// Storage classes the admin asserts are available on this backend.
    /// Each entry maps an abstract class name to a provider-interpreted
    /// concrete target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storage_classes: Vec<StorageClassMapping>,

    /// Network classes the admin asserts are available on this backend.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_classes: Vec<NetworkClassMapping>,

    /// Feature flags admin asserts are available. Provider's controller may
    /// downgrade these in status if introspection finds otherwise.
    /// Well-known values: hotAddCPU, hotAddMemory, efiSecureBoot,
    /// nestedVirtualization, gpuPassthrough.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

impl ProviderCapabilities {
    pub fn is_empty(&self) -> bool {
        self.storage_classes.is_empty()
            && self.network_classes.is_empty()
            && self.features.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorageClassMapping {
    /// Abstract name referenced by VMClass.hardware.disks[].storageClass.
    pub name: String,
    /// Concrete backend target. Free-form per provider class; the provider's
    /// controller interprets it. Examples by provider class:
    ///   vsphere:  { datastore: "ds-fast-01" }
    ///             { datastoreCluster: "dsc-gold" }
    ///             { tagCategory: "tier", tag: "gold" }
    ///   proxmox:  { storage: "ceph-pool-1" }
    ///   libvirt:  { pool: "nvme-pool" }
    pub target: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkClassMapping {
    /// Abstract name referenced by VMClass.network.interfaces[].networkClass.
    pub name: String,
    /// Concrete backend target. Free-form per provider class. Examples:
    ///   vsphere:  { portGroup: "vmnet-prod" }
    ///             { distributedPortGroup: "dvs-prod-vlan100" }
    ///   proxmox:  { bridge: "vmbr0", vlan: "100" }
    ///   libvirt:  { network: "br-prod" }
    pub target: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    /// Failure domains discovered by the provider's controller within this
    /// backend. The scheduler matches against `labels` and filters by
    /// `attributes.availableStorageClasses` / `availableNetworkClasses`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_domains: Vec<FailureDomain>,

    /// Standard Kubernetes conditions. The `Ready` condition reflects overall
    /// provider health. The `ProviderReachable` condition reflects connection
    /// state to the backend.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,

    /// The generation of the spec that the controller has reconciled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FailureDomain {
    /// Stable name. Conventionally `<provider>-<cluster-or-zone>`.
    pub name: String,

    /// Labels used by the scheduler's `failureDomainSelector` and by
    /// VirtualMachine anti-affinity `topologyKey` matching.
    /// Recommended keys: `dc`, `cluster`, `rack`, `env`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,

    /// Attributes the provider's controller resolved for this domain,
    /// including the subset of admin-listed classes that are actually
    /// reachable from here.
    #[serde(default)]
    pub attributes: FailureDomainAttributes,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FailureDomainAttributes {
    /// Subset of spec.capabilities.storageClasses[].name reachable here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_storage_classes: Vec<String>,

    /// Subset of spec.capabilities.networkClasses[].name reachable here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_network_classes: Vec<String>,

    /// Feature flags actually present here. Always a subset of
    /// spec.capabilities.features.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,

    /// Provider-specific resolved attributes; for vSphere this typically
    /// includes datacenter, cluster, resourcePool. Used by the provider's
    /// controller when filling in the infrastructure CR.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub raw: BTreeMap<String, String>,
}

#[inline]
fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;
