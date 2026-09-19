// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `pool.rs`.
//!
//! `reconcile` needs an API server; what is covered here is the pure
//! helpers it delegates to — above all the readiness-signal guard, which is
//! what makes ADR-0046's "readiness is required" decision safe rather than
//! merely strict.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::PoolReadiness;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};

    fn cond(type_: &str, status: &str) -> Condition {
        Condition {
            type_: type_.to_string(),
            status: status.to_string(),
            reason: "Test".to_string(),
            message: String::new(),
            last_transition_time: Time(k8s_openapi::jiff::Timestamp::now()),
            observed_generation: None,
        }
    }

    // ------------------------------------------------------------------
    // The readiness signal a pool watches for (ADR-0046 Decision 3)
    // ------------------------------------------------------------------

    #[test]
    fn readiness_maps_to_its_condition_type() {
        assert_eq!(
            readiness_condition_type(PoolReadiness::InfrastructureReady),
            "InfrastructureReady"
        );
        assert_eq!(
            readiness_condition_type(PoolReadiness::GuestReady),
            "GuestReady"
        );
    }

    /// The case this guard exists for: a pool set to `GuestReady` before
    /// ADR-0043 exists. No member ever publishes the condition, so the pool
    /// sits at zero forever. Without this it reports nothing at all — which
    /// is indistinguishable from a slow install.
    #[test]
    fn a_signal_no_member_publishes_is_absent() {
        let members = vec![
            vec![cond("Ready", "True"), cond("InfrastructureReady", "True")],
            vec![cond("Ready", "True")],
        ];
        assert!(readiness_signal_absent("GuestReady", &members));
    }

    /// Present but `False` is *not* absent: that is a member still coming
    /// up, which is the normal state of a filling pool and must not be
    /// reported as a misconfiguration.
    #[test]
    fn a_signal_present_but_false_is_not_absent() {
        let members = vec![vec![cond("InfrastructureReady", "False")]];
        assert!(!readiness_signal_absent("InfrastructureReady", &members));
    }

    #[test]
    fn a_signal_some_member_publishes_is_not_absent() {
        let members = vec![
            vec![cond("Ready", "True")],
            vec![cond("InfrastructureReady", "True")],
        ];
        assert!(!readiness_signal_absent("InfrastructureReady", &members));
    }

    /// An empty pool has published nothing yet. Reporting
    /// `ReadinessSignalAbsent` for a pool that simply has no members would
    /// fire on every pool's first reconcile, which would teach operators to
    /// ignore the condition.
    #[test]
    fn an_empty_pool_does_not_report_an_absent_signal() {
        assert!(!readiness_signal_absent("GuestReady", &[]));
    }

    // ------------------------------------------------------------------
    // Which reason `Warm` carries
    // ------------------------------------------------------------------

    /// Found by the live e2e: the diagnosis was landing only on `Capacity`
    /// while `Warm=False` still read `Filling` — a reason that means "making
    /// progress" on a pool that will never warm. `Warm` is what an operator
    /// reads to decide whether a pool is usable, so the cause belongs there.
    #[test]
    fn an_absent_readiness_signal_is_the_warm_reason() {
        assert_eq!(
            warm_reason(false, Some(pool_condition_reasons::READINESS_SIGNAL_ABSENT)),
            pool_condition_reasons::READINESS_SIGNAL_ABSENT
        );
    }

    /// A pool that is merely still filling keeps `Filling` — including when
    /// it is also out of capacity, which is a bound on growth rather than a
    /// reason the existing members are not warm.
    #[test]
    fn an_ordinary_not_yet_warm_pool_reports_filling() {
        assert_eq!(warm_reason(false, None), pool_condition_reasons::FILLING);
        assert_eq!(
            warm_reason(false, Some(pool_condition_reasons::MAX_REPLICAS_REACHED)),
            pool_condition_reasons::FILLING
        );
        assert_eq!(
            warm_reason(false, Some(pool_condition_reasons::ADDRESS_RANGE_EXHAUSTED)),
            pool_condition_reasons::FILLING
        );
    }

    #[test]
    fn a_warm_pool_reports_warm_whatever_else_is_blocked() {
        assert_eq!(warm_reason(true, None), pool_condition_reasons::WARM);
        assert_eq!(
            warm_reason(true, Some(pool_condition_reasons::READINESS_SIGNAL_ABSENT)),
            pool_condition_reasons::WARM
        );
    }
}
