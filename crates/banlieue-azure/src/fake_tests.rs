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
    use crate::vm::{
        DEFAULT_ADMIN_USERNAME, NicRequest, VmRequest, nic_name, os_disk_name, vm_name,
    };

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
            os_disk_name: os_disk_name(UID),
            nic_id: fake_nic_id(RG, &nic_name(UID)),
            computer_name: vm_name(UID),
            admin_username: DEFAULT_ADMIN_USERNAME.to_string(),
            ssh_public_key: "ssh-ed25519 AAAA banlieue".to_string(),
            custom_data: b"#cloud-config\n".to_vec(),
            trusted_launch: true,
            tags: tags(),
            disk_encryption_set_id: None,
            encryption_at_host: false,
        }
    }

    async fn with_nic(f: &FakeArm) {
        f.put_nic(RG, &nic_name(UID), &nic()).await.expect("nic");
    }

    #[tokio::test]
    async fn a_vm_reads_back_with_the_tags_it_was_created_with() {
        let f = fake();
        assert!(f.vm_tags(RG, &vm_name(UID)).await.expect("read").is_none());
        with_nic(&f).await;
        f.put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_NVME))
            .await
            .expect("created");
        assert_eq!(
            f.vm_tags(RG, &vm_name(UID)).await.expect("read"),
            Some(tags())
        );

        let foreign = BTreeMap::from([("banlieue-owner-uid".to_string(), "other".to_string())]);
        f.set_vm_tags(RG, &vm_name(UID), foreign.clone());
        assert_eq!(
            f.vm_tags(RG, &vm_name(UID)).await.expect("read"),
            Some(foreign)
        );
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
    async fn the_os_disk_is_named_as_asked_and_starts_untagged() {
        // ADR-0173: ARM copies none of the VM's tags onto its OS disk.
        let f = fake();
        with_nic(&f).await;
        f.put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_NVME))
            .await
            .expect("created");
        let listed = f.list_resources(RG).await.expect("list");
        let disk = listed
            .iter()
            .find(|r| r.resource_type == "Microsoft.Compute/disks")
            .expect("the OS disk");
        assert_eq!(disk.name, os_disk_name(UID));
        assert!(disk.tags.is_empty());
    }

    #[tokio::test]
    async fn merged_disk_tags_are_added_to_the_disk_and_keep_the_rest() {
        let f = fake();
        with_nic(&f).await;
        f.put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_NVME))
            .await
            .expect("created");
        let mut first = BTreeMap::new();
        first.insert("cost-centre".to_string(), "x".to_string());
        f.merge_disk_tags(RG, &os_disk_name(UID), &first)
            .await
            .expect("merged");
        f.merge_disk_tags(RG, &os_disk_name(UID), &tags())
            .await
            .expect("merged");
        let listed = f.list_resources(RG).await.expect("list");
        let disk = listed
            .iter()
            .find(|r| r.name == os_disk_name(UID))
            .expect("the OS disk");
        let mut expected = tags();
        expected.extend(first);
        assert_eq!(disk.tags, expected);
    }

    #[tokio::test]
    async fn tagging_a_missing_disk_is_not_found() {
        let f = fake();
        let err = f.merge_disk_tags(RG, "nope", &tags()).await.unwrap_err();
        assert!(err.is_not_found());
    }

    #[tokio::test]
    async fn a_disk_tag_azure_refuses_is_refused() {
        let f = fake();
        with_nic(&f).await;
        f.put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_NVME))
            .await
            .expect("created");
        let mut bad = BTreeMap::new();
        bad.insert("a/b".to_string(), "x".to_string());
        assert!(
            f.merge_disk_tags(RG, &os_disk_name(UID), &bad)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_merge_past_azures_tag_limit_is_refused() {
        // ARM counts the tags the disk ends up with, not the request's.
        let f = fake();
        with_nic(&f).await;
        f.put_vm(RG, &vm_name(UID), &vm("1", "Standard_D2s_v7", IMAGE_NVME))
            .await
            .expect("created");
        let many: BTreeMap<String, String> = (0..crate::tags::MAX_TAGS_PER_RESOURCE)
            .map(|i| (format!("t{i}"), "v".to_string()))
            .collect();
        f.merge_disk_tags(RG, &os_disk_name(UID), &many)
            .await
            .expect("exactly the limit");
        assert!(
            f.merge_disk_tags(RG, &os_disk_name(UID), &tags())
                .await
                .is_err()
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

    // ----- ADR-0172: customer-managed keys, encryption at host, tags -----

    const DES: &str = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/rg-sec/providers/Microsoft.Compute/diskEncryptionSets/des-1";
    const DES_OTHER_SUB: &str = "/subscriptions/00000000-0000-0000-0000-0000000000ff/resourceGroups/rg-sec/providers/Microsoft.Compute/diskEncryptionSets/des-1";

    fn cmk_vm() -> VmRequest {
        let mut r = vm("1", "Standard_D2s_v7", IMAGE_NVME);
        r.disk_encryption_set_id = Some(DES.to_string());
        r
    }

    #[tokio::test]
    async fn a_vm_encrypted_with_a_known_set_is_created_and_records_it() {
        let f = fake();
        f.add_disk_encryption_set(DES, "eastus2");
        with_nic(&f).await;
        f.put_vm(RG, &vm_name(UID), &cmk_vm())
            .await
            .expect("created");
        assert_eq!(
            f.vm_disk_encryption_set(RG, &vm_name(UID)).as_deref(),
            Some(DES)
        );
    }

    #[tokio::test]
    async fn a_set_that_does_not_exist_is_refused() {
        let f = fake();
        with_nic(&f).await;
        let err = f
            .put_vm(RG, &vm_name(UID), &cmk_vm())
            .await
            .expect_err("refused");
        assert!(matches!(err, Error::InvalidRequest { .. }), "{err:?}");
        assert!(f.vm_disk_encryption_set(RG, &vm_name(UID)).is_none());
    }

    #[tokio::test]
    async fn a_set_in_another_subscription_or_region_is_refused() {
        let f = fake();
        f.add_disk_encryption_set(DES_OTHER_SUB, "eastus2");
        with_nic(&f).await;
        let mut r = cmk_vm();
        r.disk_encryption_set_id = Some(DES_OTHER_SUB.to_string());
        let err = f.put_vm(RG, &vm_name(UID), &r).await.expect_err("refused");
        assert!(err.to_string().contains("subscription"), "{err}");

        let f = fake();
        f.add_disk_encryption_set(DES, "westus3");
        with_nic(&f).await;
        let err = f
            .put_vm(RG, &vm_name(UID), &cmk_vm())
            .await
            .expect_err("refused");
        assert!(err.to_string().contains("region"), "{err}");
    }

    #[tokio::test]
    async fn a_malformed_set_id_is_refused() {
        let f = fake();
        with_nic(&f).await;
        let mut r = cmk_vm();
        r.disk_encryption_set_id = Some("/subscriptions/x/des".to_string());
        assert!(f.put_vm(RG, &vm_name(UID), &r).await.is_err());
    }

    /// A subscription without the `Microsoft.Compute/EncryptionAtHost`
    /// feature refuses the property (ADR-0172 Decision 3).
    #[tokio::test]
    async fn encryption_at_host_needs_the_subscription_feature() {
        let f = fake();
        with_nic(&f).await;
        let mut r = vm("1", "Standard_D2s_v7", IMAGE_NVME);
        r.encryption_at_host = true;
        let err = f.put_vm(RG, &vm_name(UID), &r).await.expect_err("refused");
        assert!(err.to_string().contains("EncryptionAtHost"), "{err}");
        assert!(!f.vm_encryption_at_host(RG, &vm_name(UID)));

        f.register_encryption_at_host();
        f.put_vm(RG, &vm_name(UID), &r).await.expect("created");
        assert!(f.vm_encryption_at_host(RG, &vm_name(UID)));
    }

    #[tokio::test]
    async fn tags_that_differ_only_in_case_are_refused() {
        let f = fake();
        let mut n = nic();
        n.tags.insert("Env".to_string(), "a".to_string());
        n.tags.insert("env".to_string(), "b".to_string());
        assert!(f.put_nic(RG, &nic_name(UID), &n).await.is_err());
    }

    #[tokio::test]
    async fn more_than_fifty_tags_are_refused() {
        let f = fake();
        let mut n = nic();
        for i in 0..crate::tags::MAX_TAGS_PER_RESOURCE {
            n.tags.insert(format!("extra-{i}"), "x".to_string());
        }
        assert!(f.put_nic(RG, &nic_name(UID), &n).await.is_err());
    }
}

/// The gallery side of the fake (ADR-0162): it refuses what ARM refuses.
#[cfg(test)]
mod publish_tests {
    use std::collections::BTreeMap;

    use super::super::*;
    use crate::api::{ArmApi, DiskAccess, PublishApi};
    use crate::error::Error;
    use crate::gallery::{
        DISK_STATE_ACTIVE_UPLOAD, DISK_STATE_UNATTACHED, GalleryImageVersionRequest,
        ImageDefinitionRequest, PROVISIONING_CREATING, PROVISIONING_SUCCEEDED, SnapshotRequest,
        UploadDiskRequest,
    };
    use crate::tags::OwnershipTags;
    use crate::upload::PageWriterFactory;

    const RG: &str = "rg";
    const GAL: &str = "gal";
    const DEF: &str = "kairos-tl";
    const VER: &str = "1.2.3";
    const DISK: &str = "up-disk";
    const SNAP: &str = "up-snap";
    /// 2 MiB virtual size plus the footer.
    const SIZE: u64 = 2 * 1024 * 1024 + 512;

    fn tags() -> BTreeMap<String, String> {
        OwnershipTags::new("uid", "VMImage", "ns", "p").to_map()
    }

    fn fake() -> FakeArm {
        FakeArm::from_fixtures("eastus2").expect("fixtures load")
    }

    fn definition(tl: bool) -> ImageDefinitionRequest {
        ImageDefinitionRequest {
            location: "eastus2".into(),
            offer: "kairos".into(),
            trusted_launch: tl,
            tags: tags(),
        }
    }

    fn disk(size: u64) -> UploadDiskRequest {
        UploadDiskRequest {
            location: "eastus2".into(),
            upload_size_bytes: size,
            tags: tags(),
            disk_encryption_set_id: None,
        }
    }

    async fn disk_id(f: &FakeArm) -> String {
        f.get_disk(RG, DISK).await.unwrap().unwrap().id
    }

    fn snapshot(source: String) -> SnapshotRequest {
        SnapshotRequest {
            location: "eastus2".into(),
            source_disk_id: source,
            tags: tags(),
            disk_encryption_set_id: None,
        }
    }

    fn version(snapshot_id: String) -> GalleryImageVersionRequest {
        GalleryImageVersionRequest {
            location: "eastus2".into(),
            snapshot_id,
            end_of_life: None,
            tags: tags(),
            disk_encryption_set_id: None,
        }
    }

    #[tokio::test]
    async fn upload_size_must_be_pages_and_virtual_size_whole_mib() {
        let f = fake();
        assert!(f.put_upload_disk(RG, DISK, &disk(SIZE + 1)).await.is_err());
        assert!(
            f.put_upload_disk(RG, DISK, &disk(SIZE + 512))
                .await
                .is_err()
        );
        f.put_upload_disk(RG, DISK, &disk(SIZE)).await.unwrap();
        let d = f.get_disk(RG, DISK).await.unwrap().unwrap();
        assert_eq!(d.disk_state, "ReadyToUpload");
        assert_eq!(d.upload_size_bytes, Some(SIZE));
    }

    #[tokio::test]
    async fn a_snapshot_of_a_disk_still_uploading_is_refused() {
        let f = fake();
        f.put_upload_disk(RG, DISK, &disk(SIZE)).await.unwrap();
        let id = disk_id(&f).await;
        assert!(
            f.put_snapshot(RG, SNAP, &snapshot(id.clone()))
                .await
                .is_err()
        );
        let DiskAccess::Granted(_) = f.grant_disk_write_access(RG, DISK, 3600).await.unwrap()
        else {
            panic!("the fake grants synchronously");
        };
        assert!(
            f.put_snapshot(RG, SNAP, &snapshot(id.clone()))
                .await
                .is_err()
        );
        f.revoke_disk_access(RG, DISK).await.unwrap();
        assert_eq!(
            f.get_disk(RG, DISK).await.unwrap().unwrap().disk_state,
            DISK_STATE_UNATTACHED
        );
        f.put_snapshot(RG, SNAP, &snapshot(id)).await.unwrap();
        assert!(f.has_snapshot(RG, SNAP));
    }

    #[tokio::test]
    async fn write_access_only_while_awaiting_upload() {
        let f = fake();
        f.put_upload_disk(RG, DISK, &disk(SIZE)).await.unwrap();
        f.grant_disk_write_access(RG, DISK, 3600).await.unwrap();
        // Granting again while active is allowed (a restarted Job).
        f.grant_disk_write_access(RG, DISK, 3600).await.unwrap();
        f.revoke_disk_access(RG, DISK).await.unwrap();
        assert!(f.grant_disk_write_access(RG, DISK, 3600).await.is_err());
    }

    #[tokio::test]
    async fn the_sas_opens_the_disk_blob_only_while_active_and_only_on_the_upload_host() {
        let f = fake();
        f.put_upload_disk(RG, DISK, &disk(SIZE)).await.unwrap();
        let DiskAccess::Granted(sas) = f.grant_disk_write_access(RG, DISK, 3600).await.unwrap()
        else {
            panic!("granted");
        };
        let writer = f.writer(&sas).expect("valid SAS");
        writer.put_page(0, vec![1; 512]).await.unwrap();
        assert_eq!(f.upload_blob(RG, DISK).unwrap().writes(), 1);
        f.revoke_disk_access(RG, DISK).await.unwrap();
        assert!(f.writer(&sas).is_err(), "a revoked SAS writes nothing");

        let g = fake();
        g.set_upload_host("md-x.blob.core.evil.example");
        g.put_upload_disk(RG, DISK, &disk(SIZE)).await.unwrap();
        let DiskAccess::Granted(sas) = g.grant_disk_write_access(RG, DISK, 3600).await.unwrap()
        else {
            panic!("granted");
        };
        assert!(matches!(
            g.writer(&sas).err(),
            Some(Error::UntrustedUploadHost(_))
        ));
    }

    #[tokio::test]
    async fn a_version_name_must_be_int_int_int() {
        let f = fake();
        for bad in ["1.2", "v1.2.3", "1.2.3.4"] {
            assert!(
                f.get_gallery_image_version(RG, GAL, DEF, bad)
                    .await
                    .is_err()
            );
            assert!(
                f.put_gallery_image_version(RG, GAL, DEF, bad, &version("s".into()))
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn a_source_without_nvme_is_refused_by_an_nvme_definition() {
        let f = fake();
        f.put_gallery_image_definition(RG, GAL, DEF, &definition(true))
            .await
            .unwrap();
        f.insert_snapshot(RG, "scsi-only", None, tags());
        let id = f.get_snapshot(RG, "scsi-only").await.unwrap().unwrap().id;
        let err = f
            .put_gallery_image_version(RG, GAL, DEF, VER, &version(id))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("diskControllerType"), "{err}");

        f.insert_snapshot(RG, "both", Some("SCSI, NVMe"), tags());
        let id = f.get_snapshot(RG, "both").await.unwrap().unwrap().id;
        f.put_gallery_image_version(RG, GAL, DEF, VER, &version(id))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_version_needs_its_definition() {
        let f = fake();
        f.insert_snapshot(RG, "both", Some("SCSI, NVMe"), tags());
        let id = f.get_snapshot(RG, "both").await.unwrap().unwrap().id;
        let err = f
            .put_gallery_image_version(RG, GAL, DEF, VER, &version(id))
            .await
            .unwrap_err();
        assert!(err.is_not_found());
    }

    #[tokio::test]
    async fn a_tag_with_a_slash_is_refused() {
        let f = fake();
        let mut bad = definition(false);
        bad.tags.insert("banlieue.io/owner-uid".into(), "x".into());
        assert!(matches!(
            f.put_gallery_image_definition(RG, GAL, DEF, &bad).await,
            Err(Error::InvalidTag(_))
        ));
    }

    #[tokio::test]
    async fn definition_features_are_immutable() {
        let f = fake();
        f.put_gallery_image_definition(RG, GAL, DEF, &definition(true))
            .await
            .unwrap();
        f.put_gallery_image_definition(RG, GAL, DEF, &definition(true))
            .await
            .unwrap();
        assert!(
            f.put_gallery_image_definition(RG, GAL, DEF, &definition(false))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_definition_with_versions_cannot_be_deleted() {
        let f = fake();
        f.put_gallery_image_definition(RG, GAL, DEF, &definition(true))
            .await
            .unwrap();
        f.insert_gallery_image_version(RG, GAL, DEF, VER, PROVISIONING_SUCCEEDED, tags());
        assert!(
            f.delete_gallery_image_definition(RG, GAL, DEF)
                .await
                .is_err()
        );
        assert_eq!(
            f.delete_gallery_image_version(RG, GAL, DEF, VER)
                .await
                .unwrap(),
            None
        );
        assert!(
            f.list_gallery_image_versions(RG, GAL, DEF)
                .await
                .unwrap()
                .is_empty()
        );
        f.delete_gallery_image_definition(RG, GAL, DEF)
            .await
            .unwrap();
        assert!(!f.has_gallery_image_definition(RG, GAL, DEF));
    }

    #[tokio::test]
    async fn a_new_version_is_creating_until_held_reads_pass() {
        let f = fake();
        f.hold_versions_creating(2);
        f.put_gallery_image_definition(RG, GAL, DEF, &definition(true))
            .await
            .unwrap();
        f.insert_snapshot(RG, "both", Some("SCSI, NVMe"), tags());
        let id = f.get_snapshot(RG, "both").await.unwrap().unwrap().id;
        f.put_gallery_image_version(RG, GAL, DEF, VER, &version(id))
            .await
            .unwrap();
        let state = |v: Option<crate::gallery::GalleryImageVersion>| v.unwrap().provisioning_state;
        assert_eq!(
            state(
                f.get_gallery_image_version(RG, GAL, DEF, VER)
                    .await
                    .unwrap()
            ),
            PROVISIONING_CREATING
        );
        assert_eq!(
            state(
                f.get_gallery_image_version(RG, GAL, DEF, VER)
                    .await
                    .unwrap()
            ),
            PROVISIONING_CREATING
        );
        assert_eq!(
            state(
                f.get_gallery_image_version(RG, GAL, DEF, VER)
                    .await
                    .unwrap()
            ),
            PROVISIONING_SUCCEEDED
        );
    }

    #[tokio::test]
    async fn absent_things_read_as_none_and_delete_as_success() {
        let f = fake();
        assert!(
            f.get_gallery_image_version(RG, GAL, DEF, VER)
                .await
                .unwrap()
                .is_none()
        );
        assert!(f.get_disk(RG, DISK).await.unwrap().is_none());
        assert!(f.get_snapshot(RG, SNAP).await.unwrap().is_none());
        assert_eq!(
            f.delete_gallery_image_version(RG, GAL, DEF, VER)
                .await
                .unwrap(),
            None
        );
        assert_eq!(f.delete_snapshot(RG, SNAP).await.unwrap(), None);
        assert_eq!(f.delete_disk(RG, DISK).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_stuck_upload_can_be_forced_back_to_active() {
        let f = fake();
        f.put_upload_disk(RG, DISK, &disk(SIZE)).await.unwrap();
        f.set_disk_state(RG, DISK, DISK_STATE_ACTIVE_UPLOAD);
        assert_eq!(
            f.get_disk(RG, DISK).await.unwrap().unwrap().disk_state,
            DISK_STATE_ACTIVE_UPLOAD
        );
        assert!(f.publish_calls() > 0);
    }

    const DES: &str = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/rg-sec/providers/Microsoft.Compute/diskEncryptionSets/des-1";

    /// ADR-0172 Decision 2: the upload disk, snapshot and version all name
    /// the set, and the fake refuses one it does not know.
    #[tokio::test]
    async fn the_publish_resources_are_encrypted_with_a_known_set_only() {
        let f = fake();
        let mut d = disk(SIZE);
        d.disk_encryption_set_id = Some(DES.into());
        assert!(
            f.put_upload_disk(RG, DISK, &d).await.is_err(),
            "unknown set"
        );
        f.add_disk_encryption_set(DES, "eastus2");
        f.put_upload_disk(RG, DISK, &d).await.unwrap();
        assert_eq!(f.upload_disk_encryption_set(RG, DISK).as_deref(), Some(DES));

        f.grant_disk_write_access(RG, DISK, 3600).await.unwrap();
        f.revoke_disk_access(RG, DISK).await.unwrap();
        let mut snap = snapshot(disk_id(&f).await);
        snap.disk_encryption_set_id = Some(DES.into());
        f.put_snapshot(RG, SNAP, &snap).await.unwrap();
        assert_eq!(f.snapshot_encryption_set(RG, SNAP).as_deref(), Some(DES));

        f.put_gallery_image_definition(RG, GAL, DEF, &definition(true))
            .await
            .unwrap();
        let snapshot_id = f.get_snapshot(RG, SNAP).await.unwrap().unwrap().id;
        let mut v = version(snapshot_id);
        v.disk_encryption_set_id = Some(DES.into());
        f.put_gallery_image_version(RG, GAL, DEF, VER, &v)
            .await
            .unwrap();
        assert_eq!(
            f.version_encryption_set(RG, GAL, DEF, VER).as_deref(),
            Some(DES)
        );
    }
}
