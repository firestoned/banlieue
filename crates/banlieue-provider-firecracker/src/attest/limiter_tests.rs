// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `limiter.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant, SystemTime};

    /// A clock the test moves by hand.
    struct FakeClock {
        base: Instant,
        offset: Mutex<Duration>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                base: Instant::now(),
                offset: Mutex::new(Duration::ZERO),
            }
        }
        fn advance(&self, by: Duration) {
            *self.offset.lock().expect("lock") += by;
        }
    }

    impl Clock for &FakeClock {
        fn now(&self) -> Instant {
            self.base + *self.offset.lock().expect("lock")
        }
        fn wall(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
        }
    }

    const WINDOW: Duration = Duration::from_secs(60);

    #[test]
    fn allows_up_to_max_then_refuses() {
        let clock = FakeClock::new();
        let l = RateLimiter::new(&clock, 3, WINDOW);
        assert!(l.allow());
        assert!(l.allow());
        assert!(l.allow());
        assert!(!l.allow());
    }

    #[test]
    fn window_slides() {
        let clock = FakeClock::new();
        let l = RateLimiter::new(&clock, 2, WINDOW);
        assert!(l.allow());
        clock.advance(Duration::from_secs(30));
        assert!(l.allow());
        clock.advance(Duration::from_secs(31));
        // The first hit is now 61 s old; the second is 31 s old.
        assert!(l.allow());
        assert!(!l.allow());
    }

    #[test]
    fn refused_requests_count_against_the_window() {
        let clock = FakeClock::new();
        let l = RateLimiter::new(&clock, 1, WINDOW);
        assert!(l.allow());
        clock.advance(Duration::from_secs(40));
        assert!(!l.allow());
        clock.advance(Duration::from_secs(40));
        // 80 s after the allowed hit but only 40 s after the refused one.
        assert!(!l.allow());
    }

    #[test]
    fn zero_max_refuses_everything() {
        let clock = FakeClock::new();
        let l = RateLimiter::new(&clock, 0, WINDOW);
        assert!(!l.allow());
    }

    #[test]
    fn exposes_its_clock() {
        let clock = FakeClock::new();
        let l = RateLimiter::new(&clock, 1, WINDOW);
        assert_eq!(l.clock().wall(), SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn system_clock_is_monotonic_enough() {
        let c = SystemClock;
        let a = c.now();
        assert!(c.now() >= a);
        assert!(c.wall() > SystemTime::UNIX_EPOCH);
    }
}
