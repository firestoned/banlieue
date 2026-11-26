// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `vm.rs`. The expected bodies are the ones that booted a
//! Trusted Launch VM in the roadmap 23 phase 0 spike.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::super::*;
    use crate::tags::OwnershipTags;

    const UID: &str = "0b9c5f0e-7a40-4d27-9e2c-1c3a8f5d2b11";
    const IMAGE: &str = "/subscriptions/s/resourceGroups/g/providers/Microsoft.Compute/galleries/gal/images/img/versions/1.0.0";
    const NIC: &str = "/subscriptions/s/resourceGroups/g/providers/Microsoft.Network/networkInterfaces/banlieue-0b9c5f0e-7a40-4d27-9e2c-1c3a8f5d2b11-nic";
    const SUBNET: &str = "/subscriptions/s/resourceGroups/net/providers/Microsoft.Network/virtualNetworks/v/subnets/sn";
    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPlaceholderPlaceholderPlaceholderPlace banlieue";

    fn tags() -> BTreeMap<String, String> {
        OwnershipTags::new(
            UID,
            "AzureVirtualMachine",
            "banlieue-system",
            "azure-eastus2",
        )
        .to_map()
    }

    fn request() -> VmRequest {
        VmRequest {
            location: "eastus2".to_string(),
            zone: "1".to_string(),
            size: "Standard_D2s_v7".to_string(),
            image_version_id: IMAGE.to_string(),
            os_disk_size_gb: 32,
            os_disk_sku: "StandardSSD_LRS".to_string(),
            os_disk_name: os_disk_name(UID),
            nic_id: NIC.to_string(),
            computer_name: vm_name(UID),
            admin_username: DEFAULT_ADMIN_USERNAME.to_string(),
            ssh_public_key: KEY.to_string(),
            custom_data: b"#cloud-config\n".to_vec(),
            trusted_launch: true,
            tags: tags(),
            disk_encryption_set_id: None,
            encryption_at_host: false,
        }
    }

    const DES: &str = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/rg-sec/providers/Microsoft.Compute/diskEncryptionSets/des-1";

    #[test]
    fn the_body_is_the_shape_that_booted_in_the_spike() {
        let body = request().to_body().expect("valid");
        let expected = json!({
            "location": "eastus2",
            "zones": ["1"],
            "tags": tags(),
            "properties": {
                "hardwareProfile": {"vmSize": "Standard_D2s_v7"},
                "storageProfile": {
                    "imageReference": {"id": IMAGE},
                    "diskControllerType": "NVMe",
                    "osDisk": {
                        "name": os_disk_name(UID),
                        "createOption": "FromImage",
                        "deleteOption": "Delete",
                        "diskSizeGB": 32,
                        "managedDisk": {"storageAccountType": "StandardSSD_LRS"}
                    }
                },
                "osProfile": {
                    "computerName": vm_name(UID),
                    "adminUsername": "banlieue",
                    "customData": "I2Nsb3VkLWNvbmZpZwo=",
                    "linuxConfiguration": {
                        "disablePasswordAuthentication": true,
                        "provisionVMAgent": false,
                        "ssh": {"publicKeys": [{
                            "path": "/home/banlieue/.ssh/authorized_keys",
                            "keyData": KEY
                        }]}
                    }
                },
                "networkProfile": {"networkInterfaces": [
                    {"id": NIC, "properties": {"primary": true, "deleteOption": "Delete"}}
                ]},
                "securityProfile": {
                    "securityType": "TrustedLaunch",
                    "uefiSettings": {"secureBootEnabled": true, "vTpmEnabled": true}
                },
                "diagnosticsProfile": {"bootDiagnostics": {"enabled": true}}
            }
        });
        assert_eq!(body, expected);
    }

    #[test]
    fn boot_diagnostics_are_always_on() {
        // ADR-0163 Decision 4: a VM that fails to report is undiagnosable
        // without them, and cannot be changed after the timeout.
        let mut r = request();
        r.trusted_launch = false;
        let body = r.to_body().expect("valid");
        assert_eq!(
            body["properties"]["diagnosticsProfile"]["bootDiagnostics"]["enabled"],
            true
        );
        assert!(body["properties"].get("securityProfile").is_none());
    }

    /// ADR-0172 Decision 2: the OS disk names the customer-managed key.
    #[test]
    fn a_disk_encryption_set_encrypts_the_os_disk() {
        let mut r = request();
        r.disk_encryption_set_id = Some(DES.to_string());
        let body = r.to_body().expect("valid");
        assert_eq!(
            body["properties"]["storageProfile"]["osDisk"]["managedDisk"],
            json!({"storageAccountType": "StandardSSD_LRS", "diskEncryptionSet": {"id": DES}})
        );
    }

    #[test]
    fn without_a_disk_encryption_set_the_platform_key_is_used() {
        let body = request().to_body().expect("valid");
        assert!(
            body["properties"]["storageProfile"]["osDisk"]["managedDisk"]
                .get("diskEncryptionSet")
                .is_none()
        );
    }

    #[test]
    fn a_malformed_disk_encryption_set_is_refused_before_the_call() {
        let mut r = request();
        r.disk_encryption_set_id = Some("des-1".to_string());
        assert!(matches!(
            r.to_body(),
            Err(crate::Error::InvalidRequest { ref code, .. })
                if code == crate::encryption::CODE_INVALID_DISK_ENCRYPTION_SET
        ));
    }

    /// ADR-0172 Decision 3: encryption at host sits beside Trusted Launch in
    /// the one `securityProfile`.
    #[test]
    fn encryption_at_host_joins_trusted_launch_in_the_security_profile() {
        let mut r = request();
        r.encryption_at_host = true;
        let body = r.to_body().expect("valid");
        assert_eq!(
            body["properties"]["securityProfile"],
            json!({
                "securityType": "TrustedLaunch",
                "uefiSettings": {"secureBootEnabled": true, "vTpmEnabled": true},
                "encryptionAtHost": true
            })
        );
    }

    #[test]
    fn encryption_at_host_alone_sets_only_that() {
        let mut r = request();
        r.trusted_launch = false;
        r.encryption_at_host = true;
        let body = r.to_body().expect("valid");
        assert_eq!(
            body["properties"]["securityProfile"],
            json!({"encryptionAtHost": true})
        );
    }

    #[test]
    fn custom_data_over_the_limit_is_refused() {
        let mut r = request();
        r.custom_data = vec![b'x'; MAX_CUSTOM_DATA_BYTES + 1];
        assert!(matches!(
            r.to_body(),
            Err(crate::Error::InvalidRequest { .. })
        ));

        r.custom_data = vec![b'x'; MAX_CUSTOM_DATA_BYTES];
        assert!(r.to_body().is_ok());
    }

    #[test]
    fn empty_custom_data_is_omitted() {
        let mut r = request();
        r.custom_data.clear();
        let body = r.to_body().expect("valid");
        assert!(body["properties"]["osProfile"].get("customData").is_none());
    }

    #[test]
    fn a_zero_os_disk_or_missing_fields_are_refused() {
        let mut r = request();
        r.os_disk_size_gb = 0;
        assert!(r.to_body().is_err());

        let mut r = request();
        r.zone.clear();
        assert!(r.to_body().is_err());

        let mut r = request();
        r.ssh_public_key.clear();
        assert!(
            r.to_body().is_err(),
            "Azure requires a key when passwords are off"
        );
    }

    #[test]
    fn invalid_tags_are_refused_before_the_call() {
        let mut r = request();
        r.tags.insert("a/b".to_string(), "x".to_string());
        assert!(r.to_body().is_err());
    }

    #[test]
    fn names_are_derived_from_the_uid_and_fit_azure_limits() {
        let vm = vm_name(UID);
        assert_eq!(vm, format!("banlieue-{UID}"));
        assert!(vm.len() <= MAX_VM_NAME_LEN);
        assert_eq!(nic_name(UID), format!("banlieue-{UID}-nic"));
        assert!(nic_name(UID).len() <= MAX_NIC_NAME_LEN);
        assert_eq!(os_disk_name(UID), format!("banlieue-{UID}-osdisk"));
        assert!(os_disk_name(UID).len() <= MAX_DISK_NAME_LEN);
        assert!(
            os_disk_name(UID).starts_with(&vm),
            "a leftover disk must still read as the machine's"
        );
    }

    #[test]
    fn the_os_disk_is_named_so_it_can_be_tagged_after_the_create() {
        // ADR-0173: ARM takes no tags on osDisk, so the name is how the
        // provider finds the disk to tag it.
        let body = request().to_body().expect("valid");
        let os_disk = &body["properties"]["storageProfile"]["osDisk"];
        assert_eq!(os_disk["name"], os_disk_name(UID));
        assert!(
            os_disk.get("tags").is_none(),
            "ARM's OSDisk has no tags property"
        );
    }

    #[test]
    fn a_missing_or_oversized_os_disk_name_is_refused() {
        let mut r = request();
        r.os_disk_name.clear();
        assert!(r.to_body().is_err());

        let mut r = request();
        r.os_disk_name = "d".repeat(MAX_DISK_NAME_LEN + 1);
        assert!(r.to_body().is_err());
    }

    #[test]
    fn the_nic_body_attaches_one_dynamic_ip_on_the_subnet() {
        let body = NicRequest {
            location: "eastus2".to_string(),
            subnet_id: SUBNET.to_string(),
            accelerated_networking: true,
            tags: tags(),
        }
        .to_body()
        .expect("valid");
        assert_eq!(
            body,
            json!({
                "location": "eastus2",
                "tags": tags(),
                "properties": {
                    "enableAcceleratedNetworking": true,
                    "ipConfigurations": [{
                        "name": "ipconfig1",
                        "properties": {
                            "primary": true,
                            "privateIPAllocationMethod": "Dynamic",
                            "subnet": {"id": SUBNET}
                        }
                    }]
                }
            })
        );
    }
}
