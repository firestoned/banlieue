// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `publish.rs`: every step and every adopt path of the
//! publish Job, against `FakeArm` (which refuses what ARM refuses) and its
//! page blob. No network, no subscription.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use banlieue_azure::fake::FakeArm;
    use banlieue_azure::fake_blob::FakePageBlob;
    use banlieue_azure::gallery::{
        DISK_STATE_ACTIVE_UPLOAD, DISK_STATE_UNATTACHED, PROVISIONING_CREATING,
        PROVISIONING_FAILED, PROVISIONING_SUCCEEDED, UploadDiskRequest, snapshot_name,
        upload_disk_name,
    };
    use banlieue_azure::tags::{TAG_OWNER_NAMESPACE, TAG_OWNER_UID};
    use banlieue_azure::upload::{PageWriter, PageWriterFactory, UploadOptions};
    use banlieue_azure::{ArmApi, PublishApi, SasUrl, TAG_SOURCE_SHA256, gallery_version_name};
    use banlieue_diskformat::vhd::{DISK_TYPE_FIXED, FOOTER_LEN, parse_footer};
    use clap::Parser;
    use sha2::{Digest, Sha256};

    use super::super::*;

    const RG: &str = "rg-gallery";
    const GAL: &str = "gal";
    const DEF: &str = "kairos-tl";
    const UID: &str = "0b9c5f0e-7a40-4d27-9e2c-1c3a8f5d2b11";
    const OTHER_UID: &str = "11111111-2222-3333-4444-555555555555";
    const CHUNK: usize = 64 * 1024;
    /// Not a whole MiB, so the VHD gets padding.
    const RAW_LEN: usize = 1024 * 1024 + 4096;

    /// A raw disk with data at the start and in the middle, zeros
    /// elsewhere.
    fn raw_disk() -> Vec<u8> {
        let mut raw = vec![0u8; RAW_LEN];
        raw[..1000].fill(0xab);
        raw[700 * 1024..700 * 1024 + 10].fill(0x5a);
        raw[RAW_LEN - 1] = 1;
        raw
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    struct Source {
        _dir: tempfile::TempDir,
        path: std::path::PathBuf,
        bytes: Vec<u8>,
        sha: String,
    }

    fn source() -> Source {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.raw");
        let bytes = raw_disk();
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        let sha = hex(&Sha256::digest(&bytes));
        Source {
            _dir: dir,
            path,
            bytes,
            sha,
        }
    }

    fn tags_for(uid: &str) -> OwnershipTags {
        OwnershipTags::new(uid, OWNER_KIND_VMIMAGE, "banlieue-system", "azure-eastus2")
    }

    fn plan(src: &Source) -> PublishPlan {
        PublishPlan {
            target: Target {
                resource_group: RG.into(),
                gallery: GAL.into(),
                definition: DEF.into(),
                version: gallery_version_name(&src.sha),
                location: "eastus2".into(),
            },
            trusted_launch: true,
            tags: tags_for(UID),
            extra_tags: BTreeMap::new(),
            disk_encryption_set_id: None,
            source: src.path.clone(),
            checksum_hex: src.sha.clone(),
        }
    }

    fn timings() -> Timings {
        Timings {
            poll_interval: Duration::from_millis(1),
            version_deadline: Duration::from_millis(500),
            operation_deadline: Duration::from_millis(500),
            upload: UploadOptions {
                chunk_bytes: CHUNK,
                parallelism: 4,
                max_attempts: 3,
                initial_backoff: Duration::from_millis(1),
                max_backoff: Duration::from_millis(2),
            },
        }
    }

    fn fake() -> Arc<FakeArm> {
        Arc::new(FakeArm::from_fixtures("eastus2").unwrap())
    }

    /// Hands out the fake's writers and keeps the page blob, which the
    /// publish deletes with its upload disk at the end.
    struct Keeping {
        fake: Arc<FakeArm>,
        kept: Mutex<Option<Arc<FakePageBlob>>>,
        disk: String,
    }

    impl Keeping {
        fn new(fake: &Arc<FakeArm>, p: &PublishPlan) -> Self {
            Self {
                fake: fake.clone(),
                kept: Mutex::new(None),
                disk: upload_disk_name(GAL, DEF, &p.target.version),
            }
        }

        fn blob(&self) -> Option<Arc<FakePageBlob>> {
            self.kept.lock().unwrap().clone()
        }
    }

    impl PageWriterFactory for Keeping {
        fn writer(&self, sas: &SasUrl) -> banlieue_azure::Result<Arc<dyn PageWriter>> {
            let writer = self.fake.writer(sas)?;
            *self.kept.lock().unwrap() = self.fake.upload_blob(RG, &self.disk);
            Ok(writer)
        }
    }

    async fn owned_version(fake: &FakeArm, version: &str, sha: &str, state: &str, uid: &str) {
        let mut tags = tags_for(uid).to_map();
        tags.insert(TAG_SOURCE_SHA256.into(), sha.into());
        fake.insert_gallery_image_version(RG, GAL, DEF, version, state, tags);
    }

    // ----- publish -----

    #[tokio::test]
    async fn publishes_a_raw_disk_end_to_end() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        let writers = Keeping::new(&arm, &p);
        let published = publish(arm.as_ref(), &writers, &p, &timings())
            .await
            .unwrap();

        assert!(
            published
                .version_id
                .ends_with(&format!("/versions/{}", p.target.version))
        );
        assert_eq!(published.source_sha256, src.sha);
        assert_eq!(published.min_os_disk_size_gib, 1);

        let version = arm
            .get_gallery_image_version(RG, GAL, DEF, &p.target.version)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(version.provisioning_state, PROVISIONING_SUCCEEDED);
        assert_eq!(version.tags[TAG_SOURCE_SHA256], src.sha);
        assert_eq!(version.tags[TAG_OWNER_UID], UID);
        assert!(!version.tags.contains_key(TAG_OWNER_NAMESPACE));

        let definition = arm
            .get_gallery_image_definition(RG, GAL, DEF)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(definition.features["SecurityType"], "TrustedLaunch");
        assert_eq!(definition.features["DiskControllerTypes"], "SCSI, NVMe");

        // Temporaries are gone (finding 6).
        assert!(!arm.has_upload_disk(RG, &upload_disk_name(GAL, DEF, &p.target.version)));
        assert!(!arm.has_snapshot(RG, &snapshot_name(GAL, DEF, &p.target.version)));
    }

    #[tokio::test]
    async fn the_upload_writes_exactly_the_non_zero_ranges_of_the_vhd() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        let writers = Keeping::new(&arm, &p);
        publish(arm.as_ref(), &writers, &p, &timings())
            .await
            .unwrap();
        let blob = writers.blob().expect("an upload happened");
        let layout = FixedVhdLayout::for_raw_len(src.bytes.len() as u64).unwrap();
        assert_eq!(blob.size(), layout.total_len);
        assert!(layout.padding > 0, "the test disk needs padding");

        // The footer the blob holds is a valid fixed-VHD footer for the
        // padded size (its timestamp is the publish's own).
        let footer_at = usize::try_from(layout.virtual_size).unwrap();
        let held = blob.read(layout.virtual_size, FOOTER_LEN);
        let mut footer = [0u8; FOOTER_LEN];
        footer.copy_from_slice(&held);
        let parsed = parse_footer(&footer).expect("valid footer");
        assert_eq!(parsed.current_size, layout.virtual_size);
        assert_eq!(parsed.disk_type, DISK_TYPE_FIXED);
        assert_eq!(parsed.unique_id, unique_id_for(&src.sha));

        // raw + zero padding + that footer, written range by range, with
        // every all-zero range skipped.
        let mut expected = src.bytes.clone();
        expected.resize(footer_at, 0);
        expected.extend_from_slice(&footer);
        blob.check_nonzero_written(&expected, CHUNK).unwrap();
        // Data at the start, at 700 KiB, the raw disk's last byte, and the
        // footer in a range of its own past the 2 MiB virtual size: the
        // other 29 ranges (zeros and padding) are skipped.
        assert_eq!(
            blob.written_ranges().len(),
            4,
            "{:?}",
            blob.written_ranges()
        );
    }

    #[tokio::test]
    async fn a_checksum_mismatch_makes_no_call_at_all() {
        let src = source();
        let mut p = plan(&src);
        p.checksum_hex = "00".repeat(32);
        p.target.version = gallery_version_name(&p.checksum_hex);
        let arm = fake();
        let err = publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "ChecksumMismatch");
        assert_eq!(arm.publish_calls(), 0);
    }

    #[tokio::test]
    async fn a_version_that_is_not_the_digest_s_name_is_refused_before_any_call() {
        let src = source();
        let mut p = plan(&src);
        p.target.version = "1.2.3".into();
        let arm = fake();
        let err = publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "InvalidArgument");
        assert_eq!(arm.publish_calls(), 0);
    }

    #[tokio::test]
    async fn an_unreadable_source_is_reported_as_such() {
        let src = source();
        let mut p = plan(&src);
        p.source = src.path.with_file_name("missing.raw");
        let arm = fake();
        let err = publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "SourceUnreadable");
        assert_eq!(arm.publish_calls(), 0);
    }

    #[tokio::test]
    async fn a_succeeded_version_is_adopted_without_uploading() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        owned_version(
            &arm,
            &p.target.version,
            &src.sha,
            PROVISIONING_SUCCEEDED,
            UID,
        )
        .await;
        let writers = Keeping::new(&arm, &p);
        let published = publish(arm.as_ref(), &writers, &p, &timings())
            .await
            .unwrap();
        assert!(published.version_id.ends_with(&p.target.version));
        assert!(writers.blob().is_none(), "nothing uploaded");
    }

    #[tokio::test]
    async fn a_creating_version_is_adopted_and_polled() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        arm.hold_versions_creating(3);
        owned_version(
            &arm,
            &p.target.version,
            &src.sha,
            PROVISIONING_CREATING,
            UID,
        )
        .await;
        let writers = Keeping::new(&arm, &p);
        publish(arm.as_ref(), &writers, &p, &timings())
            .await
            .unwrap();
        assert!(writers.blob().is_none(), "nothing uploaded");
    }

    #[tokio::test]
    async fn a_failed_version_is_deleted_and_created_again() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        owned_version(&arm, &p.target.version, &src.sha, PROVISIONING_FAILED, UID).await;
        let writers = Keeping::new(&arm, &p);
        publish(arm.as_ref(), &writers, &p, &timings())
            .await
            .unwrap();
        assert!(writers.blob().is_some(), "uploaded again");
        let v = arm
            .get_gallery_image_version(RG, GAL, DEF, &p.target.version)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(v.provisioning_state, PROVISIONING_SUCCEEDED);
    }

    #[tokio::test]
    async fn a_version_owned_by_someone_else_is_never_adopted() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        owned_version(
            &arm,
            &p.target.version,
            &src.sha,
            PROVISIONING_SUCCEEDED,
            OTHER_UID,
        )
        .await;
        let err = publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "ForeignResource");
    }

    #[tokio::test]
    async fn a_version_recording_another_digest_is_never_adopted() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        owned_version(
            &arm,
            &p.target.version,
            &"ee".repeat(32),
            PROVISIONING_SUCCEEDED,
            UID,
        )
        .await;
        let err = publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "ForeignResource");
    }

    #[tokio::test]
    async fn a_foreign_definition_is_not_published_into() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        arm.insert_gallery_image_definition(RG, GAL, DEF, BTreeMap::new(), BTreeMap::new());
        let err = publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "ForeignResource");
    }

    async fn precreate_disk(arm: &FakeArm, p: &PublishPlan, src: &Source) -> String {
        let name = upload_disk_name(GAL, DEF, &p.target.version);
        let layout = FixedVhdLayout::for_raw_len(src.bytes.len() as u64).unwrap();
        arm.put_upload_disk(
            RG,
            &name,
            &UploadDiskRequest {
                location: "eastus2".into(),
                upload_size_bytes: layout.total_len,
                tags: p.tags.to_map(),
                disk_encryption_set_id: None,
            },
        )
        .await
        .unwrap();
        name
    }

    #[tokio::test]
    async fn a_job_killed_mid_upload_grants_again_and_re_uploads() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        let name = precreate_disk(&arm, &p, &src).await;
        arm.set_disk_state(RG, &name, DISK_STATE_ACTIVE_UPLOAD);
        let blob = arm.upload_blob(RG, &name).unwrap();
        publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap();
        assert!(blob.writes() > 0);
        assert!(!arm.has_upload_disk(RG, &name), "deleted after success");
    }

    #[tokio::test]
    async fn a_finalised_disk_is_snapshotted_without_uploading_again() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        let name = precreate_disk(&arm, &p, &src).await;
        arm.set_disk_state(RG, &name, DISK_STATE_UNATTACHED);
        let blob = arm.upload_blob(RG, &name).unwrap();
        publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap();
        assert_eq!(blob.writes(), 0);
    }

    #[tokio::test]
    async fn an_upload_disk_of_another_size_is_not_reused() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        let name = upload_disk_name(GAL, DEF, &p.target.version);
        arm.put_upload_disk(
            RG,
            &name,
            &UploadDiskRequest {
                location: "eastus2".into(),
                upload_size_bytes: 8 * 1024 * 1024 + 512,
                tags: p.tags.to_map(),
                disk_encryption_set_id: None,
            },
        )
        .await
        .unwrap();
        let err = publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "UnexpectedState");
    }

    #[tokio::test]
    async fn an_existing_snapshot_is_reused() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        let snap = snapshot_name(GAL, DEF, &p.target.version);
        arm.insert_snapshot(RG, &snap, Some("SCSI, NVMe"), p.tags.to_map());
        let writers = Keeping::new(&arm, &p);
        publish(arm.as_ref(), &writers, &p, &timings())
            .await
            .unwrap();
        assert!(writers.blob().is_none(), "no disk, no upload");
        assert!(!arm.has_snapshot(RG, &snap), "deleted after success");
    }

    #[tokio::test]
    async fn a_sas_on_another_host_is_refused_and_nothing_is_written() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        arm.set_upload_host("md-x.blob.core.evil.example");
        let err = publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "UntrustedUploadHost");
        let blob = arm
            .upload_blob(RG, &upload_disk_name(GAL, DEF, &p.target.version))
            .unwrap();
        assert_eq!(blob.writes(), 0);
        assert!(!error_line(&err).contains("sig="));
    }

    #[tokio::test]
    async fn the_real_page_writer_refuses_the_fake_s_sas_on_the_wrong_cloud() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        // A US Government publisher must not send bytes to a public host.
        let writers = HttpPageWriterFactory::new(crate::cloud::US_GOV_UPLOAD_HOST_SUFFIX);
        let err = publish(arm.as_ref(), &writers, &p, &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "UntrustedUploadHost");
    }

    #[tokio::test]
    async fn a_version_that_never_finishes_fails_loudly_at_the_deadline() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        arm.hold_versions_creating(u32::MAX);
        let err = publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "Timeout");
    }

    #[tokio::test]
    async fn a_standard_publish_declares_nvme_but_not_trusted_launch() {
        let src = source();
        let mut p = plan(&src);
        p.trusted_launch = false;
        p.target.definition = "kairos".into();
        let arm = fake();
        publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap();
        let def = arm
            .get_gallery_image_definition(RG, GAL, "kairos")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(def.features["DiskControllerTypes"], "SCSI, NVMe");
        assert!(!def.features.contains_key("SecurityType"));
    }

    // ----- delete -----

    fn delete_plan(version: &str, uid: &str) -> DeletePlan {
        DeletePlan {
            target: Target {
                resource_group: RG.into(),
                gallery: GAL.into(),
                definition: DEF.into(),
                version: version.into(),
                location: "eastus2".into(),
            },
            owner_uid: uid.into(),
        }
    }

    #[tokio::test]
    async fn delete_removes_our_version_and_then_our_empty_definition() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap();
        unpublish(
            arm.as_ref(),
            &delete_plan(&p.target.version, UID),
            &timings(),
        )
        .await
        .unwrap();
        assert!(
            arm.get_gallery_image_version(RG, GAL, DEF, &p.target.version)
                .await
                .unwrap()
                .is_none()
        );
        assert!(!arm.has_gallery_image_definition(RG, GAL, DEF));
    }

    #[tokio::test]
    async fn delete_keeps_a_definition_that_has_other_versions() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap();
        owned_version(&arm, "1.0.0", &"aa".repeat(32), PROVISIONING_SUCCEEDED, UID).await;
        unpublish(
            arm.as_ref(),
            &delete_plan(&p.target.version, UID),
            &timings(),
        )
        .await
        .unwrap();
        assert!(arm.has_gallery_image_definition(RG, GAL, DEF));
    }

    #[tokio::test]
    async fn delete_refuses_a_version_owned_by_someone_else() {
        let arm = fake();
        owned_version(
            &arm,
            "1.2.3",
            &"aa".repeat(32),
            PROVISIONING_SUCCEEDED,
            OTHER_UID,
        )
        .await;
        let err = unpublish(arm.as_ref(), &delete_plan("1.2.3", UID), &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "ForeignResource");
        assert!(
            arm.get_gallery_image_version(RG, GAL, DEF, "1.2.3")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn delete_never_removes_a_definition_it_does_not_own() {
        let arm = fake();
        arm.insert_gallery_image_definition(RG, GAL, DEF, BTreeMap::new(), BTreeMap::new());
        unpublish(arm.as_ref(), &delete_plan("1.2.3", UID), &timings())
            .await
            .unwrap();
        assert!(arm.has_gallery_image_definition(RG, GAL, DEF));
    }

    #[tokio::test]
    async fn delete_of_nothing_is_success_and_clears_leftover_temporaries() {
        let src = source();
        let p = plan(&src);
        let arm = fake();
        let name = precreate_disk(&arm, &p, &src).await;
        arm.set_disk_state(RG, &name, DISK_STATE_ACTIVE_UPLOAD);
        unpublish(
            arm.as_ref(),
            &delete_plan(&p.target.version, UID),
            &timings(),
        )
        .await
        .unwrap();
        assert!(!arm.has_upload_disk(RG, &name));
    }

    // ----- termination log, flags -----

    #[test]
    fn the_published_line_has_the_documented_shape() {
        let line = published_line(&Published {
            version_id: "/v/1.2.3".into(),
            source_sha256: "ab".into(),
            min_os_disk_size_gib: 3,
        });
        assert!(!line.contains('\n'));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["versionId"], "/v/1.2.3");
        assert_eq!(v["sourceDigest"], "sha256:ab");
        assert_eq!(v["minOsDiskSizeGiB"], 3);
        assert_eq!(v.as_object().unwrap().len(), 3);
    }

    #[test]
    fn the_deleted_and_error_lines_have_the_documented_shape() {
        let v: serde_json::Value = serde_json::from_str(&deleted_line()).unwrap();
        assert_eq!(v, serde_json::json!({ "deleted": true }));
        let e = PublishError::ForeignResource("not ours".into());
        let v: serde_json::Value = serde_json::from_str(&error_line(&e)).unwrap();
        assert_eq!(v["error"], "ForeignResource");
        assert_eq!(v["message"], "not ours");
    }

    #[test]
    fn redaction_drops_a_signature_and_caps_the_length() {
        let r = redact("PUT https://h/x?sv=1&sig=SECRET failed");
        assert!(!r.contains("SECRET"));
        assert!(redact(&"x".repeat(10_000)).len() <= 1024);
    }

    #[test]
    fn checksum_must_be_sha256_hex() {
        let hex = "AB".repeat(32);
        assert_eq!(
            parse_checksum(&format!("sha256:{hex}")).unwrap(),
            hex.to_ascii_lowercase()
        );
        assert!(parse_checksum(&format!("sha512:{hex}")).is_err());
        assert!(parse_checksum("sha256:abc").is_err());
        assert!(parse_checksum(&format!("sha256:{}", "zz".repeat(32))).is_err());
    }

    #[test]
    fn the_footer_id_is_deterministic_and_a_v4_uuid() {
        let a = unique_id_for("ab");
        assert_eq!(a, unique_id_for("ab"));
        assert_ne!(a, unique_id_for("cd"));
        assert_eq!(a[6] & 0xf0, 0x40);
        assert_eq!(a[8] & 0xc0, 0x80);
    }

    #[test]
    fn azure_errors_map_to_stable_codes() {
        let e = PublishError::Azure(banlieue_azure::Error::UntrustedUploadHost("h".into()));
        assert_eq!(e.code(), "UntrustedUploadHost");
        let e = PublishError::Azure(banlieue_azure::Error::Forbidden {
            code: "AuthorizationFailed".into(),
            message: "m".into(),
        });
        assert_eq!(e.code(), "AuthorizationFailed");
    }

    #[derive(Parser)]
    struct Wrapper {
        #[command(flatten)]
        args: PublishArgs,
    }

    fn args(extra: &[&str]) -> PublishArgs {
        let mut argv = vec![
            "image-publish",
            "--endpoint",
            "https://management.azure.com",
            "--tenant-id",
            "t",
            "--client-id",
            "c",
            "--subscription-id",
            "s",
            "--location",
            "eastus2",
            "--gallery-resource-group",
            RG,
            "--gallery-name",
            GAL,
            "--image-definition",
            DEF,
            "--owner-uid",
            UID,
            "--provider",
            "banlieue-system.azure-eastus2",
        ];
        argv.extend_from_slice(extra);
        Wrapper::try_parse_from(argv).expect("flags parse").args
    }

    #[tokio::test]
    async fn execute_publishes_from_flags() {
        let src = source();
        let checksum = format!("sha256:{}", src.sha);
        let version = gallery_version_name(&src.sha);
        let a = args(&[
            "--mode",
            "publish",
            "--version",
            &version,
            "--source",
            src.path.to_str().unwrap(),
            "--checksum",
            &checksum,
            "--trusted-launch",
            "--owner-namespace",
            "tenant-a",
        ]);
        assert!(a.trusted_launch);
        let arm = fake();
        let line = execute(&a, arm.as_ref(), arm.as_ref(), &timings())
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["sourceDigest"], checksum);
        let version = arm
            .get_gallery_image_version(RG, GAL, DEF, &version)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(version.tags[TAG_OWNER_NAMESPACE], "tenant-a");
    }

    #[tokio::test]
    async fn execute_refuses_a_publish_without_source_or_checksum() {
        let arm = fake();
        let a = args(&["--mode", "publish", "--version", "1.2.3"]);
        let err = execute(&a, arm.as_ref(), arm.as_ref(), &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "InvalidArgument");
        assert_eq!(arm.publish_calls(), 0);
    }

    #[tokio::test]
    async fn execute_deletes_from_flags() {
        let arm = fake();
        let a = args(&["--mode", "delete", "--version", "1.2.3"]);
        let line = execute(&a, arm.as_ref(), arm.as_ref(), &timings())
            .await
            .unwrap();
        assert_eq!(line, deleted_line());
    }

    #[test]
    fn bad_names_and_providers_are_refused() {
        let mut a = args(&["--mode", "delete", "--version", "1.2"]);
        assert!(target_from(&a).is_err());
        a.version = "1.2.3".into();
        a.provider = "no-dot".into();
        assert!(target_from(&a).is_err());
        a.provider = "ns.p".into();
        a.image_definition = "-bad".into();
        assert!(target_from(&a).is_err());
    }

    #[test]
    fn the_token_file_and_termination_log_have_defaults() {
        let a = args(&["--mode", "delete", "--version", "1.2.3"]);
        assert_eq!(
            a.termination_log,
            std::path::Path::new(DEFAULT_TERMINATION_LOG)
        );
        if std::env::var_os("AZURE_FEDERATED_TOKEN_FILE").is_none() {
            assert_eq!(
                a.federated_token_file,
                std::path::Path::new(DEFAULT_FEDERATED_TOKEN_FILE)
            );
        }
    }

    #[tokio::test]
    async fn run_writes_the_error_line_and_fails_for_an_unknown_cloud() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("termination-log");
        let mut a = args(&["--mode", "delete", "--version", "1.2.3"]);
        a.endpoint = "https://management.azure.com.evil.example".into();
        a.termination_log = log.clone();
        assert!(run(a).await.is_err());
        let written = std::fs::read_to_string(&log).unwrap();
        assert_eq!(written.lines().count(), 1);
        let v: serde_json::Value = serde_json::from_str(written.trim()).unwrap();
        assert_eq!(v["error"], "InvalidArgument");
    }

    #[test]
    fn each_cloud_has_its_own_upload_domain() {
        use crate::cloud::{AZURE_CLOUDS, PUBLIC_UPLOAD_HOST_SUFFIX, upload_host_suffix};
        let suffixes: Vec<&str> = AZURE_CLOUDS.iter().map(upload_host_suffix).collect();
        assert_eq!(suffixes[0], PUBLIC_UPLOAD_HOST_SUFFIX);
        let mut unique = suffixes.clone();
        unique.dedup();
        assert_eq!(unique.len(), AZURE_CLOUDS.len());
    }

    // ----- ADR-0172: a policy-governed subscription -----

    const SUB: &str = "00000000-0000-0000-0000-000000000000";
    const DES: &str = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/rg-sec/providers/Microsoft.Compute/diskEncryptionSets/des-1";

    fn governed_plan(src: &Source) -> PublishPlan {
        PublishPlan {
            extra_tags: BTreeMap::from([("cost-centre".to_string(), "1234".to_string())]),
            disk_encryption_set_id: Some(DES.into()),
            ..plan(src)
        }
    }

    /// The definition and version carry the extra tag beside the ownership
    /// and digest tags, and the version is encrypted with the set.
    #[tokio::test]
    async fn a_governed_publish_tags_and_encrypts_what_it_creates() {
        let src = source();
        let p = governed_plan(&src);
        let arm = fake();
        arm.add_disk_encryption_set(DES, "eastus2");
        let writers = Keeping::new(&arm, &p);
        publish(arm.as_ref(), &writers, &p, &timings())
            .await
            .unwrap();
        let version = arm
            .get_gallery_image_version(RG, GAL, DEF, &p.target.version)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(version.tags["cost-centre"], "1234");
        assert_eq!(version.tags[TAG_SOURCE_SHA256], src.sha);
        assert_eq!(version.tags[TAG_OWNER_UID], UID);
        assert_eq!(
            arm.version_encryption_set(RG, GAL, DEF, &p.target.version)
                .as_deref(),
            Some(DES)
        );
        let definition = arm
            .get_gallery_image_definition(RG, GAL, DEF)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(definition.tags["cost-centre"], "1234");
    }

    /// The temporaries are encrypted too: seen while a version is still
    /// creating, before they are deleted.
    #[tokio::test]
    async fn a_governed_publish_encrypts_the_upload_disk_and_snapshot() {
        let src = source();
        let p = governed_plan(&src);
        let arm = fake();
        arm.add_disk_encryption_set(DES, "eastus2");
        arm.hold_versions_creating(u32::MAX);
        let err = publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "Timeout");
        let v = &p.target.version;
        assert_eq!(
            arm.upload_disk_encryption_set(RG, &upload_disk_name(GAL, DEF, v))
                .as_deref(),
            Some(DES)
        );
        assert_eq!(
            arm.snapshot_encryption_set(RG, &snapshot_name(GAL, DEF, v))
                .as_deref(),
            Some(DES)
        );
    }

    /// A set ARM does not know fails the publish at the first resource that
    /// names it, and the error is ARM's.
    #[tokio::test]
    async fn an_unknown_disk_encryption_set_fails_the_publish() {
        let src = source();
        let p = governed_plan(&src);
        let arm = fake();
        let err = publish(arm.as_ref(), arm.as_ref(), &p, &timings())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "InvalidParameter");
        assert!(!arm.has_upload_disk(RG, &upload_disk_name(GAL, DEF, &p.target.version)));
    }

    #[test]
    fn governance_flags_parse_into_tags_and_a_set() {
        let mut a = args(&[
            "--mode",
            "publish",
            "--version",
            "1.2.3",
            "--tags",
            r#"{"cost-centre":"1234","env":"prod"}"#,
            "--disk-encryption-set-id",
            DES,
        ]);
        a.subscription_id = SUB.into();
        let (tags, des) = governance_from(&a).expect("valid");
        assert_eq!(tags.len(), 2);
        assert_eq!(tags["env"], "prod");
        assert_eq!(des.as_deref(), Some(DES));

        let plain = args(&["--mode", "delete", "--version", "1.2.3"]);
        let (tags, des) = governance_from(&plain).expect("valid");
        assert!(tags.is_empty());
        assert!(des.is_none());
    }

    #[test]
    fn invalid_governance_flags_are_refused() {
        for (tags, des) in [
            (Some("not json"), None),
            (Some(r#"["a"]"#), None),
            (Some(r#"{"banlieue-source-sha256":"00"}"#), None),
            (Some(r#"{"Banlieue-Owner-Uid":"x"}"#), None),
            (Some(r#"{"a/b":"x"}"#), None),
            (None, Some("des-1")),
            (
                None,
                Some(
                    "/subscriptions/00000000-0000-0000-0000-0000000000ff/resourceGroups/rg-sec/providers/Microsoft.Compute/diskEncryptionSets/des-1",
                ),
            ),
        ] {
            let mut a = args(&["--mode", "publish", "--version", "1.2.3"]);
            a.subscription_id = SUB.into();
            a.tags = tags.map(ToString::to_string);
            a.disk_encryption_set_id = des.map(ToString::to_string);
            let err = governance_from(&a).expect_err(&format!("{tags:?} {des:?}"));
            assert_eq!(err.code(), "InvalidArgument", "{tags:?} {des:?}");
        }
    }

    #[tokio::test]
    async fn execute_publishes_with_governance_flags() {
        let src = source();
        let checksum = format!("sha256:{}", src.sha);
        let version = gallery_version_name(&src.sha);
        let mut a = args(&[
            "--mode",
            "publish",
            "--version",
            &version,
            "--source",
            src.path.to_str().unwrap(),
            "--checksum",
            &checksum,
            "--trusted-launch",
            "--tags",
            r#"{"cost-centre":"1234"}"#,
            "--disk-encryption-set-id",
            DES,
        ]);
        a.subscription_id = SUB.into();
        let arm = fake();
        arm.add_disk_encryption_set(DES, "eastus2");
        execute(&a, arm.as_ref(), arm.as_ref(), &timings())
            .await
            .unwrap();
        let v = arm
            .get_gallery_image_version(RG, GAL, DEF, &version)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(v.tags["cost-centre"], "1234");
        assert_eq!(
            arm.version_encryption_set(RG, GAL, DEF, &version)
                .as_deref(),
            Some(DES)
        );
    }
}
