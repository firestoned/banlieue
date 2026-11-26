// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Re-placing a machine its backend refused in its failure domain
//! (ADR-0173).
//!
//! A cloud can refuse a VM in one zone that it would take in another. The
//! provider reports it on the infra CR, `VMCreated=False` with reason
//! `ZoneUnavailable` (`banlieue_api::common::allocation`), and never moves the
//! machine. The controller, the only placer, reads it here:
//!
//! 1. [`allocation_failure`]: does the infra CR of the VM's **recorded**
//!    placement report one, for that placement, on a machine that never
//!    provisioned?
//! 2. [`record_failure`]: fold it into `status.placementFailures`, once per
//!    failure (by its `lastTransitionTime`).
//! 3. [`active_exclusions`]: which `(provider, failure domain)` pairs the
//!    scheduler avoids now, each for [`exclusion_window`] of its count;
//!    [`scheduling_exclusions`] for an ordinary pass and
//!    [`replacement_exclusions`] for re-placing a refused machine.
//! 4. [`replaced_status`]: the status that records a new placement, written
//!    before the reconciler deletes the infra CR.
//!
//! Everything here is pure, so the whole decision is unit-tested; the
//! reconciler does the reads and writes.

use std::time::Duration;

use banlieue_api::banlieue::{
    MAX_PLACEMENT_FAILURES, PlacementFailure, ScheduledPlacement, VirtualMachineStatus,
};
use banlieue_api::common::{allocation, condition_types};
use banlieue_provider_sdk::status::{condition_status, find_condition, set_condition};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::jiff::{SignedDuration, Timestamp};

use super::scheduler::Decision;
use super::status_mirror::InfraMachineRead;

/// How long a failure domain is avoided after its first refusal (ADR-0161
/// Decision 2 starts the back-off at 5 minutes).
pub const ZONE_EXCLUSION_BASE: Duration = Duration::from_secs(300);
/// The longest a failure domain is avoided (ADR-0173 Decision 4).
pub const ZONE_EXCLUSION_MAX: Duration = Duration::from_secs(3600);
/// Each further refusal multiplies the exclusion by this.
pub const ZONE_EXCLUSION_FACTOR: u32 = 2;

/// `Ready=False` reason while a refused machine is re-placed.
pub const REASON_REPLACING: &str = "ReplacingAfterAllocationFailure";

/// One refusal an infra CR reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllocationFailure {
    /// The failure domain that refused the machine.
    pub failure_domain: String,
    /// The infra CR's `VMCreated` reason.
    pub reason: String,
    /// The provider's message, for the VM's condition.
    pub message: String,
    /// When: the condition's `lastTransitionTime`, which the provider moves
    /// on every failure.
    pub at: Time,
}

/// The refusal `infra` reports for the VM's `recorded` placement, if any.
///
/// `None` unless all of these hold: the infra CR has not provisioned (a VM
/// that ran is never re-placed this way); its `VMCreated` condition is
/// `False` with reason `ZoneUnavailable`; it names the recorded Provider
/// (when it names one) and reports the recorded failure domain. The last
/// two keep a terminating infra CR of an earlier placement from counting
/// against the new one.
#[must_use]
pub fn allocation_failure(
    infra: &dyn InfraMachineRead,
    recorded: &ScheduledPlacement,
) -> Option<AllocationFailure> {
    if infra.initialization().provisioned == Some(true) {
        return None;
    }
    let created = find_condition(infra.conditions(), allocation::VM_CREATED)?;
    if created.status != condition_status::FALSE || created.reason != allocation::ZONE_UNAVAILABLE {
        return None;
    }
    if infra
        .provider_name()
        .is_some_and(|p| p != recorded.provider_name)
    {
        return None;
    }
    let failure_domain = infra.failure_domain()?;
    if failure_domain != recorded.failure_domain {
        return None;
    }
    Some(AllocationFailure {
        failure_domain: failure_domain.to_string(),
        reason: created.reason.clone(),
        message: created.message.clone(),
        at: created.last_transition_time.clone(),
    })
}

/// How long a failure domain is avoided after its `failures`-th refusal:
/// [`ZONE_EXCLUSION_BASE`] doubled for each after the first, at most
/// [`ZONE_EXCLUSION_MAX`].
#[must_use]
pub fn exclusion_window(failures: u32) -> Duration {
    ZONE_EXCLUSION_FACTOR
        .checked_pow(failures.saturating_sub(1))
        .map_or(ZONE_EXCLUSION_MAX, |factor| {
            ZONE_EXCLUSION_BASE
                .saturating_mul(factor)
                .min(ZONE_EXCLUSION_MAX)
        })
}

/// Folds `failure` on `provider` into `failures`. Returns `true` when it is
/// new: no entry for the pair yet, or one whose `lastFailureTime` is older.
/// The pair's entry moves to the end (most recent last); the list keeps at
/// most [`MAX_PLACEMENT_FAILURES`], dropping the oldest.
pub fn record_failure(
    failures: &mut Vec<PlacementFailure>,
    provider: &str,
    failure: &AllocationFailure,
) -> bool {
    let position = failures
        .iter()
        .position(|f| f.provider_name == provider && f.failure_domain == failure.failure_domain);
    let entry = match position {
        Some(i) => {
            let existing = failures.remove(i);
            if existing.last_failure_time.0 >= failure.at.0 {
                failures.insert(i, existing);
                return false;
            }
            PlacementFailure {
                failures: existing.failures.saturating_add(1),
                reason: failure.reason.clone(),
                last_failure_time: failure.at.clone(),
                ..existing
            }
        }
        None => PlacementFailure {
            provider_name: provider.to_string(),
            failure_domain: failure.failure_domain.clone(),
            reason: failure.reason.clone(),
            failures: 1,
            last_failure_time: failure.at.clone(),
        },
    };
    failures.push(entry);
    let excess = failures.len().saturating_sub(MAX_PLACEMENT_FAILURES);
    failures.drain(..excess);
    true
}

/// The `(provider, failure domain)` pairs avoided at `now`: every entry
/// whose [`exclusion_window`] has not passed since its `lastFailureTime`.
#[must_use]
pub fn active_exclusions(failures: &[PlacementFailure], now: Timestamp) -> Vec<(String, String)> {
    failures
        .iter()
        .filter(|f| {
            SignedDuration::try_from(exclusion_window(f.failures))
                .ok()
                .and_then(|window| f.last_failure_time.0.checked_add(window).ok())
                .is_some_and(|until| now < until)
        })
        .map(|f| (f.provider_name.clone(), f.failure_domain.clone()))
        .collect()
}

/// The exclusions for an ordinary scheduling pass: [`active_exclusions`]
/// except the VM's own recorded placement. Whether that placement is still
/// usable is decided only from its infra CR's report ([`allocation_failure`]
/// and [`replacement_exclusions`]): excluding it here would read as drift,
/// which `migrationPolicy` answers, for a machine that may be retrying in
/// place because nothing else fits.
#[must_use]
pub fn scheduling_exclusions(
    failures: &[PlacementFailure],
    recorded: Option<&ScheduledPlacement>,
    now: Timestamp,
) -> Vec<(String, String)> {
    active_exclusions(failures, now)
        .into_iter()
        .filter(|(provider, fd)| {
            !recorded.is_some_and(|r| r.provider_name == *provider && r.failure_domain == *fd)
        })
        .collect()
}

/// The exclusions for re-placing a VM whose recorded placement on
/// `provider` just reported `failure`: [`active_exclusions`] and, whatever
/// its window, the failure domain that is refusing the machine now.
#[must_use]
pub fn replacement_exclusions(
    failures: &[PlacementFailure],
    provider: &str,
    failure: &AllocationFailure,
    now: Timestamp,
) -> Vec<(String, String)> {
    let mut excluded = active_exclusions(failures, now);
    let refusing = (provider.to_string(), failure.failure_domain.clone());
    if !excluded.contains(&refusing) {
        excluded.push(refusing);
    }
    excluded
}

/// Whether `decision` moves the VM off `recorded`.
#[must_use]
pub fn moves(decision: &Decision, recorded: &ScheduledPlacement) -> bool {
    decision.provider_name != recorded.provider_name
        || decision.failure_domain_name != recorded.failure_domain
}

/// `current` with `decision` as the new placement, `failures` as the
/// history, and `PlacementValid=False` / `Ready=False` saying why: the status
/// written before the refused infra CR is deleted. `migrationPolicy` plays
/// no part, since no VM ever ran (ADR-0173 Decision 2).
#[must_use]
pub fn replaced_status(
    current: &VirtualMachineStatus,
    failures: Vec<PlacementFailure>,
    decision: &Decision,
    failure: &AllocationFailure,
    generation: i64,
    now: Time,
) -> VirtualMachineStatus {
    let mut next = current.clone();
    let message = format!(
        "failure domain '{}' refused the machine ({}); re-placing it in '{}' on provider '{}'",
        failure.failure_domain,
        failure.message,
        decision.failure_domain_name,
        decision.provider_name
    );
    next.placement_failures = failures;
    next.scheduled = Some(decision.to_scheduled_placement(now));
    set_condition(
        &mut next.conditions,
        condition_types::PLACEMENT_VALID,
        condition_status::FALSE,
        &failure.reason,
        message.clone(),
        generation,
    );
    set_condition(
        &mut next.conditions,
        condition_types::READY,
        condition_status::FALSE,
        REASON_REPLACING,
        message,
        generation,
    );
    next.observed_generation = Some(generation);
    next
}

#[cfg(test)]
#[path = "placement_failure_tests.rs"]
mod placement_failure_tests;
