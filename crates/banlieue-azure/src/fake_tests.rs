// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `fake.rs`: the fake refuses what ARM refused in the spike
//! and allows what ARM allowed (rules/testing.md, rule 1).

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::*;
    use crate::api::{ArmApi, PowerAction};
    use crate::error::Error;
    use crate::tags::OwnershipTags;
    use crate::types::{Power, Provisioning};
    use crate::vm::{DEFAULT_ADMIN_USERNAME, NicRequest, VmRequest, nic_name, vm_name};

    const RG: &str = "rg";
    const UID: &str = "0b9c5f0e-7a40-4d27-9e2c-1c3a8f5d2b11";
    const IMAGE_NVME: &str = "/subscriptions/s/resourceGroups/g/providers/Microsoft.Compute/galleries/gal/images/nvme/versions/1.0.0";
    const IMAGE_SCSI: &str = "/subscriptions/s/resourceGroups/g/providers/Microsoft.Compute/galleries/gal/images/scsi/versions/1.0.0";
    const SUBNET: &str = "/subscriptions/s/resourceGroups/net/providers/Microsoft.Network/virtualNetworks/v/subnets/sn";
    const IMAGE_FLOOR_GB: u32 = 3;

    fn tags() -> BTreeMap<String, String> {
        OwnershipTags::new(UID, "AzureVirtualMachine", "ns", "p").to_map()
    }

    fn fake() -> FakeArm {
        let fake = FakeArm::from_fixtures("eastus2").expect("fixtures load");
        fake.register_image(IMAGE_NVME, IMAGE_FLOOR_GB, &["SCSI", "NVMe"]);
        fake.register_image(IMAGE_SCSI, IMAGE_FLOOR_GB, &["SCSI"]);
        fake
    }

    fn nic() -> NicRequest {
        NicRequest {
            location: "eastus2".to_string(),
            subnet_id: SUBNET.to_string(),
            accelerated_networking: true,
            tags: tags(),
        }
    }

    fn vm(zone: &str, size: &str, image: &str) -> VmRequest {
        VmRequest {
            location: "eastus2".to_string(),
            zone: zone.to_string(),
            size: size.to_string(),
            image_version_id: image.to_string(),
            os_disk_size_gb: 32,
            os_disk_sku: "StandardSSD_LRS".to_string(),
            nic_id: fake_nic_id(RG, &nic_name(UID)),
            computer_name: vm_name(UID),
            admin_username: DEFAULT_ADMIN_USERNAME.to_string(),
            ssh_public_key: "ssh-ed25519 AAAA banlieue".to_string(),
            custom_data: b"#cloud-config\n".to_vec(),
            trusted_launch: true,
            tags: tags(),
        }
    }

    async fn with_nic(f: &FakeArm) {
        f.put_nic(RG, &nic_name(UID), &nic()).await.expect("nic");
    }

    #[tokio::test]
    async fn a_vm_is_created_provisions_and_reports_ready() {
        let f = fake();
        with_nic(&f).await;
        f.put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_NVME))
            .await
            .expect("created");
        let view = f.vm_instance_view(RG, &vm_name(UID)).await.expect("view");
        assert_eq!(view.provisioning, Provisioning::Creating);

        f.guest_reports_ready(RG, &vm_name(UID));
        let view = f.vm_instance_view(RG, &vm_name(UID)).await.expect("view");
        assert!(view.guest_ready());
        assert_eq!(view.power, Power::Running);
        assert!(
            f.nic_private_ip(RG, &nic_name(UID))
                .await
                .expect("nic")
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_size_restricted_in_the_zone_is_refused() {
        let f = fake();
        with_nic(&f).await;
        // Captured: Standard_D2as_v6 is restricted for this subscription in 2 and 3.
        let err = f
            .put_vm(RG, &vm_name(UID), &vm("2", "Standard_D2as_v6", IMAGE_NVME))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::SkuNotAvailable { .. }), "{err}");
        assert!(err.is_zone_specific());
    }

    #[tokio::test]
    async fn an_nvme_only_size_from_a_scsi_only_image_is_refused_and_the_nic_stays() {
        let f = fake();
        with_nic(&f).await;
        let err = f
            .put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_SCSI))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidRequest { ref code, .. } if code == "InvalidParameter"),
            "{err}"
        );
        // The spike's orphan: the VM never existed, its NIC did.
        assert!(f.nic_private_ip(RG, &nic_name(UID)).await.is_ok());
        assert!(
            f.vm_instance_view(RG, &vm_name(UID))
                .await
                .unwrap_err()
                .is_not_found()
        );
    }

    #[tokio::test]
    async fn an_os_disk_below_the_image_is_refused() {
        let f = fake();
        with_nic(&f).await;
        let mut req = vm("1", "Standard_D2s_v7", IMAGE_NVME);
        req.os_disk_size_gb = IMAGE_FLOOR_GB - 1;
        assert!(matches!(
            f.put_vm(RG, &vm_name(UID), &req).await,
            Err(Error::InvalidRequest { .. })
        ));
    }

    #[tokio::test]
    async fn an_unknown_image_or_missing_nic_is_refused() {
        let f = fake();
        with_nic(&f).await;
        assert!(
            f.put_vm(
                RG,
                &vm_name(UID),
                &vm("1", "Standard_D2s_v7", "/no/such/image")
            )
            .await
            .unwrap_err()
            .is_not_found()
        );

        let f = fake();
        assert!(
            f.put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_NVME))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_tag_azure_refuses_is_refused() {
        let f = fake();
        let mut bad = nic();
        bad.tags
            .insert("banlieue.io/owner-uid".to_string(), UID.to_string());
        let err = f.put_nic(RG, &nic_name(UID), &bad).await.unwrap_err();
        assert!(
            matches!(err, Error::InvalidTag(_) | Error::InvalidRequest { .. }),
            "{err}"
        );
    }

    #[tokio::test]
    async fn quota_is_enforced() {
        // The fake's default quota is generous; the captured figures leave
        // 1 vCPU of StandardDsv7Family headroom, and D2s_v7 needs 2.
        let f = fake();
        f.use_captured_quota();
        with_nic(&f).await;
        let err = f
            .put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_NVME))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::QuotaExceeded { .. }), "{err}");
    }

    #[tokio::test]
    async fn quota_headroom_reflects_running_vms() {
        let f = fake();
        let before = crate::types::vcpu_headroom(
            &f.usages("eastus2").await.expect("u"),
            "StandardDsv7Family",
        )
        .expect("headroom");
        with_nic(&f).await;
        f.put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_NVME))
            .await
            .expect("created");
        let after = crate::types::vcpu_headroom(
            &f.usages("eastus2").await.expect("u"),
            "StandardDsv7Family",
        )
        .expect("headroom");
        assert_eq!(before - after, 2);
    }

    #[tokio::test]
    async fn deleting_a_vm_takes_its_nic_and_disk() {
        let f = fake();
        with_nic(&f).await;
        f.put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_NVME))
            .await
            .expect("created");
        f.delete_vm(RG, &vm_name(UID)).await.expect("deleted");
        assert!(
            f.nic_private_ip(RG, &nic_name(UID))
                .await
                .unwrap_err()
                .is_not_found()
        );
        assert!(
            f.list_resources(RG).await.expect("list").is_empty(),
            "nothing left behind"
        );
    }

    #[tokio::test]
    async fn deleting_what_is_already_gone_succeeds() {
        let f = fake();
        assert!(f.delete_vm(RG, "nope").await.is_ok());
        assert!(f.delete_nic(RG, "nope").await.is_ok());
        assert!(f.delete_disk(RG, "nope").await.is_ok());
    }

    #[tokio::test]
    async fn a_nic_attached_to_a_vm_cannot_be_deleted() {
        let f = fake();
        with_nic(&f).await;
        f.put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_NVME))
            .await
            .expect("created");
        let err = f.delete_nic(RG, &nic_name(UID)).await.unwrap_err();
        assert!(
            matches!(err, Error::InvalidRequest { ref code, .. } if code == "NicInUse"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_burst_of_deletes_is_throttled_with_a_hint() {
        let f = fake();
        f.set_delete_burst(3);
        for i in 0..3 {
            f.delete_disk(RG, &format!("d{i}"))
                .await
                .expect("within burst");
        }
        let err = f.delete_disk(RG, "d3").await.unwrap_err();
        assert!(matches!(err, Error::Throttled { .. }), "{err}");
        assert!(err.is_retryable());
        f.refill_delete_budget();
        assert!(f.delete_disk(RG, "d3").await.is_ok());
    }

    #[tokio::test]
    async fn a_creation_failure_surfaces_through_the_instance_view() {
        let f = fake();
        with_nic(&f).await;
        f.put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_NVME))
            .await
            .expect("created");
        f.fail_provisioning(RG, &vm_name(UID), "OSProvisioningTimedOut");
        let view = f.vm_instance_view(RG, &vm_name(UID)).await.expect("view");
        assert_eq!(
            view.provisioning,
            Provisioning::Failed {
                reason: "OSProvisioningTimedOut".to_string()
            }
        );
        // ARM refuses changes to a VM in that state.
        let err = f
            .power(RG, &vm_name(UID), PowerAction::Restart)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Conflict { .. }), "{err}");
    }

    #[tokio::test]
    async fn power_actions_move_the_power_state() {
        let f = fake();
        with_nic(&f).await;
        f.put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_NVME))
            .await
            .expect("created");
        f.guest_reports_ready(RG, &vm_name(UID));
        f.power(RG, &vm_name(UID), PowerAction::Deallocate)
            .await
            .expect("deallocate");
        let view = f.vm_instance_view(RG, &vm_name(UID)).await.expect("view");
        assert_eq!(view.power, Power::Deallocated);
        assert!(view.guest_ready(), "sticky");
        f.power(RG, &vm_name(UID), PowerAction::Start)
            .await
            .expect("start");
        assert_eq!(
            f.vm_instance_view(RG, &vm_name(UID))
                .await
                .expect("view")
                .power,
            Power::Running
        );
    }

    #[tokio::test]
    async fn listed_resources_carry_tags_and_creation_time() {
        let f = fake();
        with_nic(&f).await;
        let listed = f.list_resources(RG).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert!(listed[0].created_time.is_some());
        assert_eq!(listed[0].tags, tags());
    }

    #[tokio::test]
    async fn skus_come_from_the_captured_catalogue() {
        let f = fake();
        let skus = f.resource_skus("eastus2").await.expect("skus");
        assert!(skus.iter().any(|s| s.name == "Standard_D2s_v7"));
    }
}
