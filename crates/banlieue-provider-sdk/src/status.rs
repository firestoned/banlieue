// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Helpers for managing `metav1.Condition` lists on CR status.
//!
//! These follow the Kubernetes conventions documented at
//! <https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#typical-status-properties>:
//!
//! - Conditions are keyed by `type`; only one condition of each type may exist.
//! - `lastTransitionTime` updates only when `status` changes.
//! - `observedGeneration` always reflects the controller's view at the time
//!   the condition was set.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};

/// Standard condition status values used across the project. Mirrors
/// `metav1.ConditionStatus` so consumers don't have to import the upstream
/// `k8s_openapi` crate just for the constants.
pub mod condition_status {
    pub const TRUE: &str = "True";
    pub const FALSE: &str = "False";
    pub const UNKNOWN: &str = "Unknown";
}

/// Upsert a condition into `conditions`.
///
/// - If a condition with the same `type_` exists and its `status` differs, the
///   existing entry is replaced and `last_transition_time` is set to `now`.
/// - If a condition with the same `type_` exists and its `status` matches, the
///   entry's `reason`, `message`, and `observed_generation` are updated in
///   place but `last_transition_time` is preserved.
/// - If no condition with `type_` exists, a new entry is appended with
///   `last_transition_time = now`.
///
/// The conditions vector is kept sorted by `type` ascending so that diffs in
/// `kubectl get -o yaml` stay stable.
pub fn set_condition(
    conditions: &mut Vec<Condition>,
    type_: &str,
    status: &str,
    reason: &str,
    message: impl Into<String>,
    observed_generation: i64,
) {
    let message = message.into();
    let now = Time(k8s_openapi::jiff::Timestamp::now());

    if let Some(existing) = conditions.iter_mut().find(|c| c.type_ == type_) {
        if existing.status == status {
            existing.reason = reason.to_string();
            existing.message = message;
            existing.observed_generation = Some(observed_generation);
        } else {
            existing.status = status.to_string();
            existing.reason = reason.to_string();
            existing.message = message;
            existing.observed_generation = Some(observed_generation);
            existing.last_transition_time = now;
        }
    } else {
        conditions.push(Condition {
            type_: type_.to_string(),
            status: status.to_string(),
            reason: reason.to_string(),
            message,
            observed_generation: Some(observed_generation),
            last_transition_time: now,
        });
    }

    conditions.sort_by(|a, b| a.type_.cmp(&b.type_));
}

/// Returns `true` iff a condition with `type_` exists and its status is
/// `"True"`.
pub fn is_condition_true(conditions: &[Condition], type_: &str) -> bool {
    conditions
        .iter()
        .any(|c| c.type_ == type_ && c.status == condition_status::TRUE)
}

/// Returns the condition with `type_`, if any.
pub fn find_condition<'a>(conditions: &'a [Condition], type_: &str) -> Option<&'a Condition> {
    conditions.iter().find(|c| c.type_ == type_)
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod status_tests;
