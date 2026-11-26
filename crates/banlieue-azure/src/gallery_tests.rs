// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `gallery.rs`.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::*;
    use crate::tags::OwnershipTags;

    const DIGEST: &str = "5b2917bdd4f419e0114cfd4830e5a8a784fff59eb0eafadc88dbab631f179df1";

    fn tags() -> BTreeMap<String, String> {
        OwnershipTags::new("uid", "VMImage", "ns", "p").to_map()
    }

    #[test]
    fn version_name_is_deterministic_and_valid() {
        let a = gallery_version_name(DIGEST);
        assert_eq!(a, gallery_version_name(DIGEST));
        assert!(is_valid_version_name(&a), "{a}");
        // 0x5b2917bd and 0xd4f419e0 masked to 31 bits.
        assert_eq!(
            a,
            format!("1.{}.{}", 0x5b29_17bd_u32, 0xd4f4_19e0_u32 & 0x7fff_ffff)
        );
    }

    #[test]
    fn different_digests_give_different_versions() {
        let other = "ff".repeat(32);
        assert_ne!(gallery_version_name(DIGEST), gallery_version_name(&other));
        assert!(is_valid_version_name(&gallery_version_name(&other)));
    }

    #[test]
    fn upper_case_hex_names_the_same_version() {
        assert_eq!(
            gallery_version_name(&DIGEST.to_ascii_uppercase()),
            gallery_version_name(DIGEST)
        );
    }

    #[test]
    fn a_non_hex_input_still_yields_a_valid_version() {
        for input in ["", "zz", "not-a-digest-at-all-but-long"] {
            assert!(
                is_valid_version_name(&gallery_version_name(input)),
                "{input}"
            );
        }
    }

    #[test]
    fn version_name_validation_matches_arm() {
        for ok in ["0.0.1", "1.2147483647.0", "1.2.3"] {
            assert!(is_valid_version_name(ok), "{ok}");
        }
        for bad in [
            "1.2",
            "1.2.3.4",
            "a.b.c",
            "1.2.-3",
            "1.2.2147483648",
            "1..3",
            "",
        ] {
            assert!(!is_valid_version_name(bad), "{bad}");
        }
    }

    #[test]
    fn definition_name_is_the_vmimage_name_with_a_security_suffix() {
        assert_eq!(
            image_definition_name("kairos-debian", false),
            "kairos-debian"
        );
        assert_eq!(
            image_definition_name("kairos-debian", true),
            "kairos-debian-tl"
        );
    }

    #[test]
    fn definition_name_is_always_valid_and_deterministic() {
        let long = "a".repeat(253);
        for (name, tl) in [
            ("kairos.debian-13", true),
            (long.as_str(), true),
            (long.as_str(), false),
            ("-weird_name.", false),
            ("ünïcode/name", true),
            ("", false),
        ] {
            let def = image_definition_name(name, tl);
            assert!(is_valid_image_definition_name(&def), "{name:?} -> {def:?}");
            assert_eq!(def, image_definition_name(name, tl));
            assert_eq!(def.ends_with(TRUSTED_LAUNCH_SUFFIX), tl, "{def}");
        }
    }

    #[test]
    fn long_names_sharing_a_prefix_stay_distinct() {
        let a = format!("{}-one", "x".repeat(100));
        let b = format!("{}-two", "x".repeat(100));
        assert_ne!(
            image_definition_name(&a, true),
            image_definition_name(&b, true)
        );
        assert!(image_definition_name(&a, true).len() <= MAX_IMAGE_DEFINITION_NAME_LEN);
    }

    #[test]
    fn offer_drops_the_suffix_and_fits() {
        assert_eq!(image_offer("kairos-tl", true), "kairos");
        assert_eq!(image_offer("kairos", false), "kairos");
        assert!(image_offer(&"b".repeat(80), false).len() <= MAX_OFFER_LEN);
    }

    #[test]
    fn temporaries_are_named_deterministically_and_apart() {
        let disk = upload_disk_name("gal", "def", "1.2.3");
        assert_eq!(disk, upload_disk_name("gal", "def", "1.2.3"));
        assert_ne!(disk, upload_disk_name("gal", "def", "1.2.4"));
        assert_ne!(disk, upload_disk_name("gal2", "def", "1.2.3"));
        assert_ne!(disk, snapshot_name("gal", "def", "1.2.3"));
        assert!(disk.starts_with("banlieue-publish-"));
    }

    #[test]
    fn definition_declares_nvme_always_and_trusted_launch_when_asked() {
        let mut req = ImageDefinitionRequest {
            location: "eastus2".into(),
            offer: "kairos".into(),
            trusted_launch: true,
            tags: tags(),
        };
        let body = req.to_body().unwrap();
        let p = &body["properties"];
        assert_eq!(p["hyperVGeneration"], "V2");
        assert_eq!(p["osState"], "Generalized");
        assert_eq!(p["identifier"]["publisher"], "banlieue");
        assert_eq!(p["identifier"]["sku"], "trustedlaunch");
        let features = p["features"].as_array().unwrap();
        assert!(
            features
                .iter()
                .any(|f| f["name"] == "DiskControllerTypes" && f["value"] == "SCSI, NVMe")
        );
        assert!(
            features
                .iter()
                .any(|f| f["name"] == "SecurityType" && f["value"] == "TrustedLaunch")
        );

        req.trusted_launch = false;
        let body = req.to_body().unwrap();
        let features = body["properties"]["features"].as_array().unwrap();
        assert_eq!(features.len(), 1);
        assert_eq!(body["properties"]["identifier"]["sku"], "standard");
    }

    #[test]
    fn upload_disk_declares_both_controllers_and_its_size() {
        let body = UploadDiskRequest {
            location: "eastus2".into(),
            upload_size_bytes: 2_504_000_000,
            tags: tags(),
            disk_encryption_set_id: None,
        }
        .to_body()
        .unwrap();
        let p = &body["properties"];
        assert_eq!(p["creationData"]["createOption"], "Upload");
        assert_eq!(p["creationData"]["uploadSizeBytes"], 2_504_000_000_u64);
        assert_eq!(
            p["supportedCapabilities"]["diskControllerTypes"],
            "SCSI, NVMe"
        );
        assert_eq!(p["hyperVGeneration"], "V2");
    }

    #[test]
    fn snapshot_is_a_full_copy_declaring_both_controllers() {
        let body = SnapshotRequest {
            location: "eastus2".into(),
            source_disk_id: "/x/disks/d".into(),
            tags: tags(),
            disk_encryption_set_id: None,
        }
        .to_body()
        .unwrap();
        let p = &body["properties"];
        assert_eq!(p["creationData"]["createOption"], "Copy");
        assert_eq!(p["creationData"]["sourceResourceId"], "/x/disks/d");
        assert_eq!(p["incremental"], false);
        assert_eq!(
            p["supportedCapabilities"]["diskControllerTypes"],
            "SCSI, NVMe"
        );
    }

    #[test]
    fn version_never_blocks_deletion_and_has_no_implicit_end_of_life() {
        let mut req = GalleryImageVersionRequest {
            location: "eastus2".into(),
            snapshot_id: "/x/snapshots/s".into(),
            end_of_life: None,
            tags: tags(),
            disk_encryption_set_id: None,
        };
        let body = req.to_body().unwrap();
        let p = &body["properties"];
        assert_eq!(p["safetyProfile"]["blockDeletionBeforeEndOfLife"], false);
        assert_eq!(
            p["safetyProfile"]["allowDeletionOfReplicatedLocations"],
            true
        );
        assert_eq!(
            p["storageProfile"]["osDiskImage"]["source"]["id"],
            "/x/snapshots/s"
        );
        assert!(p["publishingProfile"].get("endOfLifeDate").is_none());
        let region = &p["publishingProfile"]["targetRegions"][0];
        assert_eq!(region["name"], "eastus2");
        assert_eq!(region["regionalReplicaCount"], 1);

        req.end_of_life = Some("2027-12-31".into());
        let body = req.to_body().unwrap();
        assert_eq!(
            body["properties"]["publishingProfile"]["endOfLifeDate"],
            "2027-12-31"
        );
    }

    #[test]
    fn a_tag_azure_refuses_fails_every_request() {
        let mut bad = tags();
        bad.insert("banlieue.io/owner".into(), "x".into());
        assert!(
            UploadDiskRequest {
                location: "l".into(),
                upload_size_bytes: 1,
                tags: bad.clone(),
                disk_encryption_set_id: None,
            }
            .to_body()
            .is_err()
        );
        assert!(
            GalleryImageVersionRequest {
                location: "l".into(),
                snapshot_id: "s".into(),
                end_of_life: None,
                tags: bad,
                disk_encryption_set_id: None,
            }
            .to_body()
            .is_err()
        );
    }

    const DES: &str = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/rg-sec/providers/Microsoft.Compute/diskEncryptionSets/des-1";

    /// ADR-0172 Decision 2: every disk-like resource the publish Job
    /// creates is encrypted with the `Provider`'s customer-managed key.
    #[test]
    fn a_disk_encryption_set_encrypts_the_upload_disk_and_snapshot() {
        let expected = serde_json::json!({
            "type": "EncryptionAtRestWithCustomerKey",
            "diskEncryptionSetId": DES,
        });
        let disk = UploadDiskRequest {
            location: "eastus2".into(),
            upload_size_bytes: 2_504_000_000,
            tags: tags(),
            disk_encryption_set_id: Some(DES.into()),
        }
        .to_body()
        .unwrap();
        assert_eq!(disk["properties"]["encryption"], expected);
        let snapshot = SnapshotRequest {
            location: "eastus2".into(),
            source_disk_id: "/x/disks/d".into(),
            tags: tags(),
            disk_encryption_set_id: Some(DES.into()),
        }
        .to_body()
        .unwrap();
        assert_eq!(snapshot["properties"]["encryption"], expected);
    }

    #[test]
    fn without_a_disk_encryption_set_nothing_names_one() {
        let disk = UploadDiskRequest {
            location: "eastus2".into(),
            upload_size_bytes: 2_504_000_000,
            tags: tags(),
            disk_encryption_set_id: None,
        }
        .to_body()
        .unwrap();
        assert!(disk["properties"].get("encryption").is_none());
        let version = GalleryImageVersionRequest {
            location: "eastus2".into(),
            snapshot_id: "/x/snapshots/s".into(),
            end_of_life: None,
            tags: tags(),
            disk_encryption_set_id: None,
        }
        .to_body()
        .unwrap();
        assert!(
            version["properties"]["publishingProfile"]["targetRegions"][0]
                .get("encryption")
                .is_none()
        );
    }

    #[test]
    fn a_disk_encryption_set_encrypts_the_version_in_its_region() {
        let body = GalleryImageVersionRequest {
            location: "eastus2".into(),
            snapshot_id: "/x/snapshots/s".into(),
            end_of_life: None,
            tags: tags(),
            disk_encryption_set_id: Some(DES.into()),
        }
        .to_body()
        .unwrap();
        assert_eq!(
            body["properties"]["publishingProfile"]["targetRegions"][0]["encryption"],
            serde_json::json!({"osDiskImage": {"diskEncryptionSetId": DES}})
        );
    }

    #[test]
    fn a_malformed_disk_encryption_set_fails_every_request() {
        let bad = Some("not-an-id".to_string());
        assert!(
            UploadDiskRequest {
                location: "l".into(),
                upload_size_bytes: 1,
                tags: tags(),
                disk_encryption_set_id: bad.clone(),
            }
            .to_body()
            .is_err()
        );
        assert!(
            SnapshotRequest {
                location: "l".into(),
                source_disk_id: "/x/disks/d".into(),
                tags: tags(),
                disk_encryption_set_id: bad.clone(),
            }
            .to_body()
            .is_err()
        );
        assert!(
            GalleryImageVersionRequest {
                location: "l".into(),
                snapshot_id: "s".into(),
                end_of_life: None,
                tags: tags(),
                disk_encryption_set_id: bad,
            }
            .to_body()
            .is_err()
        );
    }

    #[test]
    fn parses_a_version_a_disk_a_snapshot_and_a_definition() {
        let v = parse_gallery_image_version(
            br#"{"id":"/v/1.2.3","tags":{"a":"b"},"properties":{"provisioningState":"Creating"}}"#,
        )
        .unwrap();
        assert_eq!(v.provisioning_state, "Creating");
        assert_eq!(v.tags["a"], "b");

        let d = parse_managed_disk(
            br#"{"id":"/d","properties":{"diskState":"ReadyToUpload","creationData":{"createOption":"Upload","uploadSizeBytes":2097664},"supportedCapabilities":{"diskControllerTypes":"SCSI, NVMe"}}}"#,
        )
        .unwrap();
        assert_eq!(d.disk_state, DISK_STATE_READY_TO_UPLOAD);
        assert_eq!(d.upload_size_bytes, Some(2_097_664));
        assert!(
            d.disk_controller_types
                .as_deref()
                .is_some_and(declares_nvme)
        );
        assert!(d.tags.is_empty());

        let s = parse_snapshot(br#"{"id":"/s","properties":{"provisioningState":"Succeeded"}}"#)
            .unwrap();
        assert_eq!(s.provisioning_state, PROVISIONING_SUCCEEDED);

        let def = parse_gallery_image_definition(
            br#"{"id":"/i","properties":{"features":[{"name":"DiskControllerTypes","value":"SCSI, NVMe"}]}}"#,
        )
        .unwrap();
        assert_eq!(
            def.features[FEATURE_DISK_CONTROLLER_TYPES],
            DISK_CONTROLLER_TYPES
        );

        let list = parse_gallery_image_versions(
            br#"{"value":[{"id":"/v/1","properties":{"provisioningState":"Succeeded"}}]}"#,
        )
        .unwrap();
        assert_eq!(list.len(), 1);
        assert!(parse_gallery_image_version(b"{}").is_err());
    }

    #[test]
    fn access_sas_is_read_from_either_shape_and_never_echoed() {
        assert_eq!(
            parse_access_sas(br#"{"accessSAS":"https://h/x?sig=1"}"#).unwrap(),
            "https://h/x?sig=1"
        );
        assert_eq!(
            parse_access_sas(br#"{"status":"Succeeded","properties":{"output":{"accessSAS":"https://h/y?sig=2"}}}"#)
                .unwrap(),
            "https://h/y?sig=2"
        );
        let err = parse_access_sas(br#"{"accessSas":"https://h/z?sig=secret"}"#).unwrap_err();
        assert!(!err.to_string().contains("secret"));
    }

    #[test]
    fn nvme_detection() {
        assert!(declares_nvme("SCSI, NVMe"));
        assert!(declares_nvme("NVMe"));
        assert!(!declares_nvme("SCSI"));
        assert!(!declares_nvme(""));
    }
}
