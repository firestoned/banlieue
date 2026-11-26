// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `placement_failure.rs` (ADR-0173).

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use banlieue_api::common::{InitializationStatus, PowerState, ProviderReference};
    use banlieue_api::infrastructure::{
        AzureVirtualMachine, AzureVirtualMachineSpec, AzureVirtualMachineStatus,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;

    use super::super::*;

    const PROVIDER: &str = "azure-eastus2";
    const ZONE_1: &str = "azure-eastus2-1";
    const ZONE_2: &str = "azure-eastus2-2";

    fn at(secs: i64) -> Time {
        Time(Timestamp::from_second(secs).expect("ts"))
    }

    fn placement(provider: &str, fd: &str) -> ScheduledPlacement {
        ScheduledPlacement {
            provider_name: provider.into(),
            provider_class: "azure".into(),
            failure_domain: fd.into(),
            resolved_storage: Vec::new(),
            resolved_networks: Vec::new(),
            scheduled_at: None,
        }
    }

    fn created(status: &str, reason: &str, when: Time) -> Condition {
        Condition {
            type_: allocation::VM_CREATED.into(),
            status: status.into(),
            reason: reason.into(),
            message: "ZonalAllocationFailed: no capacity".into(),
            last_transition_time: when,
            observed_generation: Some(1),
        }
    }

    fn machine(
        fd: &str,
        provisioned: Option<bool>,
        conditions: Vec<Condition>,
    ) -> AzureVirtualMachine {
        AzureVirtualMachine {
            metadata: Default::default(),
            spec: AzureVirtualMachineSpec {
                provider_id: None,
                failure_domain: Some(fd.into()),
                provider_ref: ProviderReference {
                    name: PROVIDER.into(),
                    namespace: None,
                },
                zone: "1".into(),
                size: "Standard_D2s_v5".into(),
                image_version_id: "/subscriptions/s/versions/1.0.0".into(),
                os_disk_size_gi_b: 30,
                os_disk_sku: "Premium_LRS".into(),
                subnet_id: "/subscriptions/s/subnets/a".into(),
                accelerated_networking: false,
                trusted_launch: true,
                user_data: None,
                desired_power_state: PowerState::PoweredOn,
            },
            status: Some(AzureVirtualMachineStatus {
                initialization: InitializationStatus { provisioned },
                failure_domain: Some(fd.into()),
                conditions,
                ..Default::default()
            }),
        }
    }

    fn refused(fd: &str, when: Time) -> AzureVirtualMachine {
        machine(
            fd,
            Some(false),
            vec![created(
                condition_status::FALSE,
                allocation::ZONE_UNAVAILABLE,
                when,
            )],
        )
    }

    fn failure(fd: &str, when: Time) -> AllocationFailure {
        AllocationFailure {
            failure_domain: fd.into(),
            reason: allocation::ZONE_UNAVAILABLE.into(),
            message: "no capacity".into(),
            at: when,
        }
    }

    fn decision(fd: &str) -> Decision {
        Decision {
            provider_name: PROVIDER.into(),
            provider_namespace: "banlieue-system".into(),
            provider_class: "azure".into(),
            failure_domain_name: fd.into(),
            resolved_storage: Vec::new(),
            resolved_networks: Vec::new(),
            failure_domain_raw: BTreeMap::new(),
            failure_domain_labels: BTreeMap::new(),
        }
    }

    // ---- allocation_failure --------------------------------------------

    #[test]
    fn a_zone_unavailable_machine_of_the_recorded_placement_is_a_failure() {
        let got = allocation_failure(&refused(ZONE_1, at(100)), &placement(PROVIDER, ZONE_1))
            .expect("a failure");
        assert_eq!(got.failure_domain, ZONE_1);
        assert_eq!(got.reason, allocation::ZONE_UNAVAILABLE);
        assert_eq!(got.at, at(100));
        assert!(got.message.contains("ZonalAllocationFailed"));
    }

    #[test]
    fn a_provisioned_machine_is_never_re_placed() {
        let mut m = refused(ZONE_1, at(100));
        m.status
            .as_mut()
            .expect("status")
            .initialization
            .provisioned = Some(true);
        assert_eq!(
            allocation_failure(&m, &placement(PROVIDER, ZONE_1)),
            None,
            "a VM that ran keeps its placement (ADR-0125)"
        );
    }

    #[test]
    fn other_create_failures_are_not_zone_problems() {
        for (status, reason) in [
            (condition_status::FALSE, "QuotaExceeded"),
            (condition_status::FALSE, "CreateRefused"),
            (condition_status::TRUE, "VMExists"),
        ] {
            let m = machine(ZONE_1, Some(false), vec![created(status, reason, at(1))]);
            assert_eq!(
                allocation_failure(&m, &placement(PROVIDER, ZONE_1)),
                None,
                "{status}/{reason}"
            );
        }
        let none = machine(ZONE_1, None, Vec::new());
        assert_eq!(
            allocation_failure(&none, &placement(PROVIDER, ZONE_1)),
            None
        );
    }

    #[test]
    fn a_failure_for_another_placement_does_not_count() {
        // A terminating infra CR of the earlier placement, re-applied with
        // the new one's spec, still reports the old failure domain.
        let m = refused(ZONE_1, at(100));
        assert_eq!(allocation_failure(&m, &placement(PROVIDER, ZONE_2)), None);
        assert_eq!(
            allocation_failure(&m, &placement("another-provider", ZONE_1)),
            None
        );
    }

    // ---- exclusion_window ----------------------------------------------

    #[test]
    fn the_exclusion_doubles_from_five_minutes_to_an_hour() {
        let minutes = |m: u64| Duration::from_secs(m * 60);
        assert_eq!(exclusion_window(0), minutes(5));
        assert_eq!(exclusion_window(1), minutes(5));
        assert_eq!(exclusion_window(2), minutes(10));
        assert_eq!(exclusion_window(3), minutes(20));
        assert_eq!(exclusion_window(4), minutes(40));
        assert_eq!(exclusion_window(5), minutes(60));
        assert_eq!(exclusion_window(u32::MAX), ZONE_EXCLUSION_MAX);
    }

    // ---- record_failure ------------------------------------------------

    #[test]
    fn a_failure_is_counted_once_and_a_later_one_counts_up() {
        let mut failures = Vec::new();
        assert!(record_failure(
            &mut failures,
            PROVIDER,
            &failure(ZONE_1, at(100))
        ));
        assert!(
            !record_failure(&mut failures, PROVIDER, &failure(ZONE_1, at(100))),
            "the same failure read again on the next pass"
        );
        assert_eq!(failures[0].failures, 1);

        assert!(record_failure(
            &mut failures,
            PROVIDER,
            &failure(ZONE_1, at(500))
        ));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].failures, 2);
        assert_eq!(failures[0].last_failure_time, at(500));
    }

    #[test]
    fn the_most_recent_failure_is_last_and_the_list_is_bounded() {
        let mut failures = Vec::new();
        record_failure(&mut failures, PROVIDER, &failure(ZONE_1, at(100)));
        record_failure(&mut failures, PROVIDER, &failure(ZONE_2, at(200)));
        record_failure(&mut failures, PROVIDER, &failure(ZONE_1, at(300)));
        assert_eq!(failures[1].failure_domain, ZONE_1, "moved to the end");

        for i in 0..MAX_PLACEMENT_FAILURES {
            let secs = 1_000 + i64::try_from(i).expect("small");
            record_failure(
                &mut failures,
                PROVIDER,
                &failure(&format!("fd-{i}"), at(secs)),
            );
        }
        assert_eq!(failures.len(), MAX_PLACEMENT_FAILURES);
        assert!(
            failures.iter().all(|f| f.failure_domain.starts_with("fd-")),
            "the oldest entries were dropped"
        );
    }

    // ---- active_exclusions ---------------------------------------------

    #[test]
    fn an_exclusion_lasts_its_window_from_the_latest_failure() {
        let mut failures = Vec::new();
        record_failure(&mut failures, PROVIDER, &failure(ZONE_1, at(1_000)));
        let five_minutes = 300;
        assert_eq!(
            active_exclusions(&failures, at(1_000 + five_minutes - 1).0),
            vec![(PROVIDER.to_string(), ZONE_1.to_string())]
        );
        assert!(active_exclusions(&failures, at(1_000 + five_minutes).0).is_empty());

        // A second failure doubles it.
        record_failure(&mut failures, PROVIDER, &failure(ZONE_1, at(2_000)));
        assert_eq!(
            active_exclusions(&failures, at(2_000 + five_minutes + 1).0).len(),
            1
        );
        assert!(active_exclusions(&failures, at(2_000 + 2 * five_minutes).0).is_empty());
    }

    #[test]
    fn an_ordinary_pass_never_excludes_the_recorded_placement() {
        let mut failures = Vec::new();
        record_failure(&mut failures, PROVIDER, &failure(ZONE_1, at(1_000)));
        record_failure(&mut failures, PROVIDER, &failure(ZONE_2, at(1_000)));
        let recorded = placement(PROVIDER, ZONE_1);
        assert_eq!(
            scheduling_exclusions(&failures, Some(&recorded), at(1_001).0),
            vec![(PROVIDER.to_string(), ZONE_2.to_string())],
            "left to the infra CR's own report"
        );
        assert_eq!(scheduling_exclusions(&failures, None, at(1_001).0).len(), 2);
    }

    #[test]
    fn re_placing_excludes_the_refusing_zone_whatever_its_window() {
        let mut failures = Vec::new();
        record_failure(&mut failures, PROVIDER, &failure(ZONE_1, at(1_000)));
        let long_after = at(100_000).0;
        assert!(active_exclusions(&failures, long_after).is_empty());
        assert_eq!(
            replacement_exclusions(&failures, PROVIDER, &failure(ZONE_1, at(1_000)), long_after),
            vec![(PROVIDER.to_string(), ZONE_1.to_string())],
            "it is refusing the machine now"
        );

        record_failure(&mut failures, PROVIDER, &failure(ZONE_2, at(99_999)));
        let both =
            replacement_exclusions(&failures, PROVIDER, &failure(ZONE_1, at(1_000)), long_after);
        assert_eq!(
            both.len(),
            2,
            "and every zone still in its window: {both:?}"
        );
    }

    #[test]
    fn a_decision_moves_when_its_provider_or_failure_domain_differs() {
        let recorded = placement(PROVIDER, ZONE_1);
        assert!(!moves(&decision(ZONE_1), &recorded));
        assert!(moves(&decision(ZONE_2), &recorded));
        let mut elsewhere = decision(ZONE_1);
        elsewhere.provider_name = "another-provider".into();
        assert!(moves(&elsewhere, &recorded));
    }

    // ---- replaced_status -----------------------------------------------

    #[test]
    fn the_replaced_status_records_the_new_placement_and_says_why() {
        let current = VirtualMachineStatus {
            scheduled: Some(placement(PROVIDER, ZONE_1)),
            ..Default::default()
        };
        let mut failures = Vec::new();
        record_failure(&mut failures, PROVIDER, &failure(ZONE_1, at(100)));
        let next = replaced_status(
            &current,
            failures.clone(),
            &decision(ZONE_2),
            &failure(ZONE_1, at(100)),
            3,
            at(200),
        );
        let scheduled = next.scheduled.as_ref().expect("placement");
        assert_eq!(scheduled.failure_domain, ZONE_2);
        assert_eq!(scheduled.scheduled_at, Some(at(200)));
        assert_eq!(next.placement_failures, failures);
        assert_eq!(next.observed_generation, Some(3));

        let valid = find_condition(&next.conditions, condition_types::PLACEMENT_VALID)
            .expect("PlacementValid");
        assert_eq!(valid.status, condition_status::FALSE);
        assert_eq!(valid.reason, allocation::ZONE_UNAVAILABLE);
        assert!(valid.message.contains(ZONE_1) && valid.message.contains(ZONE_2));
        let ready = find_condition(&next.conditions, condition_types::READY).expect("Ready");
        assert_eq!(ready.status, condition_status::FALSE);
        assert_eq!(ready.reason, REASON_REPLACING);
    }
}
