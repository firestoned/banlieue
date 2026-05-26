// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Small helpers around [`kube::runtime::controller::Action`].
//!
//! Centralizing these constants keeps requeue semantics consistent across
//! controllers and makes it cheap to tune them globally.

use std::time::Duration;

use kube::runtime::controller::Action;

/// Default requeue interval when reconciliation succeeded and the resource
/// is in a stable state. Matches Cluster API's default tick.
pub const REQUEUE_DEFAULT_SECS: u64 = 30;

/// Requeue interval after a transient failure — short enough that recovery
/// is fast, long enough to avoid hammering a degraded apiserver / backend.
pub const REQUEUE_ON_ERROR_SECS: u64 = 5;

/// Requeue interval for terminal-but-poll-y states (e.g. waiting for a long
/// backend operation like an OVF import).
pub const REQUEUE_LONG_SECS: u64 = 300;

/// Returns an [`Action`] requeuing after the default interval.
pub fn requeue_default() -> Action {
    Action::requeue(Duration::from_secs(REQUEUE_DEFAULT_SECS))
}

/// Returns an [`Action`] requeuing after the short error interval.
pub fn requeue_on_error() -> Action {
    Action::requeue(Duration::from_secs(REQUEUE_ON_ERROR_SECS))
}

/// Returns an [`Action`] requeuing after the long interval (slow polls).
pub fn requeue_long() -> Action {
    Action::requeue(Duration::from_secs(REQUEUE_LONG_SECS))
}

/// Returns an [`Action`] with no requeue — for terminal states.
pub fn no_requeue() -> Action {
    Action::await_change()
}

#[cfg(test)]
#[path = "reconciler_tests.rs"]
mod reconciler_tests;
