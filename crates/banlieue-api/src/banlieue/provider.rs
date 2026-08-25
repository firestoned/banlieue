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
/// Provider — one backend instance registered with banlieue.
///
/// A Provider represents a single place VMs can run: one vCenter, one Proxmox
/// cluster, or one libvirt host. It carries the connection details and an
/// admin-curated declaration of the storage classes, network classes, and
/// features that backend exposes. Its controller logs in, verifies those
/// capabilities, and publishes the reachable `status.failureDomains[]`.
///
/// # Why create one
///
/// - **Make a backend schedulable.** A VirtualMachine can only be placed on a
///   Provider — no Provider, nowhere to run.
/// - **Declare capabilities explicitly.** `spec.capabilities` maps abstract
///   class names (the ones VMClass / VMImage request) to concrete backend
///   targets (a datastore, a port group). That mapping is the contract the
///   scheduler matches against — capabilities are declared, not guessed.
/// - **Model many backends, including duplicates.** A cluster can hold many
///   Providers of the same class (`prod-vsphere`, `dr-vsphere`) and mix
///   classes freely.
///
/// The provider's controller talks to the backend; banlieue's main controller
/// never does. Communication between them is CRD-only.
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

    /// vSphere only: import `Url`-kind VMImages through a vCenter Content
    /// Library rather than the default datastore-upload + `MarkAsTemplate`
    /// path. Defaults to `false` (no Content Library required), matching
    /// environments where CL is not enabled. Ignored by non-vSphere classes.
    /// See ADR-0020.
    #[serde(default, skip_serializing_if = "is_false")]
    pub use_content_library: bool,

    /// Explicit overrides for individual discovered failure domains'
    /// generated `name`. The auto-computed, collision-safe name
    /// (`<provider>-<datacenter>-<cluster>`, hashed when too long) is
    /// always the fallback for any `(datacenter, cluster)` pair with no
    /// matching entry here — this is opt-in, never required. See ADR-0023.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["datacenter", "cluster"],
    ))]
    pub failure_domain_name_overrides: Vec<FailureDomainNameOverride>,
}

/// Explicit override for one discovered failure domain's generated `name`,
/// keyed by the `(datacenter, cluster)` pair `discover_inventory` resolves
/// it from. Named fields, not a `"dc/cluster"` string key, so a call site
/// can't accidentally swap them — same reasoning as `ImportJobIdentity`
/// (ADR-0020-era `import_job_name` fix). See ADR-0023.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FailureDomainNameOverride {
    /// Datacenter name as vCenter reports it (matches `discover_inventory`'s
    /// walk, not an operator-chosen alias).
    pub datacenter: String,
    /// Cluster name as vCenter reports it.
    pub cluster: String,
    /// The name to use instead of the auto-computed one, e.g. `cluster-01`.
    /// Slugified the same way auto-computed names are, so `Cluster 01`
    /// still produces a valid Kubernetes name.
    pub name: String,
}

/// How to reach a backend: endpoint, the Secret holding its credentials, and
/// TLS handling.
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

    /// Optional CA bundle to validate the endpoint's TLS certificate.
    ///
    /// A value-or-source: inline PEM, or a `configMapRef` / `secretRef` naming a
    /// key (default `ca.crt`) in the Provider's namespace. Exactly one source
    /// must be set; see [`CABundleSource`]. Resolved by the provider controller
    /// and injected into the HTTP client (ADR-0008, BYOC). When unset, the
    /// system trust roots are used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_bundle: Option<CABundleSource>,
}

/// The capability surface an admin asserts a backend exposes. The scheduler
/// matches VMClass / VMImage requests against these entries; the provider's
/// controller verifies them and reports per-failure-domain availability in
/// status.
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

/// A concrete backend target scoped to one specific failure domain — a
/// `(datacenter, cluster)` pair — within a Provider. Keyed the same way
/// `Provider.spec.failureDomainNameOverrides` is (ADR-0023): the
/// vCenter-reported identity, not a failure domain's own (possibly
/// admin-renamed) display name, which would make the mapping fragile to a
/// rename. Shared by [`StorageClassMapping`] and [`NetworkClassMapping`] —
/// same shape, same identity key (ADR-0030).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScopedTarget {
    /// Datacenter name as vCenter reports it.
    pub datacenter: String,
    /// Cluster name as vCenter reports it.
    pub cluster: String,
    /// Concrete backend target for this zone, same shape as
    /// [`StorageClassMapping::target`] / [`NetworkClassMapping::target`].
    pub target: BTreeMap<String, String>,
}

/// Maps one abstract storage-class name to a concrete backend target.
///
/// A single class name can resolve differently per failure domain of the
/// same Provider — the "same" storage tier commonly has a differently-named
/// datastore (cluster) on each vCenter cluster. `target` is the default,
/// applied to any zone not covered by `per_zone`; `per_zone` overrides it
/// for the zones it lists. At least one of the two must resolve a target
/// for a zone, or this class is simply unavailable there — not an error,
/// the same "not reported available" path an unmatched target already took
/// before this ADR (ADR-0019, ADR-0030).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorageClassMapping {
    /// Abstract name referenced by VMClass.hardware.disks[].storageClass.
    pub name: String,
    /// Default concrete target, used in any zone `per_zone` does not cover.
    /// `None` means this class resolves ONLY in the zones `per_zone` lists.
    /// Free-form per provider class; the provider's controller interprets
    /// it. Examples by provider class:
    ///   vsphere:  { datastore: "ds-fast-01" }
    ///             { datastoreCluster: "dsc-gold" }
    ///             { tagCategory: "tier", tag: "gold" }
    ///   proxmox:  { storage: "ceph-pool-1" }
    ///   libvirt:  { pool: "nvme-pool" }
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<BTreeMap<String, String>>,
    /// Per-`(datacenter, cluster)` overrides of `target` (ADR-0030).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_zone: Vec<ScopedTarget>,
}

impl StorageClassMapping {
    /// Effective concrete target for a given `(datacenter, cluster)`: an
    /// exact `per_zone` match wins, else the default `target`, else `None`
    /// when this class does not resolve in that zone at all (ADR-0030).
    #[must_use]
    pub fn target_for(&self, datacenter: &str, cluster: &str) -> Option<&BTreeMap<String, String>> {
        self.per_zone
            .iter()
            .find(|z| z.datacenter == datacenter && z.cluster == cluster)
            .map(|z| &z.target)
            .or(self.target.as_ref())
    }
}

/// Maps one abstract network-class name to a concrete backend target.
///
/// Same per-zone override shape as [`StorageClassMapping`] and for the same
/// reason: the "same" logical network commonly has a differently-named
/// port group on each cluster of a Provider (ADR-0030).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkClassMapping {
    /// Abstract name referenced by VMClass.network.interfaces[].networkClass.
    pub name: String,
    /// Default concrete target, used in any zone `per_zone` does not cover.
    /// `None` means this class resolves ONLY in the zones `per_zone` lists.
    /// Free-form per provider class. Examples:
    ///   vsphere:  { portGroup: "vmnet-prod" }
    ///             { distributedPortGroup: "dvs-prod-vlan100" }
    ///   proxmox:  { bridge: "vmbr0", vlan: "100" }
    ///   libvirt:  { network: "br-prod" }
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<BTreeMap<String, String>>,
    /// Per-`(datacenter, cluster)` overrides of `target` (ADR-0030).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_zone: Vec<ScopedTarget>,
    /// Default subnet shape (gateway/nameservers/domain) for this network
    /// class, used in any zone `per_zone_subnet` does not cover. A port
    /// group implies a subnet, so this lives alongside `target`/`per_zone`
    /// rather than on `VMClass` — it lets a static-addressing
    /// `VirtualMachine` omit gateway/nameservers/domain entirely and have
    /// them resolved from whichever zone the scheduler picked (ADR-0032).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<SubnetShape>,
    /// Per-`(datacenter, cluster)` overrides of `subnet` (ADR-0032).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_zone_subnet: Vec<ScopedSubnet>,
}

impl NetworkClassMapping {
    /// Effective concrete target for a given `(datacenter, cluster)`: an
    /// exact `per_zone` match wins, else the default `target`, else `None`
    /// when this class does not resolve in that zone at all (ADR-0030).
    #[must_use]
    pub fn target_for(&self, datacenter: &str, cluster: &str) -> Option<&BTreeMap<String, String>> {
        self.per_zone
            .iter()
            .find(|z| z.datacenter == datacenter && z.cluster == cluster)
            .map(|z| &z.target)
            .or(self.target.as_ref())
    }

    /// Effective subnet shape for a given `(datacenter, cluster)`: an exact
    /// `per_zone_subnet` match wins, else the default `subnet`, else `None`
    /// when this class declares no subnet info for that zone (ADR-0032).
    #[must_use]
    pub fn subnet_for(&self, datacenter: &str, cluster: &str) -> Option<&SubnetShape> {
        self.per_zone_subnet
            .iter()
            .find(|z| z.datacenter == datacenter && z.cluster == cluster)
            .map(|z| &z.subnet)
            .or(self.subnet.as_ref())
    }
}

/// A subnet's gateway/DNS/domain, scoped to one specific failure domain — a
/// `(datacenter, cluster)` pair — within a Provider's network class.
/// Deliberately excludes `prefix`, which stays a per-VM field on
/// `StaticIpamConfig` rather than becoming zone-derived (ADR-0032).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScopedSubnet {
    /// Datacenter name as vCenter reports it.
    pub datacenter: String,
    /// Cluster name as vCenter reports it.
    pub cluster: String,
    /// Subnet shape for this zone.
    pub subnet: SubnetShape,
}

/// Gateway/DNS/domain for a subnet, without a per-VM address. Used both as
/// [`NetworkClassMapping::subnet`]'s default and inside [`ScopedSubnet`]'s
/// per-zone overrides (ADR-0032). Mirrors the same three fields on
/// [`StaticNetworkShape`]/[`StaticIpamConfig`] — deliberately excludes
/// `prefix`, which stays per-VM.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubnetShape {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nameservers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

/// Observed state of a Provider: the failure domains its controller discovered
/// and the health / reachability conditions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    /// Failure domains ("availability zones" — the terms are synonyms;
    /// `failureDomain` was kept to align with CAPI v1beta2's own vocabulary)
    /// discovered by the provider's controller within this backend. The
    /// scheduler matches against `labels` and filters by
    /// `attributes.availableStorageClasses` / `availableNetworkClasses`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_domains: Vec<FailureDomain>,

    /// Standard Kubernetes conditions. The `Ready` condition reflects overall
    /// provider health. The `ProviderReachable` condition reflects connection
    /// state to the backend.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["type"],
    ))]
    pub conditions: Vec<Condition>,

    /// The provider workload `banlieue-operator` created for this Provider.
    ///
    /// Written **only** by the operator's field manager
    /// (`banlieue.io/operator`); the provider's own controller never touches
    /// it. This split is deliberate: `conditions` is a plain list with no
    /// `x-kubernetes-list-type: map` marker, so two field managers writing into
    /// it would contend over the whole array rather than merging per entry.
    /// Giving the operator a disjoint field keeps server-side apply
    /// conflict-free (ADR-0012).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload: Option<ProviderWorkloadStatus>,

    /// The generation of the spec that the controller has reconciled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

/// The per-instance provider workload created for a Provider (ADR-0003).
///
/// One Deployment per Provider, so a hung or slow backend cannot stall
/// reconciliation for any other and each pod holds exactly one backend's
/// credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderWorkloadStatus {
    /// Name of the Deployment running this Provider's controller.
    /// Conventionally `banlieue-provider-<class>-<provider-name>`.
    pub deployment_name: String,

    /// Namespace the Deployment was created in — the ProviderClass's
    /// `workloadNamespace`, or the operator's own namespace when unset.
    pub namespace: String,

    /// Ready replicas reported by that Deployment. Zero means the backend's
    /// controller is not currently running, whatever the Provider's other
    /// conditions say.
    pub ready_replicas: i32,

    /// The Provider generation the operator had observed when it last applied
    /// this workload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

/// One placement target within a backend — typically a (datacenter, cluster)
/// pair or a zone. The scheduler matches VMs to failure domains by `labels`
/// and filters by the capabilities resolved in `attributes`.
///
/// "Failure domain" and "availability zone" are synonyms here — this type
/// names it `failureDomain` to align with the CAPI v1beta2 vocabulary
/// (`clusterv1.FailureDomain`, `Machine.spec.failureDomain`) that banlieue's
/// infra CRDs are built to satisfy, not because it means something distinct
/// from an AZ. Docs and CLI help text are free to say "availability zone"
/// where that reads more naturally.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FailureDomain {
    /// Stable name. Conventionally `<provider>-<cluster-or-zone>`.
    pub name: String,

    /// Labels used by the scheduler's `failureDomainSelector` and by
    /// VirtualMachine anti-affinity `topologyKey` matching.
    /// Recommended keys: `datacenter`, `cluster`, `rack`, `env`. Every
    /// provider also sets `name` to this failure domain's own resolved
    /// `name` above (auto-computed, or an ADR-0023 override) — `name` above
    /// is a top-level field a `LabelSelector` cannot match directly, so
    /// without this mirror, targeting a specific zone by its friendly
    /// override name (rather than a raw backend-reported label like
    /// `cluster`) would be impossible.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,

    /// Attributes the provider's controller resolved for this domain,
    /// including the subset of admin-listed classes that are actually
    /// reachable from here.
    #[serde(default)]
    pub attributes: FailureDomainAttributes,
}

/// The capabilities and provider-resolved details actually reachable from a
/// failure domain. Always a subset of what the Provider spec advertises.
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
