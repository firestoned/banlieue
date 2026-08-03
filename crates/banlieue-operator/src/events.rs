// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Kubernetes Events published against `Provider` objects.
//!
//! Conditions say what state a resource is *in*; events say what the controller
//! *did*, and when. Without them `kubectl describe provider` shows nothing
//! about the lifecycle: an operator watching a Provider that never comes up has
//! to go and read controller logs to learn whether the class was missing, the
//! reconcile was paused, or a workload was pruned out from under them.
//!
//! These builders are pure so their wording and severity are unit-testable —
//! publishing needs a cluster, choosing the right message does not.
//!
//! # Severity
//!
//! `Normal` for things the author asked for (a workload applied, a pause
//! honoured). `Warning` for things they probably did not: a missing
//! ProviderClass, or a running workload pruned because the class reference
//! changed. Pruning in particular deletes a pod that still holds backend
//! credentials, so it should stand out rather than blend in.

use kube::runtime::events::{Event, EventType};

use crate::reconciler::provider::SkipReason;

/// Stable `reason` identifiers.
///
/// These are matched by `kubectl get events --field-selector`, by dashboards
/// and by alert rules, so they are a closed set of CamelCase identifiers and
/// must not drift.
pub mod reasons {
    /// A workload was created or updated.
    pub const WORKLOAD_APPLIED: &str = "WorkloadApplied";
    /// A superseded workload was removed after the Provider's class changed.
    pub const WORKLOAD_PRUNED: &str = "WorkloadPruned";
    /// A workload was removed because its Provider is being deleted.
    pub const WORKLOAD_DELETED: &str = "WorkloadDeleted";
    /// Reconciliation is suspended by a `paused` flag.
    pub const RECONCILE_SKIPPED: &str = "ReconcileSkipped";
    /// The referenced `ProviderClass` does not exist.
    pub const CLASS_NOT_FOUND: &str = "ProviderClassNotFound";
}

/// A workload was created or updated.
#[must_use]
pub fn workload_applied(workload: &str, namespace: &str) -> Event {
    Event {
        type_: EventType::Normal,
        reason: reasons::WORKLOAD_APPLIED.to_string(),
        note: Some(format!(
            "Applied provider workload {workload} in namespace {namespace}"
        )),
        action: "Apply".to_string(),
        secondary: None,
    }
}

/// A superseded workload was pruned.
///
/// `Warning`, not `Normal`: this deletes a running pod that still holds backend
/// credentials. It is correct — but an operator who did not expect it should
/// see it stand out in `kubectl describe`.
#[must_use]
pub fn workload_pruned(workload: &str) -> Event {
    Event {
        type_: EventType::Warning,
        reason: reasons::WORKLOAD_PRUNED.to_string(),
        note: Some(format!(
            "Pruned superseded provider workload {workload}; it is no longer part of this \
             Provider (its ProviderClass reference changed)"
        )),
        action: "Prune".to_string(),
        secondary: None,
    }
}

/// A workload was removed because its Provider is going away.
#[must_use]
pub fn workload_deleted(workload: &str) -> Event {
    Event {
        type_: EventType::Normal,
        reason: reasons::WORKLOAD_DELETED.to_string(),
        note: Some(format!(
            "Deleted provider workload {workload} as its Provider is being removed"
        )),
        action: "Delete".to_string(),
        secondary: None,
    }
}

/// Reconciliation is suspended.
///
/// The note names *which* object is paused: from the Provider's side a paused
/// class and a paused Provider are indistinguishable, and an operator who has
/// un-paused the wrong one would otherwise have no way to tell.
#[must_use]
pub fn reconcile_skipped(reason: SkipReason) -> Event {
    let note = match reason {
        SkipReason::ProviderPaused => {
            "Reconciliation suspended: this Provider has spec.paused set. Existing workloads are \
             left running."
        }
        SkipReason::ClassPaused => {
            "Reconciliation suspended: the referenced ProviderClass has spec.paused set, which \
             suspends every Provider of that class. Existing workloads are left running."
        }
    };
    Event {
        type_: EventType::Normal,
        reason: reasons::RECONCILE_SKIPPED.to_string(),
        note: Some(note.to_string()),
        action: "Skip".to_string(),
        secondary: None,
    }
}

/// The referenced `ProviderClass` does not exist.
///
/// `Warning`: the Provider can never come up until it is created, and this is
/// the single most likely misconfiguration — a Provider applied before its
/// class, or a typo in `providerClassRef`.
#[must_use]
pub fn class_not_found(class: &str) -> Event {
    Event {
        type_: EventType::Warning,
        reason: reasons::CLASS_NOT_FOUND.to_string(),
        note: Some(format!(
            "ProviderClass {class} not found; no workload can be created until it exists. \
             `banlieue bootstrap operator` seeds one per compiled-in backend."
        )),
        action: "Resolve".to_string(),
        secondary: None,
    }
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod events_tests;
