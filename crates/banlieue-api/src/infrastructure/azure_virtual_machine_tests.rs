// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `azure_virtual_machine.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    const IMAGE: &str = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/images/providers/Microsoft.Compute/galleries/g/images/kairos/versions/1.0.0";
    const SUBNET: &str = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/net/providers/Microsoft.Network/virtualNetworks/v/subnets/s";

    fn minimal_spec() -> AzureVirtualMachineSpec {
        AzureVirtualMachineSpec {
            provider_id: None,
            failure_domain: None,
            provider_ref: ProviderReference {
                name: "azure-east".to_string(),
                namespace: None,
            },
            zone: "1".to_string(),
            size: "Standard_D2s_v5".to_string(),
            image_version_id: IMAGE.to_string(),
            os_disk_size_gi_b: 30,
            os_disk_sku: "Premium_LRS".to_string(),
            subnet_id: SUBNET.to_string(),
            accelerated_networking: false,
            trusted_launch: false,
            user_data: None,
            desired_power_state: PowerState::PoweredOn,
        }
    }

    #[test]
    fn provider_id_matches_the_capz_form() {
        assert_eq!(
            azure_provider_id("sub", "rg", "banlieue-abc"),
            "azure:///subscriptions/sub/resourceGroups/rg/providers/Microsoft.Compute/virtualMachines/banlieue-abc"
        );
    }

    #[test]
    fn minimal_spec_is_valid() {
        assert_eq!(minimal_spec().validate(), Ok(()));
    }

    #[test]
    fn user_data_at_the_limit_is_valid_and_one_byte_over_is_refused() {
        let mut spec = minimal_spec();
        spec.user_data = Some("x".repeat(MAX_USER_DATA_BYTES));
        assert_eq!(spec.validate(), Ok(()));
        spec.user_data = Some("x".repeat(MAX_USER_DATA_BYTES + 1));
        assert!(spec.validate().unwrap_err().contains("customData"));
    }

    #[test]
    fn suspended_is_refused() {
        let mut spec = minimal_spec();
        spec.desired_power_state = PowerState::Suspended;
        assert!(spec.validate().unwrap_err().contains("Suspended"));
    }

    #[test]
    fn powered_off_is_valid() {
        let mut spec = minimal_spec();
        spec.desired_power_state = PowerState::PoweredOff;
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn zero_disk_is_refused() {
        let mut spec = minimal_spec();
        spec.os_disk_size_gi_b = 0;
        assert!(spec.validate().unwrap_err().contains("osDiskSizeGiB"));
    }

    #[test]
    fn image_that_is_not_a_resource_id_is_refused() {
        let mut spec = minimal_spec();
        spec.image_version_id = "kairos:1.0.0".to_string();
        assert!(spec.validate().unwrap_err().contains("imageVersionId"));
    }

    #[test]
    fn subnet_that_is_not_a_resource_id_is_refused() {
        let mut spec = minimal_spec();
        spec.subnet_id = "default".to_string();
        assert!(spec.validate().unwrap_err().contains("subnetId"));
    }

    #[test]
    fn spec_serializes_with_camel_case_and_omits_false_flags() {
        let value = serde_json::to_value(minimal_spec()).unwrap();
        assert_eq!(value["osDiskSizeGiB"], 30);
        assert_eq!(value["imageVersionId"], IMAGE);
        assert!(value.get("trustedLaunch").is_none());
        assert!(value.get("acceleratedNetworking").is_none());
        assert!(value.get("providerID").is_none());
    }

    #[test]
    fn pending_operation_round_trips() {
        let status = AzureVirtualMachineStatus {
            pending_operation: Some(AzureOperation {
                action: AzureOperationAction::CreateVm,
                url: "https://management.azure.com/op".to_string(),
                kind: "AsyncOperation".to_string(),
            }),
            ..Default::default()
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["pendingOperation"]["action"], "CreateVm");
        let back: AzureVirtualMachineStatus = serde_json::from_value(value).unwrap();
        assert_eq!(back, status);
    }

    #[test]
    fn crd_names_do_not_collide_with_capz() {
        use kube::CustomResourceExt;
        let crd = AzureVirtualMachine::crd();
        assert_eq!(crd.spec.names.kind, "AzureVirtualMachine");
        assert_eq!(crd.spec.group, "infrastructure.banlieue.io");
        assert_eq!(crd.spec.names.short_names, Some(vec!["azvm".to_string()]));
    }
}
