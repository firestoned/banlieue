// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Migration sub-loop — recreate-only path for Phase 1A iteration 3.
//!
//! Placement in banlieue is non-sticky: the scheduler runs on every reconcile.
//! When the freshly-computed [`Decision`] differs from the previously-recorded
//! [`ScheduledPlacement`], this module decides whether and how to act, per the
//! VM's [`MigrationPolicy`]:
//!
//! - [`MigrationPolicy::Never`]   → take no action (sticky behaviour).
//!   `PlacementValid` stays `True` because the spec says drift is fine.
//! - [`MigrationPolicy::Manual`]  → surface `PlacementValid=False`. Only act
//!   when the user adds the annotation `banlieue.io/migrate=true`.
//! - [`MigrationPolicy::Automatic`] → surface `PlacementValid=False` and
//!   recreate (delete the old infra CR; the next reconcile creates a fresh
//!   one with the new placement). Live migration is Phase 2 work.
//!
//! Like the scheduler, this is a **pure function**. The reconciler is
//! responsible for actually issuing the delete.
//!
//! ## Why "recreate-only"
//!
//! Live migration semantics differ across providers (vSphere has vMotion,
//! Proxmox has live migration over shared storage, libvirt has no live
//! migration in v1). Faking a uniform live-migration contract would leak
//! per-backend behaviour into the user-visible status — exactly what the
//! abstraction principle forbids. Recreate is the lowest common
//! denominator that works on every backend, and the `MigrationPolicy::Never`
//! escape hatch lets users opt out of even that.

use banlieue_api::banlieue::{MigrationPolicy, ScheduledPlacement, VirtualMachine};
use kube::ResourceExt;

use super::scheduler::Decision;

/// Annotation that triggers a manual migration when
/// `migrationPolicy=Manual`.
pub const ANNOTATION_MIGRATE: &str = "banlieue.io/migrate";

/// Value of [`ANNOTATION_MIGRATE`] that means "go ahead and migrate now".
pub const ANNOTATION_MIGRATE_TRUE: &str = "true";

/// What the reconciler should do about a (possibly drifted) placement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrationAction {
    /// No drift — current placement matches the scheduler's decision.
    /// `PlacementValid=True`; proceed with normal apply + status mirror.
    InPlace,

    /// Drift detected but `migrationPolicy=Never` says do nothing.
    /// `PlacementValid=True` because the spec accepts this drift as
    /// permanent. The reconciler should keep using the previously-stored
    /// `scheduled` placement.
    StickToOld,

    /// Drift detected. Surface `PlacementValid=False`. The reconciler must
    /// **not** apply the new placement yet — either the user hasn't asked
    /// (`Manual` without annotation) or we're flagging the situation
    /// without acting (`Automatic` first observation, see iteration-4
    /// note below).
    SurfaceOnly { reason: PlacementDriftReason },

    /// Drift detected and policy/annotation allows action. The reconciler
    /// must delete the existing infrastructure CR; the *next* reconcile
    /// pass will create a new one with the fresh placement. The deleted
    /// CR's name is carried so the reconciler doesn't have to recompute
    /// it from `status.infrastructure_ref`.
    Recreate { reason: PlacementDriftReason },
}

/// Stable reason strings for the `PlacementValid` condition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlacementDriftReason {
    /// Provider changed: the previously-scheduled `Provider` is no longer
    /// the chosen one.
    ProviderChanged {
        from: String,
        to: String,
    },
    /// Same provider, different failure domain.
    FailureDomainChanged {
        provider: String,
        from: String,
        to: String,
    },
    StorageChanged,
    NetworkChanged,
}

impl PlacementDriftReason {
    /// Stable condition `reason` for `PlacementValid=False`.
    pub fn reason(&self) -> &'static str {
        match self {
            PlacementDriftReason::ProviderChanged { .. } => "ProviderChanged",
            PlacementDriftReason::FailureDomainChanged { .. } => "FailureDomainChanged",
            PlacementDriftReason::StorageChanged => "StorageMappingChanged",
            PlacementDriftReason::NetworkChanged => "NetworkMappingChanged",
        }
    }

    /// Human-readable condition `message`.
    pub fn message(&self) -> String {
        match self {
            PlacementDriftReason::ProviderChanged { from, to } => format!(
                "scheduler now prefers provider '{to}' over the previously scheduled '{from}'"
            ),
            PlacementDriftReason::FailureDomainChanged { provider, from, to } => format!(
                "scheduler now prefers failure domain '{to}' on provider '{provider}' (was '{from}')"
            ),
            PlacementDriftReason::StorageChanged => {
                "scheduler resolved storage classes to different backends than the recorded placement".to_string()
            }
            PlacementDriftReason::NetworkChanged => {
                "scheduler resolved network classes to different backends than the recorded placement".to_string()
            }
        }
    }
}

/// Decide what to do given the freshly-computed [`Decision`], the previously
/// recorded [`ScheduledPlacement`], and the VM's [`MigrationPolicy`].
///
/// Returns [`MigrationAction::InPlace`] when there is no drift, or one of
/// the drift variants otherwise. The function is total: every input
/// combination produces a well-defined action.
pub fn evaluate(vm: &VirtualMachine, decision: &Decision) -> MigrationAction {
    let Some(current) = vm.status.as_ref().and_then(|s| s.scheduled.as_ref()) else {
        // No previous placement → this is the first scheduling pass, not a
        // migration. The caller will write `status.scheduled` and apply the
        // infra CR via the normal path.
        return MigrationAction::InPlace;
    };

    let Some(drift) = detect_drift(current, decision) else {
        return MigrationAction::InPlace;
    };

    match vm.spec.migration_policy {
        MigrationPolicy::Never => MigrationAction::StickToOld,
        MigrationPolicy::Manual => {
            if migrate_annotation_set(vm) {
                MigrationAction::Recreate { reason: drift }
            } else {
                MigrationAction::SurfaceOnly { reason: drift }
            }
        }
        MigrationPolicy::Automatic => MigrationAction::Recreate { reason: drift },
    }
}

/// First drift difference between `current` (previously-recorded) and the
/// freshly-computed `decision`. Returns `None` when they match.
fn detect_drift(current: &ScheduledPlacement, decision: &Decision) -> Option<PlacementDriftReason> {
    if current.provider_name != decision.provider_name {
        return Some(PlacementDriftReason::ProviderChanged {
            from: current.provider_name.clone(),
            to: decision.provider_name.clone(),
        });
    }
    if current.failure_domain != decision.failure_domain_name {
        return Some(PlacementDriftReason::FailureDomainChanged {
            provider: current.provider_name.clone(),
            from: current.failure_domain.clone(),
            to: decision.failure_domain_name.clone(),
        });
    }
    if current.resolved_storage != decision.resolved_storage {
        return Some(PlacementDriftReason::StorageChanged);
    }
    if current.resolved_networks != decision.resolved_networks {
        return Some(PlacementDriftReason::NetworkChanged);
    }
    None
}

/// True when the user explicitly approved a manual migration.
fn migrate_annotation_set(vm: &VirtualMachine) -> bool {
    vm.annotations()
        .get(ANNOTATION_MIGRATE)
        .map(|v| v == ANNOTATION_MIGRATE_TRUE)
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "migration_tests.rs"]
mod migration_tests;
