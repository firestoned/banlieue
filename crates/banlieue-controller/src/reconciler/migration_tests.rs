// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::migration`].
//!
//! The function under test is pure, so every case is constructed from synthetic
//! inputs. Coverage matrix: drift kind × migration policy × annotation state.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use banlieue_api::banlieue::{
        MigrationPolicy, PlacementSpec, ResolvedResource, ScheduledPlacement, VirtualMachine,
        VirtualMachineSpec, VirtualMachineStatus,
    };
    use banlieue_api::common::{LocalObjectReference, PowerState};
    use kube::core::ObjectMeta;

    use super::super::*;
    use crate::reconciler::scheduler::Decision;

    // --- Builders ------------------------------------------------------------

    fn vm_with(
        policy: MigrationPolicy,
        annotations: BTreeMap<String, String>,
        scheduled: Option<ScheduledPlacement>,
    ) -> VirtualMachine {
        VirtualMachine {
            metadata: ObjectMeta {
                name: Some("vm-1".into()),
                namespace: Some("ns-1".into()),
                annotations: if annotations.is_empty() {
                    None
                } else {
                    Some(annotations)
                },
                ..Default::default()
            },
            spec: VirtualMachineSpec {
                class_ref: LocalObjectReference { name: "c".into() },
                image_ref: LocalObjectReference { name: "i".into() },
                placement: PlacementSpec::default(),
                desired_power_state: PowerState::PoweredOn,
                user_data: None,
                migration_policy: policy,
                paused: false,
                network_overrides: Vec::new(),
                hardware_override: None,
                folder: None,
            },
            status: scheduled.map(|s| VirtualMachineStatus {
                scheduled: Some(s),
                ..Default::default()
            }),
        }
    }

    fn placement(provider: &str, fd: &str) -> ScheduledPlacement {
        ScheduledPlacement {
            provider_name: provider.into(),
            provider_class: "vsphere".into(),
            failure_domain: fd.into(),
            resolved_storage: vec![],
            resolved_networks: vec![],
            scheduled_at: None,
        }
    }

    fn decision(provider: &str, fd: &str) -> Decision {
        Decision {
            provider_name: provider.into(),
            provider_namespace: "ns-1".into(),
            provider_class: "vsphere".into(),
            failure_domain_name: fd.into(),
            resolved_storage: vec![],
            resolved_networks: vec![],
            failure_domain_raw: BTreeMap::new(),
            failure_domain_labels: BTreeMap::new(),
        }
    }

    fn ann_migrate_true() -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert(ANNOTATION_MIGRATE.into(), ANNOTATION_MIGRATE_TRUE.into());
        m
    }

    // --- Cases ---------------------------------------------------------------

    #[test]
    fn first_schedule_returns_in_place() {
        // No prior status.scheduled → not a migration.
        let v = vm_with(MigrationPolicy::Automatic, BTreeMap::new(), None);
        let d = decision("p1", "fd-a");
        assert_eq!(evaluate(&v, &d), MigrationAction::InPlace);
    }

    #[test]
    fn no_drift_returns_in_place_regardless_of_policy() {
        for policy in [
            MigrationPolicy::Automatic,
            MigrationPolicy::Manual,
            MigrationPolicy::Never,
        ] {
            let v = vm_with(
                policy.clone(),
                BTreeMap::new(),
                Some(placement("p1", "fd-a")),
            );
            let d = decision("p1", "fd-a");
            assert_eq!(
                evaluate(&v, &d),
                MigrationAction::InPlace,
                "policy {policy:?} should be in-place when there's no drift"
            );
        }
    }

    #[test]
    fn never_sticks_to_old_under_any_drift() {
        let v = vm_with(
            MigrationPolicy::Never,
            BTreeMap::new(),
            Some(placement("p1", "fd-a")),
        );
        let d = decision("p2", "fd-b");
        assert_eq!(evaluate(&v, &d), MigrationAction::StickToOld);
    }

    #[test]
    fn manual_without_annotation_surface_only() {
        let v = vm_with(
            MigrationPolicy::Manual,
            BTreeMap::new(),
            Some(placement("p1", "fd-a")),
        );
        let d = decision("p2", "fd-a");
        let action = evaluate(&v, &d);
        match action {
            MigrationAction::SurfaceOnly {
                reason: PlacementDriftReason::ProviderChanged { from, to },
            } => {
                assert_eq!(from, "p1");
                assert_eq!(to, "p2");
            }
            other => panic!("expected SurfaceOnly with ProviderChanged, got {other:?}"),
        }
    }

    #[test]
    fn manual_with_annotation_recreates() {
        let v = vm_with(
            MigrationPolicy::Manual,
            ann_migrate_true(),
            Some(placement("p1", "fd-a")),
        );
        let d = decision("p2", "fd-a");
        let action = evaluate(&v, &d);
        assert!(
            matches!(action, MigrationAction::Recreate { .. }),
            "expected Recreate, got {action:?}"
        );
    }

    #[test]
    fn manual_with_wrong_annotation_value_does_not_recreate() {
        let mut anns = BTreeMap::new();
        anns.insert(ANNOTATION_MIGRATE.into(), "yes-please".into());
        let v = vm_with(MigrationPolicy::Manual, anns, Some(placement("p1", "fd-a")));
        let d = decision("p2", "fd-a");
        assert!(matches!(
            evaluate(&v, &d),
            MigrationAction::SurfaceOnly { .. }
        ));
    }

    #[test]
    fn automatic_recreates_on_provider_drift() {
        let v = vm_with(
            MigrationPolicy::Automatic,
            BTreeMap::new(),
            Some(placement("p1", "fd-a")),
        );
        let d = decision("p2", "fd-a");
        match evaluate(&v, &d) {
            MigrationAction::Recreate {
                reason: PlacementDriftReason::ProviderChanged { from, to },
            } => {
                assert_eq!(from, "p1");
                assert_eq!(to, "p2");
            }
            other => panic!("expected Recreate(ProviderChanged), got {other:?}"),
        }
    }

    #[test]
    fn automatic_recreates_on_failure_domain_drift() {
        let v = vm_with(
            MigrationPolicy::Automatic,
            BTreeMap::new(),
            Some(placement("p1", "fd-a")),
        );
        let d = decision("p1", "fd-b");
        match evaluate(&v, &d) {
            MigrationAction::Recreate {
                reason: PlacementDriftReason::FailureDomainChanged { provider, from, to },
            } => {
                assert_eq!(provider, "p1");
                assert_eq!(from, "fd-a");
                assert_eq!(to, "fd-b");
            }
            other => panic!("expected Recreate(FailureDomainChanged), got {other:?}"),
        }
    }

    #[test]
    fn drift_detection_prefers_provider_over_failure_domain() {
        // When BOTH provider and fd changed, the surfaced reason is the
        // provider change — more useful to operators.
        let v = vm_with(
            MigrationPolicy::Automatic,
            BTreeMap::new(),
            Some(placement("p1", "fd-a")),
        );
        let d = decision("p2", "fd-b");
        match evaluate(&v, &d) {
            MigrationAction::Recreate {
                reason: PlacementDriftReason::ProviderChanged { .. },
            } => {}
            other => panic!("expected provider-change to win, got {other:?}"),
        }
    }

    #[test]
    fn storage_drift_is_detected() {
        let mut prior = placement("p1", "fd-a");
        prior.resolved_storage = vec![ResolvedResource {
            class_name: "gold".into(),
            backend_id: "ds-old".into(),
        }];
        let v = vm_with(MigrationPolicy::Automatic, BTreeMap::new(), Some(prior));
        let mut d = decision("p1", "fd-a");
        d.resolved_storage = vec![ResolvedResource {
            class_name: "gold".into(),
            backend_id: "ds-new".into(),
        }];
        match evaluate(&v, &d) {
            MigrationAction::Recreate {
                reason: PlacementDriftReason::StorageChanged,
            } => {}
            other => panic!("expected StorageChanged, got {other:?}"),
        }
    }

    #[test]
    fn network_drift_is_detected() {
        let mut prior = placement("p1", "fd-a");
        prior.resolved_networks = vec![ResolvedResource {
            class_name: "prod".into(),
            backend_id: "pg-old".into(),
        }];
        let v = vm_with(MigrationPolicy::Automatic, BTreeMap::new(), Some(prior));
        let mut d = decision("p1", "fd-a");
        d.resolved_networks = vec![ResolvedResource {
            class_name: "prod".into(),
            backend_id: "pg-new".into(),
        }];
        match evaluate(&v, &d) {
            MigrationAction::Recreate {
                reason: PlacementDriftReason::NetworkChanged,
            } => {}
            other => panic!("expected NetworkChanged, got {other:?}"),
        }
    }

    #[test]
    fn reason_strings_are_stable() {
        // These are condition-reason wire strings — bumping them is a
        // breaking change for operator dashboards.
        assert_eq!(
            PlacementDriftReason::ProviderChanged {
                from: "a".into(),
                to: "b".into()
            }
            .reason(),
            "ProviderChanged"
        );
        assert_eq!(
            PlacementDriftReason::FailureDomainChanged {
                provider: "p".into(),
                from: "a".into(),
                to: "b".into()
            }
            .reason(),
            "FailureDomainChanged"
        );
        assert_eq!(
            PlacementDriftReason::StorageChanged.reason(),
            "StorageMappingChanged"
        );
        assert_eq!(
            PlacementDriftReason::NetworkChanged.reason(),
            "NetworkMappingChanged"
        );
    }

    // --- Recreate converges ----------------------------------------------------

    const GENERATION: i64 = 7;

    fn drift_to(provider: &str, fd: &str) -> PlacementDriftReason {
        PlacementDriftReason::FailureDomainChanged {
            provider: provider.into(),
            from: "fd-a".into(),
            to: fd.into(),
        }
    }

    fn at() -> k8s_openapi::apimachinery::pkg::apis::meta::v1::Time {
        k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
            "2026-11-26T22:00:00Z".parse().expect("timestamp"),
        )
    }

    /// The recreate path records the placement it is moving to before it
    /// deletes the infra CR, with `PlacementValid` and `Ready` false.
    #[test]
    fn recreate_status_records_the_new_placement() {
        let current = VirtualMachineStatus {
            scheduled: Some(placement("p1", "fd-a")),
            ..Default::default()
        };
        let d = decision("p1", "fd-b");
        let next = recreate_status(&current, &d, &drift_to("p1", "fd-b"), GENERATION, at());

        let scheduled = next.scheduled.expect("a placement");
        assert_eq!(scheduled.provider_name, "p1");
        assert_eq!(scheduled.failure_domain, "fd-b");
        let valid = banlieue_provider_sdk::status::find_condition(
            &next.conditions,
            banlieue_api::common::condition_types::PLACEMENT_VALID,
        )
        .expect("PlacementValid");
        assert_eq!(
            valid.status,
            banlieue_provider_sdk::status::condition_status::FALSE
        );
        assert_eq!(valid.reason, "FailureDomainChanged");
        let ready = banlieue_provider_sdk::status::find_condition(
            &next.conditions,
            banlieue_api::common::condition_types::READY,
        )
        .expect("Ready");
        assert_eq!(
            ready.status,
            banlieue_provider_sdk::status::condition_status::FALSE
        );
        assert_eq!(ready.reason, REASON_PLACEMENT_INVALID);
        assert_eq!(next.observed_generation, Some(GENERATION));
    }

    /// The regression: before the fix the recreate path left `scheduled` on
    /// the old placement, so every later pass saw the same drift and deleted
    /// again, and the VM was never recreated. With the new placement
    /// recorded, the next pass is in place and goes on to build the infra CR.
    #[test]
    fn after_a_recreate_the_next_pass_is_in_place() {
        let d = decision("p2", "fd-b");
        let before = vm_with(
            MigrationPolicy::Automatic,
            BTreeMap::new(),
            Some(placement("p1", "fd-a")),
        );
        let MigrationAction::Recreate { reason } = evaluate(&before, &d) else {
            panic!("drift under Automatic must recreate");
        };
        let mut after = before.clone();
        after.status = Some(recreate_status(
            before.status.as_ref().expect("status"),
            &d,
            &reason,
            GENERATION,
            at(),
        ));
        assert_eq!(evaluate(&after, &d), MigrationAction::InPlace);
    }

    /// Everything else on the status is carried over: the recreate path
    /// patches the whole status, so it must not drop what it did not set.
    #[test]
    fn recreate_status_keeps_the_rest_of_the_status() {
        let current = VirtualMachineStatus {
            scheduled: Some(placement("p1", "fd-a")),
            observed_power_state: Some(PowerState::PoweredOn),
            ..Default::default()
        };
        let next = recreate_status(
            &current,
            &decision("p1", "fd-b"),
            &drift_to("p1", "fd-b"),
            GENERATION,
            at(),
        );
        assert_eq!(next.observed_power_state, Some(PowerState::PoweredOn));
    }
}
