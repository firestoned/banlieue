// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `Retry-After` (ADR-0164 Decision 2).
//!
//! ARM sends it on 429 and on long-running operations, as a number of
//! seconds. On a new subscription the spike saw a one-second hint on the
//! fourth consecutive delete. An HTTP-date form is not used by ARM; it is
//! ignored and the default applies.

use std::time::Duration;

/// Used when ARM sends no usable hint.
pub const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(5);
/// A hint of zero still waits this long, so a retry loop never spins.
pub const MIN_RETRY_AFTER: Duration = Duration::from_secs(1);
/// Longer hints are capped: a reconcile requeues rather than sleeping.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

/// Parses a `Retry-After` value given in seconds, clamped to
/// [`MIN_RETRY_AFTER`]..=[`MAX_RETRY_AFTER`]. `None` for anything else.
#[must_use]
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let secs: u64 = value.trim().parse().ok()?;
    Some(Duration::from_secs(secs).clamp(MIN_RETRY_AFTER, MAX_RETRY_AFTER))
}

/// The delay to wait for a `Retry-After` header that may be absent or
/// unusable: the parsed hint, else [`DEFAULT_RETRY_AFTER`].
#[must_use]
pub fn retry_delay(header: Option<&str>) -> Duration {
    header
        .and_then(parse_retry_after)
        .unwrap_or(DEFAULT_RETRY_AFTER)
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod retry_tests;
