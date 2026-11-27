// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Back-off for throttling (ADR-0174 Decision 2).
//!
//! Throttling (`RequestLimitExceeded`, `Throttling`, HTTP 503, anything
//! [`Error::is_throttled`] says yes to) is retried with capped exponential
//! back-off and full jitter: the wait before retry `attempt` is drawn
//! uniformly from `[0, min(cap, base * 2^attempt))`. Full jitter spreads a
//! fleet of reconcilers that were throttled together, so they do not all
//! come back at once. The budget is the same for every verb, and nothing
//! else is retried here: capacity, quota, policy and invalid requests are
//! decisions for the provider, not for a loop.
//!
//! The random draw is injected ([`RetryPolicy::delay`] takes it as a value,
//! [`RetryPolicy::delay_with`] as a [`Draw`]), so every decision is
//! deterministic under test; [`system_draw`] is the production draw, from
//! aws-lc-rs's system random generator.
//!
//! [`Throttling`] tells the provider when throttling is sustained (many
//! throttled answers within a window), so it can say so on the `Provider`'s
//! conditions instead of only in logs.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use aws_lc_rs::rand::{SecureRandom, SystemRandom};

use crate::error::Error;

/// The back-off ceiling of the first retry.
pub const BASE_DELAY: Duration = Duration::from_secs(1);
/// No back-off ceiling exceeds this.
pub const MAX_DELAY: Duration = Duration::from_secs(20);
/// Attempts in all, the first included: four retries at most.
pub const MAX_ATTEMPTS: u32 = 5;

/// Throttled answers within [`SUSTAINED_THROTTLE_WINDOW`] that make
/// throttling "sustained".
pub const SUSTAINED_THROTTLE_COUNT: usize = 10;
/// The window [`SUSTAINED_THROTTLE_COUNT`] is counted over.
pub const SUSTAINED_THROTTLE_WINDOW: Duration = Duration::from_secs(60);

/// The draw used if the system random generator fails: the middle of the
/// range, so a retry still waits.
const FALLBACK_DRAW: f64 = 0.5;
/// Random bits kept from a 64-bit draw: an `f64`'s mantissa.
const DRAW_MANTISSA_BITS: u32 = 53;
/// Bits dropped from a 64-bit draw to keep [`DRAW_MANTISSA_BITS`].
const DRAW_DISCARDED_BITS: u32 = u64::BITS - DRAW_MANTISSA_BITS;
/// The exponential factor.
const BACKOFF_GROWTH: u32 = 2;
/// The largest draw below 1.
const MAX_DRAW: f64 = 1.0 - f64::EPSILON;
/// The smallest step of a [`Duration`].
const ONE_NANOSECOND: Duration = Duration::from_nanos(1);

/// A source of uniform draws in `[0, 1)`.
pub type Draw = fn() -> f64;

/// Capped exponential back-off with full jitter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// The ceiling of the first retry's wait.
    pub base: Duration,
    /// No ceiling exceeds this.
    pub cap: Duration,
    /// Attempts in all, the first included.
    pub max_attempts: u32,
}

impl Default for RetryPolicy {
    /// ADR-0174 Decision 2: [`BASE_DELAY`], [`MAX_DELAY`], [`MAX_ATTEMPTS`].
    fn default() -> Self {
        Self {
            base: BASE_DELAY,
            cap: MAX_DELAY,
            max_attempts: MAX_ATTEMPTS,
        }
    }
}

impl RetryPolicy {
    /// The upper bound of the wait after the failed attempt `attempt`
    /// (0-based): `min(cap, base * 2^attempt)`, saturating at `cap` for any
    /// `attempt`, however large.
    #[must_use]
    pub fn ceiling(&self, attempt: u32) -> Duration {
        BACKOFF_GROWTH
            .checked_pow(attempt)
            .and_then(|factor| self.base.checked_mul(factor))
            .map_or(self.cap, |ceiling| ceiling.min(self.cap))
    }

    /// Whether another attempt is allowed after the failed attempt
    /// `attempt` (0-based).
    fn has_attempt_after(&self, attempt: u32) -> bool {
        attempt.saturating_add(1) < self.max_attempts
    }

    /// The wait before retrying after the failed attempt `attempt`
    /// (0-based), for a uniform `draw` in `[0, 1)`: `draw * ceiling`,
    /// strictly below [`RetryPolicy::ceiling`]. A draw outside `[0, 1)` is
    /// clamped into it, NaN to 0.
    ///
    /// Returns `None` when `attempt` was the last of
    /// [`RetryPolicy::max_attempts`].
    #[must_use]
    pub fn delay(&self, attempt: u32, draw: f64) -> Option<Duration> {
        if !self.has_attempt_after(attempt) {
            return None;
        }
        let ceiling = self.ceiling(attempt);
        let draw = clamp_draw(draw);
        // The product of two non-negative finite values; `as` saturates.
        let nanos = (ceiling.as_nanos() as f64 * draw).floor() as u64;
        let latest = ceiling.saturating_sub(ONE_NANOSECOND);
        Some(Duration::from_nanos(nanos).min(latest))
    }

    /// [`RetryPolicy::delay`] with the draw taken from `draw`, which is
    /// called only when a retry is still allowed.
    #[must_use]
    pub fn delay_with(&self, attempt: u32, draw: Draw) -> Option<Duration> {
        if !self.has_attempt_after(attempt) {
            return None;
        }
        self.delay(attempt, draw())
    }

    /// Whether `error`, from the failed attempt `attempt` (0-based), should
    /// be retried: only throttling ([`Error::is_throttled`]), and only
    /// while attempts remain.
    #[must_use]
    pub fn should_retry(&self, error: &Error, attempt: u32) -> bool {
        error.is_throttled() && self.has_attempt_after(attempt)
    }
}

/// `draw` clamped into `[0, 1)`; NaN becomes 0.
fn clamp_draw(draw: f64) -> f64 {
    if draw.is_nan() || draw <= 0.0 {
        return 0.0;
    }
    draw.min(MAX_DRAW)
}

/// Maps 64 random bits onto `[0, 1)` uniformly, keeping the top
/// [`DRAW_MANTISSA_BITS`].
#[must_use]
pub fn unit_interval(bits: u64) -> f64 {
    let kept = bits >> DRAW_DISCARDED_BITS;
    // Both values fit an f64 exactly.
    kept as f64 / (1u64 << DRAW_MANTISSA_BITS) as f64
}

/// A uniform draw in `[0, 1)` from aws-lc-rs's [`SystemRandom`], filling
/// 8 bytes. Should the generator fail, the middle of the range is used, so
/// a retry still waits rather than spinning.
#[must_use]
pub fn system_draw() -> f64 {
    let mut bytes = [0u8; std::mem::size_of::<u64>()];
    if SystemRandom::new().fill(&mut bytes).is_err() {
        return FALLBACK_DRAW;
    }
    unit_interval(u64::from_le_bytes(bytes))
}

/// Counts throttled answers in a sliding window, so the provider can report
/// sustained throttling on the `Provider` (ADR-0174 Decision 2).
///
/// Time is passed in, never read, so the tracker is deterministic under
/// test. It keeps at most `threshold` timestamps.
#[derive(Clone, Debug)]
pub struct Throttling {
    threshold: usize,
    window: Duration,
    throttled_at: VecDeque<Instant>,
}

impl Default for Throttling {
    /// [`SUSTAINED_THROTTLE_COUNT`] within [`SUSTAINED_THROTTLE_WINDOW`].
    fn default() -> Self {
        Self::new(SUSTAINED_THROTTLE_COUNT, SUSTAINED_THROTTLE_WINDOW)
    }
}

impl Throttling {
    /// A tracker that calls throttling sustained once `threshold` throttled
    /// answers fall within `window`. A `threshold` of 0 is never sustained.
    #[must_use]
    pub fn new(threshold: usize, window: Duration) -> Self {
        Self {
            threshold,
            window,
            throttled_at: VecDeque::with_capacity(threshold),
        }
    }

    /// Records a throttled answer at `now`.
    pub fn record_throttled(&mut self, now: Instant) {
        self.throttled_at.push_back(now);
        while self.throttled_at.len() > self.threshold {
            self.throttled_at.pop_front();
        }
    }

    /// Records `error` at `now` if it is throttling; anything else is
    /// ignored.
    pub fn observe(&mut self, error: &Error, now: Instant) {
        if !error.is_throttled() {
            return;
        }
        self.record_throttled(now);
    }

    /// Throttled answers within the window ending at `now`, up to the
    /// threshold.
    #[must_use]
    pub fn count(&self, now: Instant) -> usize {
        self.throttled_at
            .iter()
            .filter(|at| now.saturating_duration_since(**at) <= self.window)
            .count()
    }

    /// Whether throttling is sustained at `now`: at least the threshold of
    /// throttled answers within the window.
    #[must_use]
    pub fn is_sustained(&self, now: Instant) -> bool {
        if self.threshold == 0 {
            return false;
        }
        self.count(now) >= self.threshold
    }

    /// Forgets every recorded answer, for example once the `Provider`'s
    /// condition has been cleared after a quiet period.
    pub fn clear(&mut self) {
        self.throttled_at.clear();
    }
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod retry_tests;
