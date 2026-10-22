// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `sealing.rs` (ADR-0088 Decision 5).

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::common::FirstBootSealing;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
    use k8s_openapi::jiff::Timestamp;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    fn required(partitions: &[&str], timeout: u32) -> FirstBootSealing {
        FirstBootSealing {
            partitions: s(partitions),
            timeout_seconds: timeout,
        }
    }

    fn ready_at(epoch: i64) -> Vec<Condition> {
        vec![Condition {
            type_: "Ready".to_string(),
            status: "True".to_string(),
            reason: "Reconciled".to_string(),
            message: String::new(),
            observed_generation: Some(1),
            last_transition_time: Time(Timestamp::from_second(epoch).unwrap()),
        }]
    }

    fn at(epoch: i64) -> Timestamp {
        Timestamp::from_second(epoch).unwrap()
    }

    // ---- parse_sealed_report ----------------------------------------------

    #[test]
    fn a_report_is_a_comma_separated_list_of_labels() {
        assert_eq!(
            parse_sealed_report("COS_PERSISTENT,COS_DATA\n"),
            s(&["COS_PERSISTENT", "COS_DATA"])
        );
        assert_eq!(
            parse_sealed_report(" COS_PERSISTENT , "),
            s(&["COS_PERSISTENT"])
        );
    }

    #[test]
    fn an_empty_report_seals_nothing() {
        assert!(parse_sealed_report("").is_empty());
        assert!(parse_sealed_report("\n").is_empty());
    }

    /// Guest-supplied: anything that is not a plain label is dropped, and a
    /// label is never counted twice.
    #[test]
    fn a_report_keeps_only_plain_labels_once() {
        assert_eq!(
            parse_sealed_report("COS_PERSISTENT,cos_x,$(id),COS_PERSISTENT"),
            s(&["COS_PERSISTENT"])
        );
    }

    // ---- sticky_sealed_partitions -----------------------------------------

    #[test]
    fn sealed_partitions_are_sticky_across_an_empty_observation() {
        // The report lives in volatile state: a powered-off or just-booted
        // machine reports nothing, and has not become unsealed.
        assert_eq!(
            sticky_sealed_partitions(&s(&["COS_PERSISTENT"]), &[]),
            s(&["COS_PERSISTENT"])
        );
    }

    #[test]
    fn new_observations_are_added_in_order_without_duplicates() {
        assert_eq!(
            sticky_sealed_partitions(&s(&["COS_PERSISTENT"]), &s(&["COS_DATA", "COS_PERSISTENT"])),
            s(&["COS_PERSISTENT", "COS_DATA"])
        );
    }

    // ---- sealing_gate -----------------------------------------------------

    #[test]
    fn every_listed_partition_reported_satisfies_the_gate() {
        let req = required(&["COS_PERSISTENT"], 600);
        assert_eq!(
            sealing_gate(&req, &s(&["COS_PERSISTENT"]), &ready_at(0), at(10_000)),
            SealingGate::Satisfied
        );
    }

    #[test]
    fn a_missing_partition_is_pending_until_the_deadline() {
        let req = required(&["COS_PERSISTENT", "COS_DATA"], 600);
        assert_eq!(
            sealing_gate(&req, &s(&["COS_PERSISTENT"]), &ready_at(1_000), at(1_599)),
            SealingGate::Pending {
                missing: s(&["COS_DATA"])
            }
        );
    }

    #[test]
    fn a_missing_partition_after_the_deadline_times_out() {
        let req = required(&["COS_PERSISTENT"], 600);
        assert_eq!(
            sealing_gate(&req, &[], &ready_at(1_000), at(1_601)),
            SealingGate::TimedOut {
                missing: s(&["COS_PERSISTENT"])
            }
        );
    }

    /// No Ready=True yet means provisioning has not finished, so the clock
    /// has not started: pending, never timed out.
    #[test]
    fn the_clock_does_not_start_before_ready() {
        let req = required(&["COS_PERSISTENT"], 600);
        assert_eq!(
            sealing_gate(&req, &[], &[], at(1_000_000)),
            SealingGate::Pending {
                missing: s(&["COS_PERSISTENT"])
            }
        );
        let mut not_ready = ready_at(0);
        not_ready[0].status = "False".to_string();
        assert!(matches!(
            sealing_gate(&req, &[], &not_ready, at(1_000_000)),
            SealingGate::Pending { .. }
        ));
    }

    #[test]
    fn missing_partitions_are_named_in_declaration_order() {
        let req = required(&["COS_A", "COS_B", "COS_C"], 600);
        assert_eq!(
            missing_partitions(&req, &s(&["COS_B"])),
            s(&["COS_A", "COS_C"])
        );
    }
}
