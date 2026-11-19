// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
use super::*;

const T0_SECS: u64 = 1_800_000_000;

fn at(offset_secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(T0_SECS + offset_secs)
}

fn limiter() -> RateLimiter {
    RateLimiter::new(RateLimit {
        max_requests: 2,
        window: Duration::from_secs(10),
    })
}

#[test]
fn admits_up_to_the_limit_then_refuses() {
    let mut l = limiter();
    assert!(l.admit("a", at(0)));
    assert!(l.admit("a", at(1)));
    assert!(!l.admit("a", at(2)));
}

#[test]
fn refused_requests_do_not_extend_the_window() {
    let mut l = limiter();
    assert!(l.admit("a", at(0)));
    assert!(l.admit("a", at(1)));
    assert!(!l.admit("a", at(9)));
    // The first request left the window at t=10; the refusal at 9 did not count.
    assert!(l.admit("a", at(10)));
}

#[test]
fn window_slides_with_the_injected_clock() {
    let mut l = limiter();
    assert!(l.admit("a", at(0)));
    assert!(l.admit("a", at(0)));
    assert!(!l.admit("a", at(9)));
    assert!(l.admit("a", at(10)));
}

#[test]
fn subjects_are_limited_independently() {
    let mut l = limiter();
    assert!(l.admit("a", at(0)));
    assert!(l.admit("a", at(0)));
    assert!(!l.admit("a", at(0)));
    assert!(l.admit("b", at(0)));
}

#[test]
fn idle_subjects_are_forgotten() {
    let mut l = limiter();
    assert!(l.admit("a", at(0)));
    assert!(l.admit("b", at(100)));
    assert_eq!(l.seen.len(), 1);
}

#[test]
fn default_limit_is_nonzero() {
    let d = RateLimit::default();
    assert!(d.max_requests > 0 && !d.window.is_zero());
}
