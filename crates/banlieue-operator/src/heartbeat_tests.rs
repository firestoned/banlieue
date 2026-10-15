// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::heartbeat`] (ADR-0102).

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use banlieue_api::banlieue::Heartbeat;
    use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
    use k8s_openapi::jiff::Timestamp;

    use super::super::*;

    const STALE_AFTER: Duration = Duration::from_secs(60);
    const NOW: i64 = 1_760_000_000;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(second).unwrap()
    }

    fn lease(renewed: Option<i64>, acquired: Option<i64>) -> Lease {
        Lease {
            spec: Some(LeaseSpec {
                renew_time: renewed.map(|s| MicroTime(at(s))),
                acquire_time: acquired.map(|s| MicroTime(at(s))),
                ..LeaseSpec::default()
            }),
            ..Lease::default()
        }
    }

    #[test]
    fn a_recently_renewed_lease_is_alive_until_its_deadline() {
        let reading = evaluate(Some(&lease(Some(NOW - 10), None)), at(NOW), STALE_AFTER);
        assert_eq!(reading.heartbeat, Heartbeat::Alive);
        assert_eq!(
            reading.last_heartbeat_time.map(|t| t.0.as_second()),
            Some(NOW - 10)
        );
        // Re-checked exactly when it would go stale, not a fixed interval later.
        assert_eq!(reading.recheck_in, Duration::from_secs(50));
    }

    /// The 2026-10-13 outage: the Lease still exists, its renewal stopped.
    #[test]
    fn a_lease_not_renewed_for_longer_than_stale_after_is_stale() {
        let twelve_hours_ago = NOW - 12 * 3600;
        let reading = evaluate(
            Some(&lease(Some(twelve_hours_ago), None)),
            at(NOW),
            STALE_AFTER,
        );
        assert_eq!(reading.heartbeat, Heartbeat::Stale);
        assert_eq!(
            reading.last_heartbeat_time.map(|t| t.0.as_second()),
            Some(twelve_hours_ago)
        );
        assert_eq!(reading.recheck_in, STALE_AFTER);
    }

    #[test]
    fn exactly_stale_after_is_still_alive() {
        let reading = evaluate(Some(&lease(Some(NOW - 60), None)), at(NOW), STALE_AFTER);
        assert_eq!(reading.heartbeat, Heartbeat::Alive);
    }

    #[test]
    fn no_lease_is_absent() {
        let reading = evaluate(None, at(NOW), STALE_AFTER);
        assert_eq!(reading.heartbeat, Heartbeat::Absent);
        assert!(reading.last_heartbeat_time.is_none());
        assert_eq!(reading.recheck_in, STALE_AFTER);
    }

    /// A Lease acquired but never renewed still proves when it was last held.
    #[test]
    fn the_acquire_time_stands_in_for_a_missing_renew_time() {
        let reading = evaluate(Some(&lease(None, Some(NOW - 5))), at(NOW), STALE_AFTER);
        assert_eq!(reading.heartbeat, Heartbeat::Alive);
    }

    /// A Lease that says nothing about when it was held proves nothing.
    #[test]
    fn a_lease_with_no_times_is_absent() {
        assert_eq!(
            evaluate(Some(&lease(None, None)), at(NOW), STALE_AFTER).heartbeat,
            Heartbeat::Absent
        );
        assert_eq!(
            evaluate(Some(&Lease::default()), at(NOW), STALE_AFTER).heartbeat,
            Heartbeat::Absent
        );
    }

    /// A renewal stamped ahead of the operator's clock (skew between hosts)
    /// is alive, never a negative age.
    #[test]
    fn a_renewal_from_the_future_is_alive() {
        let reading = evaluate(Some(&lease(Some(NOW + 30), None)), at(NOW), STALE_AFTER);
        assert_eq!(reading.heartbeat, Heartbeat::Alive);
        assert_eq!(reading.recheck_in, STALE_AFTER);
    }

    #[test]
    fn going_stale_and_coming_back_are_the_only_transitions_reported() {
        use Heartbeat::{Absent, Alive, Stale};
        assert_eq!(transition(Some(Alive), Stale), Some(Transition::WentStale));
        // First evaluation after an upgrade, of a provider already down.
        assert_eq!(transition(None, Stale), Some(Transition::WentStale));
        assert_eq!(transition(Some(Absent), Stale), Some(Transition::WentStale));
        assert_eq!(transition(Some(Stale), Alive), Some(Transition::Restored));

        assert_eq!(transition(Some(Stale), Stale), None);
        assert_eq!(transition(Some(Alive), Alive), None);
        assert_eq!(transition(None, Alive), None);
        assert_eq!(transition(None, Absent), None);
        assert_eq!(transition(Some(Stale), Absent), None);
    }

    #[test]
    fn the_requeue_is_the_sooner_of_the_deadline_and_the_normal_interval() {
        assert_eq!(
            requeue_after(Duration::from_secs(5)),
            Duration::from_secs(5)
        );
        assert_eq!(
            requeue_after(Duration::from_secs(600)),
            Duration::from_secs(banlieue_provider_sdk::reconciler::REQUEUE_DEFAULT_SECS)
        );
        // Never a zero-length requeue, which would spin.
        assert_eq!(requeue_after(Duration::ZERO), Duration::from_secs(1));
    }
}
