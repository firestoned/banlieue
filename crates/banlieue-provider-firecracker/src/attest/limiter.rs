// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A per-guest sliding-window rate limiter with an injectable clock.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

/// Requests a guest may make per window by default (renewal at half life is a
/// few requests).
pub const DEFAULT_MAX_REQUESTS: usize = 6;
/// The default window.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(60);

/// The time source: a monotonic instant for limiting, wall time for validity.
pub trait Clock {
    /// Monotonic now.
    fn now(&self) -> Instant;
    /// Wall-clock now, for certificate validity.
    fn wall(&self) -> SystemTime;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// At most `max` requests per `window`, counting refused ones too.
#[derive(Debug)]
pub struct RateLimiter<C: Clock = SystemClock> {
    clock: C,
    max: usize,
    window: Duration,
    hits: Mutex<VecDeque<Instant>>,
}

impl<C: Clock> RateLimiter<C> {
    /// A limiter allowing `max` requests per `window`.
    pub fn new(clock: C, max: usize, window: Duration) -> Self {
        Self {
            clock,
            max,
            window,
            hits: Mutex::new(VecDeque::new()),
        }
    }

    /// The clock this limiter reads.
    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// Records one request; `true` if it is within the limit.
    pub fn allow(&self) -> bool {
        let now = self.clock.now();
        let mut hits = self.hits.lock().unwrap_or_else(PoisonError::into_inner);
        while let Some(&oldest) = hits.front() {
            if now.saturating_duration_since(oldest) < self.window {
                break;
            }
            hits.pop_front();
        }
        // Refused requests are recorded too, so a burst cannot wait itself out
        // by being refused. The record is bounded by `max` plus one.
        hits.push_back(now);
        while hits.len() > self.max.saturating_add(1) {
            hits.pop_front();
        }
        hits.len() <= self.max
    }
}

#[cfg(test)]
#[path = "limiter_tests.rs"]
mod limiter_tests;
