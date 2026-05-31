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

/// Points at a Secret carrying the guest bootstrap payload (cloud-init /
/// ignition / sysprep), delivered into the guest per the image's `guestAgent`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserDataSpec {
    /// Secret in the VirtualMachine's namespace.
    pub secret_ref: LocalObjectReference,
    /// Key within the Secret containing the user-data blob.
    /// Default: `user-data`.
    #[serde(default = "default_userdata_key")]
    pub key: String,
}

fn default_userdata_key() -> String {
    "user-data".to_string()
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
