// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Provider liveness from its leader-election Lease (ADR-0102).
//!
//! A provider reports its own health in `status.conditions`, and a provider
//! that is down reports nothing, so its last `Ready=True` stands forever.
//! Liveness is therefore judged here, outside the provider, from the Lease
//! its leader election renews every few seconds, and written into
//! `status.workload`, the operator's own field (ADR-0012).
//!
//! Everything is pure: [`evaluate`] reads a Lease snapshot at a given time,
//! [`transition`] decides which Event a change deserves, and
//! [`requeue_after`] picks when to look again. The reconciler does the one
//! `get` and the status patch.

use std::time::Duration;

use banlieue_api::banlieue::Heartbeat;
use banlieue_provider_sdk::reconciler::REQUEUE_DEFAULT_SECS;
use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, Time};
use k8s_openapi::jiff::Timestamp;

/// Default for `--provider-stale-after-secs`: four lease durations, so a
/// slow renewal or a leader handover is never read as death.
pub const DEFAULT_STALE_AFTER_SECS: u64 = 60;

/// The shortest requeue [`requeue_after`] returns, so a deadline that has
/// just passed never becomes a zero-length (spinning) requeue.
const MIN_RECHECK_SECS: u64 = 1;

/// One evaluation of a provider's Lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// The provider's liveness.
    pub heartbeat: Heartbeat,
    /// The Lease's last renewal (or acquisition), when it records one.
    pub last_heartbeat_time: Option<Time>,
    /// How long until this reading could change on its own: the moment an
    /// `Alive` heartbeat would go stale, else `stale_after`.
    pub recheck_in: Duration,
}

/// Judge `lease` at `now`.
///
/// `Alive` if it was renewed (or, failing a `renewTime`, acquired) at most
/// `stale_after` ago, including a renewal stamped ahead of `now` by clock
/// skew; `Stale` if longer ago; `Absent` if there is no Lease, or one that
/// records neither time and so proves nothing.
#[must_use]
pub fn evaluate(lease: Option<&Lease>, now: Timestamp, stale_after: Duration) -> Reading {
    let held = lease
        .and_then(|l| l.spec.as_ref())
        .and_then(|spec| spec.renew_time.as_ref().or(spec.acquire_time.as_ref()));
    let Some(MicroTime(renewed)) = held else {
        return Reading {
            heartbeat: Heartbeat::Absent,
            last_heartbeat_time: None,
            recheck_in: stale_after,
        };
    };

    let last_heartbeat_time = Some(Time(*renewed));
    let age_secs = now.as_second().saturating_sub(renewed.as_second());
    let stale_after_secs = i64::try_from(stale_after.as_secs()).unwrap_or(i64::MAX);
    if age_secs > stale_after_secs {
        return Reading {
            heartbeat: Heartbeat::Stale,
            last_heartbeat_time,
            recheck_in: stale_after,
        };
    }
    // A negative age (skew) waits a whole `stale_after`, never longer.
    let remaining = u64::try_from(stale_after_secs - age_secs.max(0)).unwrap_or(0);
    Reading {
        heartbeat: Heartbeat::Alive,
        last_heartbeat_time,
        recheck_in: Duration::from_secs(remaining),
    }
}

/// A change in liveness worth an Event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// The provider stopped renewing its Lease.
    WentStale,
    /// A stale provider is renewing again.
    Restored,
}

/// The Event, if any, for going from `previous` to `current`. Becoming
/// `Stale` from anything (including a first evaluation that finds a
/// provider already down) and recovering from `Stale` are reported; the
/// rest is noise.
#[must_use]
pub fn transition(previous: Option<Heartbeat>, current: Heartbeat) -> Option<Transition> {
    match (previous, current) {
        (Some(Heartbeat::Stale), Heartbeat::Alive) => Some(Transition::Restored),
        (Some(Heartbeat::Stale), _) => None,
        (_, Heartbeat::Stale) => Some(Transition::WentStale),
        _ => None,
    }
}

/// When to reconcile the Provider again: the sooner of `recheck_in` and the
/// operator's normal interval, and never less than a second.
#[must_use]
pub fn requeue_after(recheck_in: Duration) -> Duration {
    recheck_in
        .min(Duration::from_secs(REQUEUE_DEFAULT_SECS))
        .max(Duration::from_secs(MIN_RECHECK_SECS))
}

#[cfg(test)]
#[path = "heartbeat_tests.rs"]
mod heartbeat_tests;
