// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::audit`]: the ledger, the comparison of the
//! API, the cache and the ledger, and the escalation from a bad pass to a
//! stale controller (ADR-0145).

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    use super::super::*;

    const GRACE: Duration = Duration::from_secs(120);
    const STALE_AFTER: Duration = Duration::from_secs(300);

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    /// One object as the audit sees it. `served`, `in_store` and `invoked`
    /// default to the healthy case in the callers that do not care.
    fn obs(
        namespace: &str,
        name: &str,
        age: u64,
        served: bool,
        in_store: bool,
        invoked: bool,
    ) -> Observed {
        Observed {
            namespace: namespace.into(),
            name: name.into(),
            uid: format!("uid-{namespace}-{name}"),
            age: secs(age),
            served,
            in_store,
            invoked,
        }
    }

    fn healthy(name: &str, age: u64) -> Observed {
        obs("banlieue-system", name, age, true, true, true)
    }

    // ---- ledger ------------------------------------------------------------

    #[test]
    fn a_ledger_remembers_the_uids_it_was_told_about() {
        let ledger = Ledger::default();
        assert!(!ledger.contains("a"));
        ledger.record("a");
        assert!(ledger.contains("a"));
        assert!(!ledger.contains("b"));
    }

    #[test]
    fn recording_a_uid_twice_is_one_entry() {
        let ledger = Ledger::default();
        ledger.record("a");
        ledger.record("a");
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn retain_drops_the_uids_that_no_longer_exist_and_says_how_many() {
        let ledger = Ledger::default();
        for uid in ["a", "b", "c"] {
            ledger.record(uid);
        }
        let live: HashSet<String> = ["a".to_string(), "c".to_string()].into();
        assert_eq!(ledger.retain(&live), 1);
        assert!(ledger.contains("a") && ledger.contains("c"));
        assert!(!ledger.contains("b"));
    }

    #[test]
    fn clones_of_a_ledger_share_state() {
        let ledger = Ledger::default();
        let clone = ledger.clone();
        clone.record("x");
        assert!(ledger.contains("x"));
    }

    // ---- building what the audit sees ---------------------------------------

    #[test]
    fn build_observed_joins_the_listing_with_the_store_the_ledger_and_the_gate() {
        let listed = vec![
            Listed {
                namespace: "banlieue-system".into(),
                name: "vm-a".into(),
                uid: "u-a".into(),
                age: secs(500),
            },
            Listed {
                namespace: "other".into(),
                name: "vm-b".into(),
                uid: "u-b".into(),
                age: secs(500),
            },
        ];
        let store: HashSet<String> = ["u-a".to_string()].into();
        let ledger = Ledger::default();
        ledger.record("u-a");

        let observed = build_observed(&listed, &store, &ledger, |ns| ns == "banlieue-system");

        assert_eq!(observed.len(), 2);
        assert_eq!(
            (
                observed[0].served,
                observed[0].in_store,
                observed[0].invoked
            ),
            (true, true, true)
        );
        assert_eq!(
            (
                observed[1].served,
                observed[1].in_store,
                observed[1].invoked
            ),
            (false, false, false)
        );
    }

    // ---- evaluation -----------------------------------------------------------

    #[test]
    fn nothing_is_wrong_when_every_object_is_cached_and_invoked() {
        let report = evaluate(&[healthy("a", 600), healthy("b", 900)], GRACE);
        assert!(report.is_clean());
        assert_eq!(report.total(), 0);
    }

    #[test]
    fn an_empty_cluster_is_clean() {
        assert!(evaluate(&[], GRACE).is_clean());
    }

    #[test]
    fn an_object_missing_from_the_store_past_the_grace_is_reported() {
        let missing = obs("banlieue-system", "vm-a", 600, true, false, true);
        let report = evaluate(&[missing], GRACE);
        assert_eq!(report.missing_from_store, ["banlieue-system/vm-a"]);
        assert!(report.never_reconciled.is_empty());
        assert!(!report.is_clean());
    }

    #[test]
    fn a_cached_object_never_invoked_past_the_grace_is_reported() {
        let starved = obs("banlieue-system", "vm-a", 600, true, true, false);
        let report = evaluate(&[starved], GRACE);
        assert_eq!(report.never_reconciled, ["banlieue-system/vm-a"]);
        assert!(report.missing_from_store.is_empty());
    }

    #[test]
    fn a_young_object_is_never_reported() {
        // Just created: the watch has not delivered it and the reconcile has not run yet.
        let young = obs("banlieue-system", "vm-a", 30, true, false, false);
        assert!(evaluate(&[young], GRACE).is_clean());
    }

    #[test]
    fn the_grace_boundary_is_exclusive() {
        let at = obs("n", "vm-a", 120, true, false, false);
        assert!(evaluate(&[at], GRACE).is_clean());
        let past = obs("n", "vm-a", 121, true, false, false);
        assert!(!evaluate(&[past], GRACE).is_clean());
    }

    #[test]
    fn an_object_in_an_unserved_namespace_is_never_reported() {
        // Not reconciling it is the point of the namespace gate (ADR-0130).
        let unserved = obs("someone-elses", "vm-a", 9_000, false, false, false);
        assert!(evaluate(&[unserved], GRACE).is_clean());
    }

    #[test]
    fn missing_from_the_store_wins_over_never_invoked_so_an_object_is_reported_once() {
        // The cache is the upstream cause: an object that is not cached cannot be invoked.
        let both = obs("n", "vm-a", 600, true, false, false);
        let report = evaluate(&[both], GRACE);
        assert_eq!(report.missing_from_store, ["n/vm-a"]);
        assert!(report.never_reconciled.is_empty());
        assert_eq!(report.total(), 1);
    }

    #[test]
    fn the_report_names_are_sorted_and_the_oldest_age_is_of_the_flagged_objects_only() {
        let report = evaluate(
            &[
                obs("n", "vm-b", 700, true, false, true),
                obs("n", "vm-a", 300, true, false, true),
                // Older than both, but healthy: must not set `oldest`.
                healthy("old-and-fine", 99_999),
            ],
            GRACE,
        );
        assert_eq!(report.missing_from_store, ["n/vm-a", "n/vm-b"]);
        assert_eq!(report.oldest, secs(700));
    }

    #[test]
    fn a_sample_caps_the_names_that_reach_a_log_line() {
        let many: Vec<Observed> = (0..50)
            .map(|i| obs("n", &format!("vm-{i:02}"), 600, true, false, true))
            .collect();
        let report = evaluate(&many, GRACE);
        assert_eq!(report.total(), 50);
        let sample = report.sample(20);
        assert_eq!(sample.len(), 20);
        assert_eq!(sample[0], "n/vm-00");
    }

    // ---- escalation ---------------------------------------------------------------

    #[test]
    fn a_clean_pass_is_healthy() {
        let mut tracker = Tracker::default();
        assert_eq!(
            tracker.observe(true, Instant::now(), STALE_AFTER),
            Verdict::Healthy
        );
    }

    #[test]
    fn a_bad_pass_is_degraded_until_it_has_lasted_the_stale_threshold() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        assert_eq!(tracker.observe(false, t0, STALE_AFTER), Verdict::Degraded);
        assert_eq!(
            tracker.observe(false, t0 + secs(240), STALE_AFTER),
            Verdict::Degraded
        );
        assert_eq!(
            tracker.observe(false, t0 + secs(300), STALE_AFTER),
            Verdict::Stale
        );
        assert_eq!(
            tracker.observe(false, t0 + secs(900), STALE_AFTER),
            Verdict::Stale
        );
    }

    #[test]
    fn a_clean_pass_resets_the_window() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        assert_eq!(tracker.observe(false, t0, STALE_AFTER), Verdict::Degraded);
        assert_eq!(
            tracker.observe(true, t0 + secs(200), STALE_AFTER),
            Verdict::Healthy
        );
        // A new bad run starts counting from here, not from t0.
        assert_eq!(
            tracker.observe(false, t0 + secs(260), STALE_AFTER),
            Verdict::Degraded
        );
        assert_eq!(
            tracker.observe(false, t0 + secs(500), STALE_AFTER),
            Verdict::Degraded
        );
        assert_eq!(
            tracker.observe(false, t0 + secs(560), STALE_AFTER),
            Verdict::Stale
        );
    }

    // ---- start-up --------------------------------------------------------------------

    #[test]
    fn the_audit_waits_out_the_startup_grace() {
        let cfg = AuditConfig::default();
        assert!(!cfg.past_startup(secs(0)));
        assert!(!cfg.past_startup(cfg.startup_grace - secs(1)));
        assert!(cfg.past_startup(cfg.startup_grace));
    }

    #[test]
    fn the_defaults_are_the_adr_numbers() {
        let cfg = AuditConfig::default();
        assert_eq!(cfg.interval, secs(60));
        assert_eq!(cfg.grace, secs(120));
        assert_eq!(cfg.startup_grace, secs(180));
        assert_eq!(cfg.stale_after, secs(300));
    }
}
