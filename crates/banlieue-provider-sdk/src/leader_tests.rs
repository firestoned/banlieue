// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::leader`].
//!
//! These tests target only the pure decision function `decide_action` —
//! the async lease-API loop is exercised by the controller binary's
//! end-to-end smoke test, not unit-tested here.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
    use k8s_openapi::jiff::Timestamp;

    use super::super::{LeaderConfig, LeaseAction, decide_action};

    const OUR_IDENTITY: &str = "controller-pod-a";
    const OTHER_IDENTITY: &str = "controller-pod-b";
    const LEASE_DURATION_SECS: u64 = 15;
    const NOW_EPOCH_SECS: i64 = 1_700_000_000;

    fn config() -> LeaderConfig {
        LeaderConfig {
            namespace: "kube-system".to_string(),
            lease_name: "banlieue-controller".to_string(),
            identity: OUR_IDENTITY.to_string(),
            lease_duration: Duration::from_secs(LEASE_DURATION_SECS),
            renew_period: Duration::from_secs(5),
            retry_period: Duration::from_secs(2),
        }
    }

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).expect("valid epoch seconds")
    }

    fn lease_with(holder: Option<&str>, renew_secs: Option<i64>) -> Lease {
        Lease {
            spec: Some(LeaseSpec {
                holder_identity: holder.map(String::from),
                renew_time: renew_secs.map(|s| MicroTime(ts(s))),
                lease_duration_seconds: Some(LEASE_DURATION_SECS as i32),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn no_lease_yet_means_acquire_new() {
        let cfg = config();
        let action = decide_action(ts(NOW_EPOCH_SECS), None, &cfg);
        assert_eq!(action, LeaseAction::AcquireNew);
    }

    #[test]
    fn lease_with_no_holder_means_acquire_new() {
        let cfg = config();
        let lease = lease_with(None, Some(NOW_EPOCH_SECS));
        let action = decide_action(ts(NOW_EPOCH_SECS), Some(&lease), &cfg);
        assert_eq!(action, LeaseAction::AcquireNew);
    }

    #[test]
    fn lease_held_by_us_means_renew() {
        let cfg = config();
        let lease = lease_with(Some(OUR_IDENTITY), Some(NOW_EPOCH_SECS - 1));
        let action = decide_action(ts(NOW_EPOCH_SECS), Some(&lease), &cfg);
        assert_eq!(action, LeaseAction::Renew);
    }

    #[test]
    fn lease_held_by_us_but_stale_still_renew() {
        // Even if our own renew_time has expired, we re-renew rather than
        // gratuitously bumping transitions — the lease still names us.
        let cfg = config();
        let stale = NOW_EPOCH_SECS - (LEASE_DURATION_SECS as i64) - 60;
        let lease = lease_with(Some(OUR_IDENTITY), Some(stale));
        let action = decide_action(ts(NOW_EPOCH_SECS), Some(&lease), &cfg);
        assert_eq!(action, LeaseAction::Renew);
    }

    #[test]
    fn lease_held_by_another_within_duration_means_wait() {
        let cfg = config();
        let lease = lease_with(Some(OTHER_IDENTITY), Some(NOW_EPOCH_SECS - 1));
        let action = decide_action(ts(NOW_EPOCH_SECS), Some(&lease), &cfg);
        assert_eq!(action, LeaseAction::Wait);
    }

    #[test]
    fn lease_held_by_another_just_at_boundary_means_wait() {
        // renew_time + lease_duration == now is still "alive" by convention —
        // strict less-than-or-equal would race the holder's renewal cycle.
        let cfg = config();
        let lease = lease_with(
            Some(OTHER_IDENTITY),
            Some(NOW_EPOCH_SECS - (LEASE_DURATION_SECS as i64)),
        );
        let action = decide_action(ts(NOW_EPOCH_SECS), Some(&lease), &cfg);
        assert_eq!(action, LeaseAction::Wait);
    }

    #[test]
    fn lease_held_by_another_past_duration_means_take_over() {
        let cfg = config();
        let lease = lease_with(
            Some(OTHER_IDENTITY),
            Some(NOW_EPOCH_SECS - (LEASE_DURATION_SECS as i64) - 1),
        );
        let action = decide_action(ts(NOW_EPOCH_SECS), Some(&lease), &cfg);
        assert_eq!(action, LeaseAction::TakeOver);
    }

    #[test]
    fn lease_held_by_another_with_no_renew_time_means_take_over() {
        let cfg = config();
        let lease = lease_with(Some(OTHER_IDENTITY), None);
        let action = decide_action(ts(NOW_EPOCH_SECS), Some(&lease), &cfg);
        assert_eq!(action, LeaseAction::TakeOver);
    }

    #[test]
    fn lease_with_no_spec_means_acquire_new() {
        let cfg = config();
        let lease = Lease::default();
        let action = decide_action(ts(NOW_EPOCH_SECS), Some(&lease), &cfg);
        assert_eq!(action, LeaseAction::AcquireNew);
    }

    #[test]
    fn config_rejects_zero_durations() {
        let mut cfg = config();
        cfg.lease_duration = Duration::ZERO;
        assert!(
            cfg.validate().is_err(),
            "zero lease_duration must be invalid"
        );

        let mut cfg = config();
        cfg.renew_period = Duration::ZERO;
        assert!(cfg.validate().is_err(), "zero renew_period must be invalid");

        let mut cfg = config();
        cfg.retry_period = Duration::ZERO;
        assert!(cfg.validate().is_err(), "zero retry_period must be invalid");
    }

    #[test]
    fn config_rejects_renew_not_less_than_lease() {
        let mut cfg = config();
        cfg.renew_period = cfg.lease_duration;
        assert!(
            cfg.validate().is_err(),
            "renew_period must be strictly less than lease_duration"
        );
    }

    #[test]
    fn config_rejects_empty_identity() {
        let mut cfg = config();
        cfg.identity = String::new();
        assert!(cfg.validate().is_err(), "empty identity must be invalid");
    }

    #[test]
    fn config_default_identity_is_pod_name_or_hostname() {
        let id = LeaderConfig::default_identity();
        assert!(!id.is_empty(), "default identity must never be empty");
    }
}
