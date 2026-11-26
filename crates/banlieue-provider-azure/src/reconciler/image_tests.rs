// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `VMImage` reconciler's row decision.

#[cfg(test)]
mod tests {
    use banlieue_api::banlieue::{
        AzureImageGallery, AzurePublisherIdentity, BuildArtifactKind, BuildArtifactPhase,
        BuildArtifactStatus, ProviderCapabilities, VMImageStatus,
    };
    use banlieue_api::common::FirstBootSealing;

    use crate::reconciler::gallery::{self, PublishDecision, VersionState};
    use crate::reconciler::publish_job::JobOutput;

    use super::super::*;
    use crate::test_support::{self, HOME, IMAGE_ID, PROVIDER};

    fn source(kind: ImageSourceKind, reference: &str) -> ImageSource {
        ImageSource {
            provider_class: PROVIDER_CLASS_NAME.into(),
            kind,
            reference: reference.into(),
            import_from: None,
            checksum: None,
        }
    }

    fn provider() -> Provider {
        test_support::provider(ProviderCapabilities::default())
    }

    /// Regional: one ready row with no zones, so every zone passes the
    /// scheduler's image check.
    #[test]
    fn a_gallery_version_is_ready_in_every_zone() {
        let row = row_for(&provider(), &source(ImageSourceKind::Template, IMAGE_ID));
        assert!(row.ready);
        assert!(row.zones.is_empty());
        assert_eq!(row.provider_name, PROVIDER);
        assert_eq!(row.provider_namespace, HOME);
        assert_eq!(row.resolved_ref.as_deref(), Some(IMAGE_ID));
        assert_eq!(row.reason.as_deref(), Some(reasons::RECONCILED));
    }

    #[test]
    fn an_id_is_matched_case_insensitively() {
        assert!(is_gallery_version_id(
            &IMAGE_ID
                .to_uppercase()
                .replace("/SUBSCRIPTIONS/", "/subscriptions/")
        ));
    }

    #[test]
    fn a_reference_that_is_not_a_version_id_is_invalid() {
        for bad in [
            "",
            "kairos-1.0.0",
            "/subscriptions/s/resourceGroups/rg/providers/Microsoft.Compute/galleries/g/images/i",
            "/subscriptions/s/resourceGroups/rg/providers/Microsoft.Compute/galleries/g/images/i/versions/",
            "/subscriptions/s/resourceGroups/rg/providers/Microsoft.Compute/images/i",
            "https://example.com/subscriptions/s/providers/Microsoft.Compute/galleries/g/images/i/versions/1",
        ] {
            assert!(!is_gallery_version_id(bad), "{bad}");
            let row = row_for(&provider(), &source(ImageSourceKind::Template, bad));
            assert!(!row.ready);
            assert_eq!(row.reason.as_deref(), Some(reasons::INVALID_REF));
        }
    }

    #[test]
    fn backing_file_and_oci_sources_are_unsupported() {
        for kind in [ImageSourceKind::BackingFile, ImageSourceKind::OciArtifact] {
            let row = row_for(&provider(), &source(kind, IMAGE_ID));
            assert!(!row.ready);
            assert_eq!(
                row.reason.as_deref(),
                Some(reasons::UNSUPPORTED_SOURCE_KIND)
            );
        }
    }

    #[test]
    fn the_azure_source_is_found_among_others() {
        let mut other = source(ImageSourceKind::Template, "9000");
        other.provider_class = "proxmox".into();
        let sources = vec![other, source(ImageSourceKind::Template, IMAGE_ID)];
        assert_eq!(
            find_azure_source(&sources).map(|s| s.reference.as_str()),
            Some(IMAGE_ID)
        );
        assert!(find_azure_source(&sources[..1]).is_none());
    }

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const OLD_ID: &str = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/rg-img/providers/Microsoft.Compute/galleries/banlieue/images/kairos/versions/0.9.0";

    fn publishing_provider() -> Provider {
        let mut p = provider();
        let azure = p.spec.connection.azure.as_mut().unwrap();
        azure.image_gallery = Some(AzureImageGallery {
            resource_group: "rg-img".into(),
            name: "banlieue".into(),
        });
        azure.publisher = Some(AzurePublisherIdentity {
            client_id: "00000000-0000-0000-0000-000000000003".into(),
        });
        p
    }

    fn vmimage(rows: Vec<ImagePerProviderStatus>) -> VMImage {
        let mut image: VMImage = serde_json::from_value(serde_json::json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "VMImage",
            "metadata": { "name": "kairos", "uid": "u" },
            "spec": {
                "osFamily": "linux",
                "osDistribution": "kairos",
                "osVersion": "4.4",
                "architecture": "amd64",
                "sources": [],
            },
        }))
        .unwrap();
        image.status = Some(VMImageStatus {
            per_provider: rows,
            ..Default::default()
        });
        image
    }

    fn published(digest: &str, min: Option<u32>) -> ImagePerProviderStatus {
        ImagePerProviderStatus {
            resolved_ref: Some(OLD_ID.into()),
            source_digest: Some(digest.into()),
            min_os_disk_size_gib: min,
            ..image_row(&provider(), true, reasons::RECONCILED, None)
        }
    }

    fn artifact() -> BuildArtifactStatus {
        BuildArtifactStatus {
            kind: BuildArtifactKind::CloudImage,
            phase: BuildArtifactPhase::Ready,
            os_artifact_ref: "a".into(),
            os_artifact_uid: None,
            pvc_ref: None,
            file: Some("a.raw".into()),
            reason: None,
            message: None,
            checksum: Some(format!("sha256:{HEX}")),
            oci_artifact: None,
            first_boot_sealing_layer: false,
            prelaid_disk_size_gi_b: None,
        }
    }

    #[test]
    fn a_provider_without_gallery_or_publisher_does_not_publish() {
        let (reason, _) = publish_target(&provider()).err().unwrap();
        assert_eq!(reason, gallery::reasons::GALLERY_NOT_CONFIGURED);
        let mut no_publisher = publishing_provider();
        no_publisher
            .spec
            .connection
            .azure
            .as_mut()
            .unwrap()
            .publisher = None;
        let (reason, _) = publish_target(&no_publisher).err().unwrap();
        assert_eq!(reason, gallery::reasons::PUBLISHER_NOT_CONFIGURED);
        assert!(publish_target(&publishing_provider()).is_ok());
    }

    #[test]
    fn first_boot_sealing_publishes_a_trusted_launch_definition() {
        let mut image = vmimage(Vec::new());
        assert!(!trusted_launch(&image));
        image.spec.first_boot_sealing =
            Some(serde_json::from_value::<FirstBootSealing>(serde_json::json!({})).unwrap());
        assert!(trusted_launch(&image));
    }

    #[test]
    fn the_existing_row_is_matched_by_name_and_namespace() {
        let mut elsewhere = published("sha256:x", None);
        elsewhere.provider_namespace = "elsewhere".into();
        let image = vmimage(vec![elsewhere]);
        assert!(existing_row(&image, &provider()).is_none());
        let image = vmimage(vec![published("sha256:x", None)]);
        assert!(existing_row(&image, &provider()).is_some());
    }

    /// A gate (rebuild in progress, failed read) must not forget a version
    /// banlieue published and will have to delete.
    #[test]
    fn a_holding_row_keeps_the_published_version_but_is_not_ready() {
        let existing = published("sha256:old", Some(64));
        let row = holding_row(&provider(), Some(&existing), "BuildPending", "m".into());
        assert!(!row.ready);
        assert_eq!(row.resolved_ref.as_deref(), Some(OLD_ID));
        assert_eq!(row.source_digest.as_deref(), Some("sha256:old"));
        assert_eq!(row.min_os_disk_size_gib, Some(64));
        // A Template row's ref is not banlieue's: not carried.
        let template = ImagePerProviderStatus {
            source_digest: None,
            ..existing
        };
        let row = holding_row(&provider(), Some(&template), "BuildPending", "m".into());
        assert!(row.resolved_ref.is_none());
    }

    #[test]
    fn a_published_row_carries_id_digest_and_disk_floor() {
        let a = artifact();
        let build = gallery::gate_build(Some(&a)).unwrap();
        let id = IMAGE_ID.to_string();
        let state = VersionState::Published { id: id.clone() };
        let decision = PublishDecision {
            action: gallery::JobAction::None,
            ready: true,
            reason: gallery::reasons::RECONCILED,
            message: None,
        };
        let output = JobOutput::Published {
            version_id: id.to_uppercase(),
            source_digest: build.digest(),
            min_os_disk_size_gib: Some(40),
        };
        let p = provider();
        let parts = PublishedRow {
            provider: &p,
            existing: None,
            build: &build,
            state: &state,
            decision: &decision,
            output: Some(&output),
            message: None,
            superseded: vec![OLD_ID.into()],
        };
        let row = published_row(&parts);
        assert!(row.ready);
        assert!(row.zones.is_empty());
        assert_eq!(row.resolved_ref.as_deref(), Some(IMAGE_ID));
        assert_eq!(row.source_digest, Some(format!("sha256:{HEX}")));
        assert_eq!(row.min_os_disk_size_gib, Some(40));
        assert_eq!(row.superseded_refs, vec![OLD_ID.to_string()]);

        // The Job is gone: the floor comes from the row for the same digest,
        // and never from a row for another digest.
        let same = published(&build.digest(), Some(48));
        let row = published_row(&PublishedRow {
            output: None,
            existing: Some(&same),
            ..parts
        });
        assert_eq!(row.min_os_disk_size_gib, Some(48));
    }

    #[test]
    fn a_floor_from_another_digest_is_not_carried() {
        let a = artifact();
        let build = gallery::gate_build(Some(&a)).unwrap();
        let state = VersionState::Published {
            id: IMAGE_ID.into(),
        };
        let decision = PublishDecision {
            action: gallery::JobAction::None,
            ready: true,
            reason: gallery::reasons::RECONCILED,
            message: None,
        };
        let other = published("sha256:old", Some(48));
        let p = provider();
        let row = published_row(&PublishedRow {
            provider: &p,
            existing: Some(&other),
            build: &build,
            state: &state,
            decision: &decision,
            output: None,
            message: None,
            superseded: Vec::new(),
        });
        assert_eq!(row.min_os_disk_size_gib, None);
    }

    #[test]
    fn a_row_not_yet_published_has_no_ref() {
        let a = artifact();
        let build = gallery::gate_build(Some(&a)).unwrap();
        let decision = PublishDecision {
            action: gallery::JobAction::Create,
            ready: false,
            reason: gallery::reasons::PUBLISHING,
            message: Some("publish Job created".into()),
        };
        let p = provider();
        let row = published_row(&PublishedRow {
            provider: &p,
            existing: None,
            build: &build,
            state: &VersionState::Missing,
            decision: &decision,
            output: None,
            message: decision.message.clone(),
            superseded: Vec::new(),
        });
        assert!(!row.ready);
        assert!(row.resolved_ref.is_none());
        assert_eq!(row.reason.as_deref(), Some(gallery::reasons::PUBLISHING));
    }

    #[test]
    fn merged_rows_replace_only_the_same_provider() {
        let mut other = published("sha256:x", None);
        other.provider_name = "other".into();
        let mine = image_row(&provider(), false, reasons::DELETING, None);
        let merged = merge_rows(vec![published("sha256:x", None), other.clone()], &[mine]);
        assert_eq!(merged[0].reason.as_deref(), Some(reasons::DELETING));
        assert_eq!(merged[1], other);
    }
}
