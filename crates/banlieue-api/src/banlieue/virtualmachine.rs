// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue.io/v1alpha1` VirtualMachine CRD.
//!
//! The user-facing CR. Expresses intent: which class, which image, where
//! to place the VM, and what power state to maintain. The banlieue
//! controller schedules it onto a Provider + failure domain, creates the
//! provider-specific infrastructure CR (e.g. `VSphereMachine`), and mirrors
//! the infra CR's status back here.
//!
//! Per design choice: placement is **not** sticky after creation. The
//! scheduler re-evaluates on each reconcile. The `migrationPolicy` field
//! controls whether drift is acted on automatically.

use crate::common::*;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "banlieue.io",
    version = "v1alpha1",
    kind = "VirtualMachine",
    plural = "virtualmachines",
    shortname = "vm",
    namespaced,
    status = "VirtualMachineStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"Class","type":"string","jsonPath":".spec.classRef.name"}"#,
    printcolumn = r#"{"name":"Image","type":"string","jsonPath":".spec.imageRef.name"}"#,
    printcolumn = r#"{"name":"Provider","type":"string","jsonPath":".status.scheduled.providerName"}"#,
    printcolumn = r#"{"name":"FailureDomain","type":"string","jsonPath":".status.scheduled.failureDomain","priority":1}"#,
    printcolumn = r#"{"name":"Power","type":"string","jsonPath":".status.observedPowerState"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type=='Ready')].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
/// VirtualMachine — the user-facing request for a running VM.
///
/// This is the one resource end users create. It expresses *intent*: which
/// VMClass (shape) and VMImage (OS) to use, optional placement constraints,
/// the desired power state, and optional guest user-data. banlieue's
/// controller schedules it onto a Provider + failure domain, creates the
/// matching provider infrastructure CR (e.g. `VSphereMachine`), and mirrors
/// that CR's status back here.
///
/// # Why create one
///
/// - **Declare a VM the Kubernetes way.** Describe the VM you want; the
///   controller reconciles reality toward it, including power state.
/// - **Stay backend-agnostic.** You reference a class and an image by name,
///   not a datastore or a port group. Where it lands is the scheduler's job.
/// - **Compose with policy.** Label / anti-affinity selectors and a migration
///   policy steer placement and drift handling without coupling to a specific
///   Provider.
///
/// Independent of Cluster API: a VirtualMachine is **not** a `clusterv1.
/// Machine`. It can coexist with CAPI but does not depend on it.
///
/// Namespaced: candidate Providers are drawn from the VM's own namespace.
pub struct VirtualMachineSpec {
    /// Reference to a (cluster-scoped) VMClass.
    pub class_ref: LocalObjectReference,

    /// Reference to a (cluster-scoped) VMImage.
    pub image_ref: LocalObjectReference,

    /// Placement intent. If unset, the scheduler considers every Provider
    /// in the VM's namespace and every failure domain.
    #[serde(default)]
    pub placement: PlacementSpec,

    /// Desired power state. Defaults to `PoweredOn`.
    #[serde(default = "default_power_on")]
    pub desired_power_state: PowerState,

    /// Optional user-data delivered to the guest via the image's
    /// `guestAgent` (cloud-init / ignition / sysprep).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_data: Option<UserDataSpec>,

    /// What to do when current placement no longer satisfies the spec.
    #[serde(default)]
    pub migration_policy: MigrationPolicy,

    /// Suspend reconciliation in-band.
    #[serde(default, skip_serializing_if = "is_false")]
    pub paused: bool,

    /// Per-VM overrides for specific VMClass-declared network interfaces
    /// (ADR-0024). Keyed by `NetworkInterfaceSpec.name`; an interface with
    /// no entry here uses its VMClass's own `ipam` verbatim (commonly
    /// `dhcp`). Lets many VMs share one VMClass while each still gets its
    /// own static address — a VMClass-level `ipam.static` cannot express
    /// that, since a class is shared by design.
    ///
    /// **This is a delta, not the primary definition.** The VMClass is the
    /// authoritative source for the VM's network shape. Entries here are
    /// layered on top: only the named interface's `ipam` is replaced;
    /// every other interface is inherited from the class unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["name"],
    ))]
    pub network_overrides: Vec<NetworkInterfaceOverride>,

    /// Per-VM override for the `VMClass`'s hardware shape — CPUs, memory,
    /// and disk sizes.
    ///
    /// **This is a delta, not the primary definition.** The `VMClass` is the
    /// authoritative source for a VM's hardware shape: its `spec.hardware`
    /// is fixed and shared by every VM that references the class. This
    /// field applies *on top of* the class — only the fields you set here
    /// replace the class value; everything else is inherited verbatim.
    ///
    /// Use this sparingly. Its primary purpose is to accommodate the rare
    /// VM that genuinely needs a different CPU, memory, or disk budget than
    /// its class defines — for example, a database primary bumped to 16 CPUs
    /// while all other replicas use the 4-CPU class shape, or one VM that
    /// needs a larger data disk. If you find yourself setting the same
    /// override on every VM of a given class, create a new `VMClass` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware_override: Option<HardwareOverride>,

    /// Destination folder for the provisioned VM (e.g. `apps/prod` on
    /// vSphere). When unset, the provider defaults to organizing the VM
    /// the same way it organizes its source template — on vSphere, the
    /// same per-zone folder the template lives in (ADR-0020 Decision #5).
    ///
    /// Unlike `networkOverrides` / `hardwareOverride`, this has no
    /// `VMClass`-level counterpart to be a delta *against* — placement is
    /// purely an infrastructure concern, not part of a VM's abstract
    /// shape. It is also the one field here that is unavoidably
    /// backend-flavored (a "folder" is a vSphere concept); other backends
    /// may interpret it differently or ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

/// Per-VM override for the hardware shape declared by the `VMClass`.
///
/// **This is a delta, not the primary definition.** The `VMClass` owns the
/// canonical hardware shape (`spec.hardware`). This struct holds only the
/// values that deviate from that shape for a specific VM. Absent fields
/// are inherited from the class unchanged.
///
/// Named `HardwareOverride` (rather than reusing `HardwareSpec`) to make
/// the layered relationship explicit in the type name: a reader can
/// distinguish "the authoritative class definition" from "this VM's
/// per-instance delta" without consulting the field docs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HardwareOverride {
    /// Override the `VMClass`'s `spec.hardware.cpus`.
    /// If absent, the class value is used unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 256))]
    pub cpus: Option<u32>,

    /// Override the `VMClass`'s `spec.hardware.memoryMiB`.
    /// If absent, the class value is used unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 128, max = 4_194_304))]
    pub memory_mi_b: Option<u32>,

    /// Per-disk size overrides, keyed by `DiskSpec.name`.
    /// Only `sizeGiB` can be overridden per VM; the disk's `storageClass`
    /// and `provisioning` are class-level concerns.
    ///
    /// **This is a delta, not the primary definition.** A disk with no
    /// entry here inherits the `VMClass`'s size verbatim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["name"],
    ))]
    pub disk_overrides: Vec<DiskOverride>,
}

/// A per-VM size override for one `VMClass`-declared disk.
///
/// **This is a delta, not the primary definition.** The `VMClass` defines
/// the disk set; this struct lets a single VM request a larger size for
/// one of those disks (e.g. a bigger data volume) without altering the
/// shared class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DiskOverride {
    /// Matches a `VMClass.spec.hardware.disks[].name`.
    pub name: String,
    /// Override the disk's `sizeGiB`. Must be ≥ the class value (the
    /// provider will reject a shrink). If absent, the class size is used.
    #[schemars(range(min = 1, max = 65_536))]
    pub size_gi_b: u32,
}

/// A per-VM static-address override for one `VMClass`-declared network
/// interface (ADR-0024).
///
/// **This is a delta, not the primary definition.** The `VMClass` is the
/// authoritative source for the VM's network shape. An entry here replaces
/// only the named interface's `ipam`; every other interface, and all other
/// attributes of the named interface, are inherited from the class unchanged.
///
/// Named `NetworkInterfaceOverride` (rather than reusing
/// `NetworkInterfaceSpec`) for the same reason as `HardwareOverride`: the
/// name signals layered intent — "a per-VM delta on top of a shared class"
/// — rather than a standalone definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInterfaceOverride {
    /// Matches a `VMClass.spec.network.interfaces[].name`.
    pub name: String,
    /// The static address to use for this interface, overriding whatever
    /// the `VMClass`'s own `ipam` declares.
    #[serde(rename = "static")]
    pub static_: StaticIpamConfig,
}

fn default_power_on() -> PowerState {
    PowerState::PoweredOn
}

/// Optional constraints that narrow where a VirtualMachine may be placed.
/// When empty, every Provider in the VM's namespace and every failure domain
/// is a candidate.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlacementSpec {
    /// Match Providers by their `metadata.labels`. A Provider is a candidate
    /// only if its labels match this selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_selector: Option<LabelSelector>,

    /// Match failure domains by their `status.failureDomains[].labels`.
    /// Across all candidate Providers, only failure domains whose labels
    /// match are considered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain_selector: Option<LabelSelector>,

    /// Anti-affinity rules against other VirtualMachines in the same
    /// namespace. Evaluated at scheduling time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anti_affinity: Vec<AntiAffinityRule>,
}

/// A rule that spreads this VM away from other VirtualMachines across a
/// failure-domain topology key, evaluated at scheduling time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AntiAffinityRule {
    /// A label key from the failure domain's labels. Spreading is required
    /// across distinct values of this key.
    /// Common keys: `cluster`, `rack`, `host`, `dc`.
    pub topology_key: String,
    /// Other VMs (by their own metadata.labels) to spread away from.
    pub label_selector: LabelSelector,
    /// Strictness. `required` filters candidates; `preferred` is best-effort.
    #[serde(default)]
    pub mode: AffinityMode,
}

/// Strictness of an [`AntiAffinityRule`]: `Required` filters candidates,
/// `Preferred` is best-effort.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AffinityMode {
    #[default]
    Required,
    Preferred,
}

/// Default key read from a Secret or ConfigMap referenced by a [`UserDataSpec`]
/// when `key` is omitted.
pub const DEFAULT_USER_DATA_KEY: &str = "user-data";

/// Source of guest bootstrap data (cloud-init / ignition / sysprep), delivered
/// into the guest per the image's `guestAgent`.
///
/// Mirrors [`CABundleSource`](crate::common::CABundleSource)'s value-or-source
/// pattern. Exactly one of the two fields must be set:
///
/// - `secret_ref` — a key in a Secret (key defaults to
///   [`DEFAULT_USER_DATA_KEY`]).
/// - `config_map_ref` — a key in a ConfigMap (key defaults to
///   [`DEFAULT_USER_DATA_KEY`]). Use for non-sensitive bootstrap data (e.g.
///   cloud-config without secrets).
///
/// Setting both or neither is invalid — the controller rejects it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserDataSpec {
    /// Key in a Secret in the VirtualMachine's namespace (key defaults to
    /// [`DEFAULT_USER_DATA_KEY`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<KeySelector>,
    /// Key in a ConfigMap in the VirtualMachine's namespace (key defaults to
    /// [`DEFAULT_USER_DATA_KEY`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_map_ref: Option<KeySelector>,
}

impl UserDataSpec {
    /// Number of sources set. The "exactly one" invariant means a valid source
    /// has a count of `1`.
    pub fn source_count(&self) -> usize {
        usize::from(self.secret_ref.is_some()) + usize::from(self.config_map_ref.is_some())
    }

    /// Validate the "exactly one of secretRef / configMapRef" invariant.
    ///
    /// # Errors
    /// Returns a static message when zero or more than one source is set, so
    /// the caller can surface it on status.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self.source_count() {
            1 => Ok(()),
            0 => Err("userData: exactly one of secretRef, configMapRef must be set (none were)"),
            _ => Err(
                "userData: exactly one of secretRef, configMapRef must be set (more than one was)",
            ),
        }
    }
}

/// Policy for handling placement drift.
///
/// Because placement is non-sticky by design, the scheduler runs on every
/// reconcile. This field controls whether drift causes an action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MigrationPolicy {
    /// Surface drift via `PlacementValid=False` and migrate automatically.
    /// Live-migrate if both source and target failure domains support it
    /// (and the provider class supports cross-domain migration); otherwise
    /// recreate the VM on the new placement. Default.
    #[default]
    Automatic,
    /// Surface drift via `PlacementValid=False` but do NOT act. Migration
    /// is triggered manually by adding the annotation
    /// `banlieue.io/migrate=true` to the VirtualMachine.
    Manual,
    /// Never re-evaluate after initial scheduling. Sticky behavior.
    Never,
}

/// Observed state of a VirtualMachine: the scheduling decision, the infra CR
/// it owns, mirrored provisioning / address / power state, and conditions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VirtualMachineStatus {
    /// Current scheduling decision. Absent until first successful schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled: Option<ScheduledPlacement>,

    /// Reference to the provider-specific infrastructure CR
    /// (e.g. `infrastructure.banlieue.io/v1alpha1/VSphereMachine`).
    /// Set after scheduling, owned by this VirtualMachine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infrastructure_ref: Option<TypedObjectReference>,

    /// Mirrored from the infra CR's `status.initialization`.
    #[serde(default)]
    pub initialization: InitializationStatus,

    /// Mirrored from the infra CR's `status.addresses`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<MachineAddress>,

    /// Observed power state from the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_power_state: Option<PowerState>,

    /// Standard Kubernetes conditions. Required types:
    ///   `Ready`               — overall readiness
    ///   `Scheduled`           — placement decision exists and is current
    ///   `PlacementValid`      — current placement satisfies the spec
    ///   `InfrastructureReady` — mirrors the infra CR's Ready condition
    /// Optional:
    ///   `Migrating`           — true while a migration is in progress
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["type"],
    ))]
    pub conditions: Vec<Condition>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

/// The scheduler's current placement decision for a VirtualMachine, with the
/// abstract storage / network classes resolved to concrete backend identifiers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledPlacement {
    /// Provider name (in the VM's namespace).
    pub provider_name: String,
    /// Provider's ProviderClass (denormalized for convenience in printer columns).
    pub provider_class: String,
    /// Failure domain name (one of the Provider's `status.failureDomains[].name`).
    pub failure_domain: String,
    /// Resolved storage class → concrete backend identifier mappings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_storage: Vec<ResolvedResource>,
    /// Resolved network class → concrete backend identifier mappings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_networks: Vec<ResolvedResource>,
    /// Time the placement decision was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<Time>,
}

/// One abstract class → concrete backend identifier mapping resolved at
/// schedule time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedResource {
    /// Class name as referenced in the VMClass (e.g. "gold", "prod").
    pub class_name: String,
    /// Backend identifier the provider resolved to (e.g. "ds-fast-01", "vmnet-prod").
    pub backend_id: String,
}

#[inline]
fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
#[path = "virtualmachine_tests.rs"]
mod virtualmachine_tests;
