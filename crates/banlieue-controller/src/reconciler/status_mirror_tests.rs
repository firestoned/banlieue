// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::status_mirror`].

#[cfg(test)]
mod tests {
    use banlieue_api::banlieue::VirtualMachineStatus;
    use banlieue_api::common::condition_types;
    use banlieue_api::common::{InitializationStatus, MachineAddress, MachineAddressType};
    use banlieue_provider_sdk::status::condition_status;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};

    use super::super::*;

    /// Test double — implements [`InfraMachineRead`] without needing a real
    /// VSphereMachine.
    struct FakeInfra {
        init: InitializationStatus,
        addresses: Vec<MachineAddress>,
        failure_domain: Option<String>,
        provider_id: Option<String>,
        conditions: Vec<Condition>,
    }

    impl InfraMachineRead for FakeInfra {
        fn initialization(&self) -> &InitializationStatus {
            &self.init
        }
        fn addresses(&self) -> &[MachineAddress] {
            &self.addresses
        }
        fn failure_domain(&self) -> Option<&str> {
            self.failure_domain.as_deref()
        }
        fn provider_id(&self) -> Option<&str> {
            self.provider_id.as_deref()
        }
        fn conditions(&self) -> &[Condition] {
            &self.conditions
        }
    }

    fn cond(type_: &str, status: &str, reason: &str) -> Condition {
        Condition {
            type_: type_.into(),
            status: status.into(),
            reason: reason.into(),
            message: format!("{type_}={status}"),
            observed_generation: Some(1),
            last_transition_time: Time(k8s_openapi::jiff::Timestamp::now()),
        }
    }

    fn baseline_status_scheduled() -> VirtualMachineStatus {
        let mut s = VirtualMachineStatus::default();
        s.conditions.push(cond(
            condition_types::SCHEDULED,
            condition_status::TRUE,
            "Scheduled",
        ));
        s
    }

    // ----------------------------------------------------------------------

    #[test]
    fn mirrors_initialization_and_addresses_verbatim() {
        let current = baseline_status_scheduled();
        let infra = FakeInfra {
            init: InitializationStatus {
                provisioned: Some(true),
            },
            addresses: vec![MachineAddress {
                address_type: MachineAddressType::InternalIP,
                address: "10.0.0.5".into(),
            }],
            failure_domain: Some("dc1-cluster-a".into()),
            provider_id: Some("vsphere://uuid".into()),
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
        };

        let out = mirror_status_from_infra(&current, &infra, 7);
        assert_eq!(out.initialization.provisioned, Some(true));
        assert_eq!(out.addresses.len(), 1);
        assert_eq!(out.addresses[0].address, "10.0.0.5");
        assert_eq!(out.observed_generation, Some(7));
    }

    #[test]
    fn maps_infra_ready_to_infrastructure_ready_condition() {
        let current = baseline_status_scheduled();
        let infra = FakeInfra {
            init: InitializationStatus::default(),
            addresses: vec![],
            failure_domain: None,
            provider_id: None,
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
        };
        let out = mirror_status_from_infra(&current, &infra, 1);

        let ir = out
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::INFRASTRUCTURE_READY)
            .expect("InfrastructureReady present");
        assert_eq!(ir.status, "True");
        assert_eq!(ir.reason, "Provisioned");
    }

    #[test]
    fn missing_infra_ready_condition_yields_pending_reason() {
        let current = baseline_status_scheduled();
        let infra = FakeInfra {
            init: InitializationStatus::default(),
            addresses: vec![],
            failure_domain: None,
            provider_id: None,
            conditions: vec![],
        };
        let out = mirror_status_from_infra(&current, &infra, 1);

        let ir = out
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::INFRASTRUCTURE_READY)
            .unwrap();
        assert_eq!(ir.status, "False");
        assert_eq!(ir.reason, "Pending");
    }

    #[test]
    fn aggregate_ready_is_true_when_scheduled_placement_valid_and_infra_ready() {
        let mut current = baseline_status_scheduled();
        // PlacementValid not explicitly set is treated as not-False = valid.
        current.conditions.push(cond(
            condition_types::PLACEMENT_VALID,
            condition_status::TRUE,
            "Valid",
        ));
        let infra = FakeInfra {
            init: InitializationStatus {
                provisioned: Some(true),
            },
            addresses: vec![],
            failure_domain: None,
            provider_id: None,
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
        };
        let out = mirror_status_from_infra(&current, &infra, 1);

        let ready = out
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::READY)
            .unwrap();
        assert_eq!(ready.status, "True");
        assert_eq!(ready.reason, "Reconciled");
    }

    #[test]
    fn aggregate_ready_is_false_when_infra_not_ready() {
        let current = baseline_status_scheduled();
        let infra = FakeInfra {
            init: InitializationStatus::default(),
            addresses: vec![],
            failure_domain: None,
            provider_id: None,
            conditions: vec![cond(
                condition_types::READY,
                condition_status::FALSE,
                "Cloning",
            )],
        };
        let out = mirror_status_from_infra(&current, &infra, 1);

        let ready = out
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::READY)
            .unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "InfrastructureNotReady");
    }

    #[test]
    fn aggregate_ready_is_false_when_placement_invalid_even_if_infra_ready() {
        let mut current = baseline_status_scheduled();
        current.conditions.push(cond(
            condition_types::PLACEMENT_VALID,
            condition_status::FALSE,
            "Drift",
        ));
        let infra = FakeInfra {
            init: InitializationStatus::default(),
            addresses: vec![],
            failure_domain: None,
            provider_id: None,
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
        };
        let out = mirror_status_from_infra(&current, &infra, 1);

        let ready = out
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::READY)
            .unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "PlacementInvalid");
    }

    #[test]
    fn aggregate_ready_is_false_when_not_scheduled() {
        // current has no Scheduled=True
        let current = VirtualMachineStatus::default();
        let infra = FakeInfra {
            init: InitializationStatus::default(),
            addresses: vec![],
            failure_domain: None,
            provider_id: None,
            conditions: vec![cond(
                condition_types::READY,
                condition_status::TRUE,
                "Provisioned",
            )],
        };
        let out = mirror_status_from_infra(&current, &infra, 1);

        let ready = out
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::READY)
            .unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "Scheduling");
    }
}
