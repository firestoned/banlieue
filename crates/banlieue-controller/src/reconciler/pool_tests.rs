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

    // ------------------------------------------------------------------
    // addressing.pool entry parsing (ADR-0056)
    // ------------------------------------------------------------------

    #[test]
    fn a_single_address_is_a_range_of_one() {
        let r = parse_address_pool_entry("192.0.2.40").unwrap();
        assert_eq!(r.start, Ipv4Addr::new(192, 0, 2, 40));
        assert_eq!(r.end, Ipv4Addr::new(192, 0, 2, 40));
    }

    #[test]
    fn a_low_high_range_parses_both_ends() {
        let r = parse_address_pool_entry("192.0.2.10-192.0.2.29").unwrap();
        assert_eq!(r.start, Ipv4Addr::new(192, 0, 2, 10));
        assert_eq!(r.end, Ipv4Addr::new(192, 0, 2, 29));
    }

    #[test]
    fn a_cidr_expands_to_its_full_block_network_and_broadcast_included() {
        let r = parse_address_pool_entry("192.0.2.4/30").unwrap();
        assert_eq!(r.start, Ipv4Addr::new(192, 0, 2, 4));
        assert_eq!(r.end, Ipv4Addr::new(192, 0, 2, 7));
    }

    #[test]
    fn a_slash_32_cidr_is_a_single_address() {
        let r = parse_address_pool_entry("192.0.2.9/32").unwrap();
        assert_eq!(r.start, Ipv4Addr::new(192, 0, 2, 9));
        assert_eq!(r.end, Ipv4Addr::new(192, 0, 2, 9));
    }

    #[test]
    fn a_slash_0_cidr_covers_every_address() {
        let r = parse_address_pool_entry("0.0.0.0/0").unwrap();
        assert_eq!(r.start, Ipv4Addr::new(0, 0, 0, 0));
        assert_eq!(r.end, Ipv4Addr::new(255, 255, 255, 255));
    }

    #[test]
    fn a_malformed_entry_names_itself_in_the_error() {
        let err = parse_address_pool_entry("not-an-ip").unwrap_err();
        assert!(err.contains("not-an-ip"), "{err}");
    }

    #[test]
    fn a_prefix_length_over_32_is_rejected() {
        let err = parse_address_pool_entry("192.0.2.0/33").unwrap_err();
        assert!(err.contains('3'), "{err}");
    }

    #[test]
    fn pool_inputs_rejects_a_pool_naming_the_bad_entry() {
        let mut json = serde_json::json!({
            "warmReplicas": 1,
            "maxReplicas": 2,
            "readiness": "InfrastructureReady",
            "template": {
                "spec": {
                    "classRef": { "name": "sandbox" },
                    "imageRef": { "name": "kairos" }
                }
            },
            "addressing": {
                "interface": "eth0",
                "pool": ["192.0.2.10-192.0.2.29", "garbage"],
                "prefix": 24
            }
        });
        let spec: banlieue_api::banlieue::VirtualMachinePoolSpec =
            serde_json::from_value(json.take()).unwrap();
        let pool = VirtualMachinePool::new("test", spec);
        let err = pool_inputs(&pool, "rev").unwrap_err();
        assert!(err.contains("garbage"), "{err}");
    }

    /// Multiple valid entries all resolve, each independently, in list order
    /// — the reconciler-side half of ADR-0056's ordering guarantee.
    #[test]
    fn pool_inputs_resolves_every_valid_entry_in_order() {
        let mut json = serde_json::json!({
            "warmReplicas": 1,
            "maxReplicas": 2,
            "readiness": "InfrastructureReady",
            "template": {
                "spec": {
                    "classRef": { "name": "sandbox" },
                    "imageRef": { "name": "kairos" }
                }
            },
            "addressing": {
                "interface": "eth0",
                "pool": ["192.0.2.40", "192.0.2.10-192.0.2.11"],
                "prefix": 24
            }
        });
        let spec: banlieue_api::banlieue::VirtualMachinePoolSpec =
            serde_json::from_value(json.take()).unwrap();
        let pool = VirtualMachinePool::new("test", spec);
        let inputs = pool_inputs(&pool, "rev").unwrap();
        assert_eq!(
            inputs.address_ranges,
            vec![
                AddressRange {
                    start: Ipv4Addr::new(192, 0, 2, 40),
                    end: Ipv4Addr::new(192, 0, 2, 40),
                },
                AddressRange {
                    start: Ipv4Addr::new(192, 0, 2, 10),
                    end: Ipv4Addr::new(192, 0, 2, 11),
                },
            ]
        );
    }
}
