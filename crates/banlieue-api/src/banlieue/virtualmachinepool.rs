// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue.io/v1alpha1` VirtualMachinePool CRD (roadmap 70, ADR-0046).
//!
//! A pool keeps a number of *already-installed, never-used* VirtualMachines
//! warm so that a consumer does not wait out a Deferred-mode install
//! (ADR-0040) at request time. A claim takes exactly one warm member out of
//! the pool, for exactly one subject, exactly once: a claimed member is
//! deleted when its claim ends and is never returned to the warm set.
//!
//! Both kinds are backend-neutral on purpose. A pool member is an ordinary
//! `VirtualMachine`; the scheduler, the infra CRs and every provider are
//! unaware pools exist. There is deliberately no `VSpherePool` /
//! `LibvirtPool` infra kind: nothing about pooling is provider-specific, and
//! adding one per backend would triple the work for no behavior.

use crate::banlieue::VirtualMachineSpec;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Label on every member `VirtualMachine`: the owning pool's name.
pub const LABEL_POOL: &str = "banlieue.io/pool";
/// Label on a member: digest of the `VMImage` build it was created from.
/// Compared for equality against the pool's current revision to find stale
/// members (nightly image rebuilds).
pub const LABEL_POOL_IMAGE_REVISION: &str = "banlieue.io/pool-image-revision";
/// Label on a member once bound: the `VirtualMachineClaim`'s name. Presence
/// of this label is what "claimed" means.
///
/// Defined here rather than with the claim (ADR-0047) because the **pool**
/// is what must never touch a member carrying it: invariant 1 of
/// [`super::super::...`]'s planner — a claimed member is deleted only by its
/// claim ending — is expressed by reading this label.
pub const LABEL_CLAIM: &str = "banlieue.io/claim";
/// Default for [`VirtualMachinePoolSpec::max_surge`].
pub const DEFAULT_POOL_MAX_SURGE: u32 = 2;
/// Default for [`VirtualMachinePoolSpec::provisioning_timeout_seconds`].
/// Generous on purpose: a Deferred-mode member is not Ready until a full
/// unattended install plus a reboot have finished (ADR-0040).
pub const DEFAULT_POOL_PROVISIONING_TIMEOUT_SECS: u64 = 1800;

pub mod pool_condition_types {
    /// `True` when `status.available >= spec.warmReplicas`.
    pub const WARM: &str = "Warm";
    /// `False` when the planner wanted members it could not create.
    pub const CAPACITY: &str = "Capacity";
    /// `True` once the claim has a member and that member is Ready.
    pub const BOUND: &str = "Bound";
}

pub mod pool_condition_reasons {
    pub const FILLING: &str = "Filling";
    pub const WARM: &str = "Warm";
    pub const MAX_REPLICAS_REACHED: &str = "MaxReplicasReached";
    pub const ADDRESS_RANGE_EXHAUSTED: &str = "AddressRangeExhausted";
    /// No member has ever published the condition named by `spec.readiness`
    /// (ADR-0046 Decision 3). A pool stuck at zero must say why, and the
    /// most likely cause is `GuestReady` before ADR-0043 exists.
    pub const READINESS_SIGNAL_ABSENT: &str = "ReadinessSignalAbsent";
    pub const NO_MEMBER_AVAILABLE: &str = "NoMemberAvailable";
    pub const POOL_NOT_FOUND: &str = "PoolNotFound";
    pub const EXPIRED: &str = "Expired";
    pub const MEMBER_LOST: &str = "MemberLost";
}

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "banlieue.io",
    version = "v1alpha1",
    kind = "VirtualMachinePool",
    plural = "virtualmachinepools",
    shortname = "vmpool",
    namespaced,
    status = "VirtualMachinePoolStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"Warm","type":"integer","jsonPath":".spec.warmReplicas"}"#,
    printcolumn = r#"{"name":"Available","type":"integer","jsonPath":".status.available"}"#,
    printcolumn = r#"{"name":"Provisioning","type":"integer","jsonPath":".status.provisioning"}"#,
    printcolumn = r#"{"name":"Claimed","type":"integer","jsonPath":".status.claimed"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
/// VirtualMachinePool: a self-refilling set of warm, single-use VMs.
///
/// # Why create one
///
/// - **Hide install latency.** A `tpmEnabled` class must pair with an
///   `installMode: Deferred` image, so every VM pays a full install on first
///   boot. The pool pays that ahead of time.
/// - **Single use by construction.** Members are handed out through
///   `VirtualMachineClaim` and destroyed when the claim ends. There is no
///   code path that returns a used member to the warm set.
/// - **Follow the image.** When the referenced `VMImage` is rebuilt, warm
///   members from the old build are replaced surge-style without dropping
///   claimable capacity.
///
/// Namespaced: members are created in the pool's own namespace, which is
/// also where the scheduler looks for candidate Providers.
pub struct VirtualMachinePoolSpec {
    /// How many Ready, unclaimed members to keep available.
    pub warm_replicas: u32,

    /// Hard ceiling on members of any phase, claimed ones included. Leave
    /// headroom above `warmReplicas` plus expected concurrent claims, or
    /// image rollouts have to trade warm capacity for replacements.
    pub max_replicas: u32,

    /// Most members allowed to be provisioning at once. Bounds the install
    /// load a refill puts on the hosts that claimed members are running on.
    #[serde(default = "default_max_surge")]
    pub max_surge: u32,

    /// Template for each member. `spec.networkOverrides` entries for the
    /// interface named in `addressing` are replaced per member; everything
    /// else is copied verbatim.
    pub template: VirtualMachineTemplate,

    /// Per-member static addressing. Omit for DHCP or class-level IPAM.
    /// Interim until the CAPI IPAM contract lands (ADR-0033); at that point
    /// this gains a `poolRef` alternative and the inline range stays as the
    /// zero-dependency option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addressing: Option<PoolAddressing>,

    /// Which member condition makes it claimable. See [`PoolReadiness`].
    ///
    /// **Required, with no default** (ADR-0046 Decision 2). `GuestReady` is
    /// the correct answer for a Deferred-mode image and would be the natural
    /// default — but nothing publishes that condition until ADR-0043 lands,
    /// so defaulting to it would leave every pool at zero warm members
    /// forever, with no error. A field whose wrong value produces silence
    /// rather than a failure is one the operator has to state out loud.
    pub readiness: PoolReadiness,

    /// A member still provisioning after this long is treated as poisoned
    /// and deleted, never repaired.
    #[serde(default = "default_provisioning_timeout")]
    pub provisioning_timeout_seconds: u64,

    /// Replace a Ready member that has sat unclaimed this long. Bounds how
    /// stale an unpatched warm VM can get between image rebuilds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_idle_seconds: Option<u64>,

    /// Replace warm members when the referenced `VMImage`'s build changes.
    #[serde(default = "default_true")]
    pub recycle_on_image_change: bool,
}

fn default_max_surge() -> u32 {
    DEFAULT_POOL_MAX_SURGE
}
fn default_provisioning_timeout() -> u64 {
    DEFAULT_POOL_PROVISIONING_TIMEOUT_SECS
}
fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VirtualMachineTemplate {
    /// Extra labels stamped on each member, in addition to the pool's own.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Extra annotations stamped on each member.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
    pub spec: VirtualMachineSpec,
}

/// Which signal makes a member claimable.
///
/// Deliberately has no `Default`: see [`VirtualMachinePoolSpec::readiness`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum PoolReadiness {
    /// The member's `GuestReady` condition is `True` (ADR-0043): the
    /// *installed* system has booted and said so. The only correct choice
    /// for a Deferred-mode image, where `InfrastructureReady` fires when the
    /// install starts, not when it ends.
    ///
    /// Not satisfiable yet: ADR-0043 is what publishes this condition. A
    /// pool set to it today reports `Warm=False` with reason
    /// `ReadinessSignalAbsent` rather than filling.
    GuestReady,
    /// The member's `InfrastructureReady` condition is `True`. Only safe for
    /// `installMode: Immediate` images.
    InfrastructureReady,
}

/// Inline static addressing for pool members. IPv4 only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PoolAddressing {
    /// Name of the `VMClass` network interface to stamp, matching
    /// `NetworkInterfaceOverride.name`.
    pub interface: String,
    /// First address of the inclusive range.
    pub range_start: String,
    /// Last address of the inclusive range. Size it at `maxReplicas` plus a
    /// few spares: an address stays held until a deleted member's backend VM
    /// is actually gone.
    pub range_end: String,
    pub prefix: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nameservers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VirtualMachinePoolStatus {
    /// Members of any phase, excluding ones already being deleted.
    #[serde(default)]
    pub replicas: u32,
    /// Ready and unclaimed: what a claim can bind right now.
    #[serde(default)]
    pub available: u32,
    #[serde(default)]
    pub provisioning: u32,
    #[serde(default)]
    pub claimed: u32,
    /// Image revision new members are currently being built from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

#[cfg(test)]
#[path = "virtualmachinepool_tests.rs"]
mod virtualmachinepool_tests;
