// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `retry.rs`.

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::super::*;
    use crate::error::{AwsFault, Error};

    /// The largest `f64` below 1.
    const JUST_UNDER_ONE: f64 = 1.0 - f64::EPSILON;

    fn throttled() -> Error {
        Error::Throttled(AwsFault::new(
            503,
            "RequestLimitExceeded",
            "slow down",
            "r-1",
        ))
    }

    fn ceilings() -> Vec<Duration> {
        (0..MAX_ATTEMPTS - 1)
            .map(|attempt| RetryPolicy::default().ceiling(attempt))
            .collect()
    }

    // ---- the policy's constants (ADR-0174 Decision 2) ----

    #[test]
    fn the_default_policy_is_base_1s_cap_20s_five_attempts() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.base, Duration::from_secs(1));
        assert_eq!(policy.cap, Duration::from_secs(20));
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(BASE_DELAY, policy.base);
        assert_eq!(MAX_DELAY, policy.cap);
        assert_eq!(MAX_ATTEMPTS, policy.max_attempts);
    }

    // ---- full jitter bounds ----

    #[test]
    fn ceilings_double_from_the_base() {
        assert_eq!(
            ceilings(),
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
            ]
        );
    }

    #[test]
    fn a_draw_of_zero_waits_nothing_on_every_attempt() {
        let policy = RetryPolicy::default();
        for attempt in 0..MAX_ATTEMPTS - 1 {
            assert_eq!(
                policy.delay(attempt, 0.0),
                Some(Duration::ZERO),
                "attempt {attempt}"
            );
        }
    }

    #[test]
    fn a_draw_just_under_one_stays_below_the_ceiling() {
        let policy = RetryPolicy::default();
        for attempt in 0..MAX_ATTEMPTS - 1 {
            let ceiling = policy.ceiling(attempt);
            let delay = policy.delay(attempt, JUST_UNDER_ONE).unwrap();
            assert!(
                delay < ceiling,
                "attempt {attempt}: {delay:?} >= {ceiling:?}"
            );
            assert!(
                delay > ceiling.mul_f64(0.999),
                "attempt {attempt}: {delay:?} not near {ceiling:?}"
            );
        }
    }

    #[test]
    fn a_draw_of_one_half_is_half_the_ceiling() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.delay(2, 0.5), Some(Duration::from_secs(2)));
    }

    #[test]
    fn the_cap_is_reached_and_held() {
        let policy = RetryPolicy {
            max_attempts: 10,
            ..RetryPolicy::default()
        };
        // 2^5 = 32 s > 20 s.
        assert_eq!(policy.ceiling(4), Duration::from_secs(16));
        assert_eq!(policy.ceiling(5), MAX_DELAY);
        assert_eq!(policy.ceiling(8), MAX_DELAY);
        let delay = policy.delay(8, JUST_UNDER_ONE).unwrap();
        assert!(delay < MAX_DELAY);
    }

    #[test]
    fn a_draw_outside_zero_to_one_is_clamped() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.delay(1, -3.0), Some(Duration::ZERO));
        assert_eq!(policy.delay(1, f64::NAN), Some(Duration::ZERO));
        let high = policy.delay(1, 7.0).unwrap();
        assert!(high < policy.ceiling(1));
        let inf = policy.delay(1, f64::INFINITY).unwrap();
        assert!(inf < policy.ceiling(1));
    }

    // ---- exhaustion ----

    #[test]
    fn attempts_are_exhausted_after_max_attempts() {
        let policy = RetryPolicy::default();
        assert!(policy.delay(MAX_ATTEMPTS - 2, 0.5).is_some());
        assert_eq!(policy.delay(MAX_ATTEMPTS - 1, 0.5), None);
        assert_eq!(policy.delay(MAX_ATTEMPTS, 0.5), None);
    }

    #[test]
    fn a_policy_of_one_attempt_never_waits() {
        let policy = RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        };
        assert_eq!(policy.delay(0, 0.5), None);
        assert!(!policy.should_retry(&throttled(), 0));
    }

    #[test]
    fn large_attempt_numbers_do_not_overflow() {
        let policy = RetryPolicy {
            max_attempts: u32::MAX,
            ..RetryPolicy::default()
        };
        for attempt in [31, 32, 63, 64, 1000, u32::MAX - 2, u32::MAX - 1, u32::MAX] {
            assert_eq!(policy.ceiling(attempt), MAX_DELAY, "attempt {attempt}");
            if let Some(delay) = policy.delay(attempt, JUST_UNDER_ONE) {
                assert!(delay < MAX_DELAY);
            }
        }
        assert_eq!(policy.delay(u32::MAX, 0.5), None);
    }

    #[test]
    fn a_huge_base_saturates_at_the_cap() {
        let policy = RetryPolicy {
            base: Duration::MAX,
            ..RetryPolicy::default()
        };
        assert_eq!(policy.ceiling(3), MAX_DELAY);
    }

    // ---- what is retried ----

    #[test]
    fn throttled_errors_are_retried_until_exhausted() {
        let policy = RetryPolicy::default();
        for attempt in 0..MAX_ATTEMPTS - 1 {
            assert!(
                policy.should_retry(&throttled(), attempt),
                "attempt {attempt}"
            );
        }
        assert!(!policy.should_retry(&throttled(), MAX_ATTEMPTS - 1));
        assert!(!policy.should_retry(&throttled(), u32::MAX));
    }

    #[test]
    fn nothing_but_throttling_is_retried() {
        let policy = RetryPolicy::default();
        let fault = || AwsFault::new(500, "X", "m", "");
        let others = [
            Error::InsufficientCapacity(fault()),
            Error::NotOffered(fault()),
            Error::QuotaExceeded(fault()),
            Error::IdempotentParameterMismatch(fault()),
            Error::NotFound(fault()),
            Error::Forbidden(fault()),
            Error::Unauthenticated(fault()),
            Error::InvalidRequest(fault()),
            Error::Server(fault()),
            Error::Invalid("x".to_string()),
            Error::Decode("x".to_string()),
            Error::Transport("x".to_string()),
        ];
        for error in &others {
            assert!(
                !policy.should_retry(error, 0),
                "{error} must not be retried"
            );
        }
    }

    // ---- the default draw ----

    #[test]
    fn the_system_draw_is_in_zero_to_one() {
        for _ in 0..1000 {
            let draw = system_draw();
            assert!((0.0..1.0).contains(&draw), "{draw}");
        }
    }

    #[test]
    fn the_system_draw_varies() {
        let draws: std::collections::BTreeSet<u64> =
            (0..16).map(|_| system_draw().to_bits()).collect();
        assert!(draws.len() > 1);
    }

    #[test]
    fn unit_interval_from_bytes_spans_zero_to_just_under_one() {
        assert_eq!(unit_interval(0), 0.0);
        let top = unit_interval(u64::MAX);
        assert!(top < 1.0 && top > 0.999_999);
    }

    #[test]
    fn delay_with_uses_the_injected_draw() {
        let policy = RetryPolicy::default();
        let half: Draw = || 0.5;
        assert_eq!(policy.delay_with(3, half), Some(Duration::from_secs(4)));
    }

    // ---- sustained throttling ----

    #[test]
    fn the_tracker_reports_sustained_after_the_threshold_within_the_window() {
        let start = Instant::now();
        let mut tracker = Throttling::default();
        for i in 0..SUSTAINED_THROTTLE_COUNT - 1 {
            tracker.record_throttled(start + Duration::from_secs(u64::try_from(i).unwrap()));
        }
        let now = start + Duration::from_secs(u64::try_from(SUSTAINED_THROTTLE_COUNT).unwrap());
        assert!(!tracker.is_sustained(now));
        tracker.record_throttled(now);
        assert!(tracker.is_sustained(now));
    }

    #[test]
    fn throttles_outside_the_window_do_not_count() {
        let start = Instant::now();
        let mut tracker = Throttling::new(3, Duration::from_secs(10));
        tracker.record_throttled(start);
        tracker.record_throttled(start + Duration::from_secs(1));
        tracker.record_throttled(start + Duration::from_secs(2));
        assert!(tracker.is_sustained(start + Duration::from_secs(5)));
        // The first falls out of the window.
        assert!(!tracker.is_sustained(start + Duration::from_secs(11)));
        assert_eq!(tracker.count(start + Duration::from_secs(11)), 2);
    }

    #[test]
    fn the_tracker_keeps_at_most_threshold_entries() {
        let start = Instant::now();
        let mut tracker = Throttling::new(3, Duration::from_secs(60));
        for i in 0..100 {
            tracker.record_throttled(start + Duration::from_millis(i));
        }
        assert_eq!(tracker.count(start + Duration::from_secs(1)), 3);
        assert!(tracker.is_sustained(start + Duration::from_secs(1)));
    }

    #[test]
    fn clearing_the_tracker_ends_sustained_throttling() {
        let start = Instant::now();
        let mut tracker = Throttling::new(1, Duration::from_secs(60));
        tracker.record_throttled(start);
        assert!(tracker.is_sustained(start));
        tracker.clear();
        assert!(!tracker.is_sustained(start));
    }

    #[test]
    fn a_zero_threshold_is_never_sustained_with_no_throttles() {
        let tracker = Throttling::new(0, Duration::from_secs(60));
        assert!(!tracker.is_sustained(Instant::now()));
    }

    #[test]
    fn observe_records_only_throttled_results() {
        let start = Instant::now();
        let mut tracker = Throttling::new(2, Duration::from_secs(60));
        tracker.observe(&Error::Transport("x".to_string()), start);
        tracker.observe(&throttled(), start);
        assert_eq!(tracker.count(start), 1);
        tracker.observe(&throttled(), start);
        assert!(tracker.is_sustained(start));
    }
}
