// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue.io/v1alpha1` VirtualMachineMove CRD (ADR-0127).
//!
//! A request to move one running `VirtualMachine` to another
//! `cloud-hypervisor` host in the same zone, keeping its memory, disk, MAC and
//! addresses. faubourg creates one per VM when it drains a host (faubourg
//! ADR-0017); a person can create the same request.
//!
//! The requester decides *when* and *from which host*; banlieue decides
//! *where* (unless the request names a target) and does the move. A requester
//! needs `create` on moves and read access to VMs, never write access to a VM:
//! that is why this is a CRD and not an annotation.
//!
//! Served before any controller acts on it (ADR-0127 Decision 13, API first):
//! until Stage 1 ships, every move stays `Pending`.

use crate::common::{LabelSelector, LocalObjectReference};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Condition types on a `VirtualMachineMove`.
pub mod move_condition_types {
    /// `True` once banlieue admitted the move (ADR-0127 Decision 3); `False`,
    /// with one of [`super::move_reasons`], when it rejected it.
    pub const ADMITTED: &str = "Admitted";
}

/// Reasons a move is rejected or fails. API: a requester reports them to an
/// operator verbatim (faubourg ADR-0017 Decision 4).
pub mod move_reasons {
    /// Another move for the same VM is not finished (Decision 2).
    pub const MOVE_IN_PROGRESS: &str = "MoveInProgress";
    /// The VM has a vTPM, which moves only from Stage 2 (Decision 8).
    pub const VTPM_NOT_MOVABLE: &str = "VTPMNotMovable";
    /// The source storage class does not reflink (Decision 7).
    pub const STORAGE_WITHOUT_REFLINK: &str = "StorageWithoutReflink";
    /// The VM's `migrationPolicy` is `Never`.
    pub const MIGRATION_POLICY_NEVER: &str = "MigrationPolicyNever";
    /// The source Provider has no `banlieue.io/zone` label, or the named target
    /// is in another zone.
    pub const NOT_IN_A_ZONE: &str = "NotInAZone";
    /// The two hosts differ in VMM version or CPU model (`raw.vmm`,
    /// `raw.cpuModel`).
    pub const HOSTS_DIFFER: &str = "HostsDiffer";
    /// No host in the zone passes every check for this VM.
    pub const NO_DESTINATION: &str = "NoDestination";
    /// The VM is not running, or is still installing.
    pub const NOT_RUNNING: &str = "NotRunning";
    /// The source or destination does not advertise the `move` feature.
    pub const MOVE_UNSUPPORTED: &str = "MoveUnsupported";
    /// The move was aborted before commit; the VM runs where it was
    /// (Decision 12).
    pub const ABORTED: &str = "Aborted";
}

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "banlieue.io",
    version = "v1alpha1",
    kind = "VirtualMachineMove",
    plural = "virtualmachinemoves",
    shortname = "vmmove",
    namespaced,
    status = "VirtualMachineMoveStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"VM","type":"string","jsonPath":".spec.vmRef.name"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Source","type":"string","jsonPath":".status.source"}"#,
    printcolumn = r#"{"name":"Destination","type":"string","jsonPath":".status.destination"}"#,
    printcolumn = r#"{"name":"Downtime(ms)","type":"integer","jsonPath":".status.downtimeMilliseconds","priority":1}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
/// VirtualMachineMove: move one running VM to another Cloud Hypervisor host in
/// its zone, keeping its memory, disk, MAC and addresses (ADR-0127).
///
/// # What a move guarantees
///
/// - **The guest runs on exactly one host, always.** Failure before commit
///   leaves it where it was; after commit the old copy is never booted again.
/// - **Admitted only where it can work**: same zone (`banlieue.io/zone`), both
///   hosts advertising `move`, identical VMM version and CPU model, and a
///   destination the VM's own placement allows. Otherwise `Rejected`, with a
///   named reason.
/// - **One move per VM at a time.** Deleting a move before `HandingOver`
///   cancels it; after that it runs to `Succeeded` or `Failed`.
///
/// # What a move is not
///
/// Not a restart elsewhere after a host failure: that is fencing and HA, and
/// banlieue does not do it. Until Stage 2 a VM with a vTPM is not movable.
pub struct VirtualMachineMoveSpec {
    /// The VM to move, in the move's namespace. Immutable.
    #[schemars(extend("x-kubernetes-validations" = [{
        "rule": "self == oldSelf",
        "message": "vmRef is immutable: delete the move and create another",
    }]))]
    pub vm_ref: LocalObjectReference,

    /// Where to move it. When absent, banlieue picks a host in the VM's zone:
    /// of those passing every check, the one with the most free memory left
    /// after the move, then the most free vCPUs, then by name (Decision 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-kubernetes-validations" = [{
        "rule": "has(self.providerName) != has(self.providerSelector)",
        "message": "target takes exactly one of providerName or providerSelector",
    }]))]
    pub target: Option<MoveTarget>,
}

/// A move's destination: one named Provider, or a selector among the zone's.
/// Exactly one of the two.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MoveTarget {
    /// The destination Provider's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    /// Restricts the destination to the zone's Providers matching it; banlieue
    /// still ranks among them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_selector: Option<LabelSelector>,
}

/// Where a move is. One way: nothing returns to an earlier phase.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum MovePhase {
    /// Not admitted yet. Where every move stays until a controller acts on it.
    #[default]
    Pending,
    /// Admitted; the destination machine is being prepared.
    Preparing,
    /// The disk is being copied while the guest runs.
    Copying,
    /// The guest is paused for cutover, or the handover is in progress. No
    /// longer cancellable.
    HandingOver,
    /// The guest runs on the destination.
    Succeeded,
    /// The move failed; the VM is where `status.source` says, or (after
    /// commit) waits on the destination.
    Failed,
    /// Not admitted, with a reason in the `Admitted` condition.
    Rejected,
}

impl MovePhase {
    /// Whether deleting the move still cancels it (Decision 2).
    #[must_use]
    pub fn is_cancellable(self) -> bool {
        matches!(self, Self::Pending | Self::Preparing | Self::Copying)
    }

    /// Whether the move is finished, one way or another.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Rejected)
    }
}

/// Observed state of a move.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VirtualMachineMoveStatus {
    #[serde(default)]
    pub phase: MovePhase,
    /// The Provider the VM was on when the move was admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The Provider chosen to receive it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// Bytes the source has to send (disk extents and VMM state), once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_total: Option<u64>,
    /// Bytes sent so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_sent: Option<u64>,
    /// How long the guest was paused, cutover to resume. Set on `Succeeded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downtime_milliseconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Time>,
    /// Standard conditions; `Admitted` carries the rejection reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["type"],
    ))]
    pub conditions: Vec<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

#[cfg(test)]
#[path = "virtualmachinemove_tests.rs"]
mod virtualmachinemove_tests;
