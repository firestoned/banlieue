// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A per-subject sliding-window rate limit with an injectable clock.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

/// At most `max_requests` per subject in any `window`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimit {
    /// Requests allowed per window.
    pub max_requests: usize,
    /// The window length.
    pub window: Duration,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self {
            max_requests: DEFAULT_MAX_REQUESTS,
            window: DEFAULT_WINDOW,
        }
    }
}

/// Renewal at half life is a few requests; a burst is a bug or an attack.
const DEFAULT_MAX_REQUESTS: usize = 6;
/// The default window.
const DEFAULT_WINDOW: Duration = Duration::from_secs(60);

/// Request history per subject.
#[derive(Debug, Default)]
pub(crate) struct RateLimiter {
    limit: RateLimit,
    seen: HashMap<String, Vec<SystemTime>>,
}

impl RateLimiter {
    pub(crate) fn new(limit: RateLimit) -> Self {
        Self {
            limit,
            seen: HashMap::new(),
        }
    }

    /// Record a request at `now`; false when it exceeds the limit.
    pub(crate) fn admit(&mut self, subject: &str, now: SystemTime) -> bool {
        let window = self.limit.window;
        // A timestamp in the future (the clock stepped back) still counts.
        let in_window = |t: &SystemTime| now.duration_since(*t).map_or(true, |age| age < window);
        self.seen.retain(|_, times| {
            times.retain(in_window);
            !times.is_empty()
        });
        let times = self.seen.entry(subject.to_owned()).or_default();
        if times.len() >= self.limit.max_requests {
            return false;
        }
        times.push(now);
        true
    }
}

#[cfg(test)]
#[path = "ratelimit_tests.rs"]
mod ratelimit_tests;
