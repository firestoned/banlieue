// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! First-boot sealing readback, shared by every provider (ADR-0088
//! Decision 5, as amended 2026-10-22).
//!
//! A clone of an image with `firstBootSealing` seals the listed partitions to
//! its own vTPM in its initramfs, then a `boot` stage reports which listed
//! partitions are LUKS containers: `guestinfo.banlieue.sealed` on vSphere,
//! `/run/banlieue/sealed` on libvirt. The provider reads its channel; this
//! module decides what the reading means for `GuestReady`. A clone whose
//! sealing failed halts fail closed and never reports, so the decision also
//! carries a deadline.
//!
//! The report is guest-supplied input: it is parsed strictly and only ever
//! used to withhold `GuestReady`, never to grant anything beyond it.

use banlieue_api::common::{FirstBootSealing, condition_types};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use k8s_openapi::jiff::Timestamp;

use crate::status::condition_status;

/// `GuestReady` reason: the guest is up but has not reported every listed
/// partition sealed yet.
pub const REASON_FIRST_BOOT_SEALING_UNVERIFIED: &str = "FirstBootSealingUnverified";

/// `GuestReady` reason: the deadline passed without a complete report. A
/// halted first boot looks like this; the serial console shows the cause.
pub const REASON_FIRST_BOOT_SEALING_TIMEOUT: &str = "FirstBootSealingTimeout";

/// What a machine's sealing report means for `GuestReady`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SealingGate {
    /// Every listed partition has been reported sealed.
    Satisfied,
    /// Some are missing and the deadline has not passed (or not started).
    Pending {
        /// Listed partitions not reported sealed, in declaration order.
        missing: Vec<String>,
    },
    /// Some are missing and the deadline has passed.
    TimedOut {
        /// Listed partitions not reported sealed, in declaration order.
        missing: Vec<String>,
    },
}

/// A plain Kairos partition label (`[A-Z0-9_]+`).
fn is_plain_label(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Parse a raw report, `COS_PERSISTENT,COS_DATA`: comma separated, trimmed,
/// plain labels only, each once, in the order reported.
#[must_use]
pub fn parse_sealed_report(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for label in raw.split(',').map(str::trim).filter(|l| is_plain_label(l)) {
        if !out.iter().any(|o| o == label) {
            out.push(label.to_string());
        }
    }
    out
}

/// Fold an observation into the recorded `sealedPartitions`, stickily: the
/// report lives in volatile state, and a machine that was powered off has
/// not become unsealed. Previous entries keep their order; new ones follow.
#[must_use]
pub fn sticky_sealed_partitions(previous: &[String], observed: &[String]) -> Vec<String> {
    let mut out = previous.to_vec();
    for p in observed {
        if !out.contains(p) {
            out.push(p.clone());
        }
    }
    out
}

/// The listed partitions not in `sealed`, in declaration order.
#[must_use]
pub fn missing_partitions(required: &FirstBootSealing, sealed: &[String]) -> Vec<String> {
    required
        .partitions
        .iter()
        .filter(|p| !sealed.contains(p))
        .cloned()
        .collect()
}

/// When the machine's `Ready` condition last became `True`: the end of
/// provisioning, which on every backend is the power-on. The deadline counts
/// from here (ADR-0088 amendment D).
fn ready_since(conditions: &[Condition]) -> Option<Timestamp> {
    conditions
        .iter()
        .find(|c| c.type_ == condition_types::READY && c.status == condition_status::TRUE)
        .map(|c| c.last_transition_time.0)
}

/// Decide what `sealed` means against `required` at `now`.
///
/// # Arguments
/// * `required` - the machine's `spec.firstBootSealing`
/// * `sealed` - its sticky `status.sealedPartitions`
/// * `conditions` - its status conditions, for the `Ready=True` transition
/// * `now` - the current time
#[must_use]
pub fn sealing_gate(
    required: &FirstBootSealing,
    sealed: &[String],
    conditions: &[Condition],
    now: Timestamp,
) -> SealingGate {
    let missing = missing_partitions(required, sealed);
    if missing.is_empty() {
        return SealingGate::Satisfied;
    }
    let Some(since) = ready_since(conditions) else {
        return SealingGate::Pending { missing };
    };
    // Whole seconds, like leader election: the budget is in seconds.
    let deadline = since.as_second() + i64::from(required.timeout_seconds);
    if now.as_second() > deadline {
        return SealingGate::TimedOut { missing };
    }
    SealingGate::Pending { missing }
}

#[cfg(test)]
#[path = "sealing_tests.rs"]
mod sealing_tests;
