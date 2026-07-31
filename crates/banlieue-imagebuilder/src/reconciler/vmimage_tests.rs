// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `vmimage.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{Architecture, ImageSource, ImageSourceKind};
    use banlieue_provider_sdk::scheduling::BuildScheduling;

    fn url_source(import_from: &str) -> ImageSource {
        ImageSource {
            provider_class: "vsphere".to_string(),
            kind: ImageSourceKind::Url,
            reference: "ignored".to_string(),
            import_from: Some(import_from.to_string()),
            checksum: None,
        }
    }

    fn template_source() -> ImageSource {
        ImageSource {
            provider_class: "vsphere".to_string(),
            kind: ImageSourceKind::Template,
            reference: "ubuntu-22.04-cloudinit".to_string(),
            import_from: None,
            checksum: None,
        }
    }

    // ----------------------------------------------------------------------
    // find_url_source
    // ----------------------------------------------------------------------

    #[test]
    fn find_url_source_returns_the_url_entry() {
        let sources = vec![
            template_source(),
            url_source("quay.io/kairos/ubuntu:24.04-core-amd64-generic-v3.7.2"),
        ];
        let found = find_url_source(&sources).expect("expected a Url source");
        assert_eq!(found.kind, ImageSourceKind::Url);
    }

    #[test]
    fn find_url_source_none_when_only_template_sources() {
        let sources = vec![template_source()];
        assert!(find_url_source(&sources).is_none());
    }

    #[test]
    fn find_url_source_ignores_url_without_import_from() {
        let sources = vec![ImageSource {
            provider_class: "vsphere".to_string(),
            kind: ImageSourceKind::Url,
            reference: "ignored".to_string(),
            import_from: None,
            checksum: None,
        }];
        assert!(find_url_source(&sources).is_none());
    }

    #[test]
    fn find_url_source_empty_sources_is_none() {
        assert!(find_url_source(&[]).is_none());
    }

    // ----------------------------------------------------------------------
    // os_artifact_name / os_artifact_api_resource
    // ----------------------------------------------------------------------

    #[test]
    fn os_artifact_name_appends_suffix() {
        assert_eq!(
            os_artifact_name("kairos-ubuntu-2404"),
            "kairos-ubuntu-2404-build"
        );
    }

    #[test]
    fn os_artifact_api_resource_matches_kairos_operator_gvk() {
        let ar = os_artifact_api_resource();
        assert_eq!(ar.group, OSARTIFACT_GROUP);
        assert_eq!(ar.version, OSARTIFACT_VERSION);
        assert_eq!(ar.kind, OSARTIFACT_KIND);
        assert_eq!(ar.plural, OSARTIFACT_PLURAL);
    }

    // ----------------------------------------------------------------------
    // desired_os_artifact
    // ----------------------------------------------------------------------

    #[test]
    fn desired_os_artifact_requests_only_cloud_image() {
        let source = url_source("quay.io/kairos/ubuntu:24.04-core-amd64-generic-v3.7.2");
        let obj = desired_os_artifact(
            "kairos-ubuntu-2404-build",
            "banlieue-system",
            &source,
            &Architecture::Amd64,
            None,
            &BuildScheduling::default(),
        );
        assert_eq!(obj["apiVersion"], "build.kairos.io/v1alpha2");
        assert_eq!(obj["kind"], "OSArtifact");
        assert_eq!(obj["metadata"]["name"], "kairos-ubuntu-2404-build");
        assert_eq!(obj["metadata"]["namespace"], "banlieue-system");
        assert_eq!(
            obj["spec"]["image"]["ref"],
            "quay.io/kairos/ubuntu:24.04-core-amd64-generic-v3.7.2"
        );
        assert_eq!(obj["spec"]["artifacts"]["cloudImage"], true);
        assert_eq!(obj["spec"]["artifacts"]["arch"], "amd64");
        // Never request other artifact kinds — only the raw disk is consumed.
        assert!(obj["spec"]["artifacts"]["iso"].is_null());
        assert!(obj["spec"]["artifacts"]["azureImage"].is_null());
    }

    #[test]
    fn desired_os_artifact_arm64() {
        let source = url_source("quay.io/kairos/ubuntu:24.04-arm64");
        let obj = desired_os_artifact(
            "x-build",
            "ns",
            &source,
            &Architecture::Arm64,
            None,
            &BuildScheduling::default(),
        );
        assert_eq!(obj["spec"]["artifacts"]["arch"], "arm64");
    }

    // ----------------------------------------------------------------------
    // extract_kairos_status
    // ----------------------------------------------------------------------

    #[test]
    fn extract_kairos_status_reads_phase_and_message() {
        let data = serde_json::json!({
            "status": { "phase": "Building", "message": "pulling image" }
        });
        let view = extract_kairos_status(&data);
        assert_eq!(view.phase.as_deref(), Some("Building"));
        assert_eq!(view.message.as_deref(), Some("pulling image"));
    }

    #[test]
    fn extract_kairos_status_missing_status_is_empty_view() {
        let data = serde_json::json!({});
        let view = extract_kairos_status(&data);
        assert!(view.phase.is_none());
        assert!(view.message.is_none());
    }

    // ----------------------------------------------------------------------
    // map_kairos_phase
    // ----------------------------------------------------------------------

    #[test]
    fn map_kairos_phase_pending() {
        assert_eq!(map_kairos_phase("Pending"), RawDiskArtifactPhase::Pending);
    }

    #[test]
    fn map_kairos_phase_building_and_exporting_both_map_to_building() {
        assert_eq!(map_kairos_phase("Building"), RawDiskArtifactPhase::Building);
        assert_eq!(
            map_kairos_phase("Exporting"),
            RawDiskArtifactPhase::Building
        );
    }

    #[test]
    fn map_kairos_phase_ready() {
        assert_eq!(map_kairos_phase("Ready"), RawDiskArtifactPhase::Ready);
    }

    #[test]
    fn map_kairos_phase_error_fails_closed() {
        assert_eq!(map_kairos_phase("Error"), RawDiskArtifactPhase::Failed);
    }

    #[test]
    fn map_kairos_phase_unrecognized_fails_closed() {
        // An unknown phase string from a future kairos-operator release must
        // never be silently treated as progress.
        assert_eq!(
            map_kairos_phase("SomeFuturePhase"),
            RawDiskArtifactPhase::Failed
        );
    }

    // ----------------------------------------------------------------------
    // compute_raw_disk_artifact_status
    // ----------------------------------------------------------------------

    #[test]
    fn compute_status_missing_phase_defaults_pending() {
        let view = KairosArtifactStatusView::default();
        let s = compute_raw_disk_artifact_status("x-build", &view, None);
        assert_eq!(s.phase, RawDiskArtifactPhase::Pending);
        assert_eq!(s.os_artifact_ref, "x-build");
        assert!(s.pvc_ref.is_none());
        assert!(s.disk_file.is_none());
    }

    #[test]
    fn compute_status_building_has_no_pvc_yet() {
        let view = KairosArtifactStatusView {
            phase: Some("Building".to_string()),
            message: Some("pulling image".to_string()),
        };
        let s = compute_raw_disk_artifact_status("x-build", &view, None);
        assert_eq!(s.phase, RawDiskArtifactPhase::Building);
        assert!(s.pvc_ref.is_none());
        assert!(s.disk_file.is_none());
        assert_eq!(s.message.as_deref(), Some("pulling image"));
    }

    #[test]
    fn compute_status_ready_populates_pvc_and_disk_file_by_kairos_convention() {
        let view = KairosArtifactStatusView {
            phase: Some("Ready".to_string()),
            message: None,
        };
        let s = compute_raw_disk_artifact_status("kairos-ubuntu-2404-build", &view, None);
        assert_eq!(s.phase, RawDiskArtifactPhase::Ready);
        assert_eq!(
            s.pvc_ref.unwrap().name,
            "kairos-ubuntu-2404-build-artifacts"
        );
        assert_eq!(s.disk_file.unwrap(), "kairos-ubuntu-2404-build.raw");
    }

    #[test]
    fn compute_status_failed_carries_message_no_pvc() {
        let view = KairosArtifactStatusView {
            phase: Some("Error".to_string()),
            message: Some("pull failed: manifest unknown".to_string()),
        };
        let s = compute_raw_disk_artifact_status("x-build", &view, None);
        assert_eq!(s.phase, RawDiskArtifactPhase::Failed);
        assert!(s.pvc_ref.is_none());
        assert_eq!(s.message.as_deref(), Some("pull failed: manifest unknown"));
    }

    #[test]
    fn compute_status_reason_is_stable_per_phase() {
        for (phase_str, expected_reason) in [
            ("Pending", reasons::PENDING),
            ("Building", reasons::BUILDING),
            ("Ready", reasons::READY),
            ("Error", reasons::FAILED),
        ] {
            let view = KairosArtifactStatusView {
                phase: Some(phase_str.to_string()),
                message: None,
            };
            let s = compute_raw_disk_artifact_status("x-build", &view, None);
            assert_eq!(s.reason.as_deref(), Some(expected_reason));
        }
    }

    // ----------------------------------------------------------------------
    // SEC-005: owner reference + staleness binding
    // ----------------------------------------------------------------------

    #[test]
    fn desired_os_artifact_carries_the_vmimage_owner_reference() {
        let source = url_source("quay.io/kairos/ubuntu:24.04-core-amd64-generic-v3.7.2");
        let obj = desired_os_artifact(
            "kairos-ubuntu-2404-build",
            "banlieue-system",
            &source,
            &Architecture::Amd64,
            Some(OwnerRef {
                name: "kairos-ubuntu-2404",
                uid: "9f2b1c7e-1234-4cde-9abc-def012345678",
            }),
            &BuildScheduling::default(),
        );
        let owner = &obj["metadata"]["ownerReferences"][0];
        assert_eq!(owner["apiVersion"], "banlieue.io/v1alpha1");
        assert_eq!(owner["kind"], "VMImage");
        assert_eq!(owner["name"], "kairos-ubuntu-2404");
        assert_eq!(owner["uid"], "9f2b1c7e-1234-4cde-9abc-def012345678");
        assert_eq!(owner["controller"], true);
        // No blockOwnerDeletion: it would require finalizers RBAC this
        // controller does not otherwise need.
        assert_eq!(owner["blockOwnerDeletion"], false);
    }

    #[test]
    fn owner_uid_matches_only_the_exact_uid() {
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
        let refs = vec![OwnerReference {
            uid: "aaaa".to_string(),
            ..Default::default()
        }];
        assert!(owner_uid_matches(Some(&refs), "aaaa"));
        // Same name, new UID — a deleted-and-recreated VMImage must NOT match.
        assert!(!owner_uid_matches(Some(&refs), "bbbb"));
        assert!(!owner_uid_matches(None, "aaaa"));
        assert!(!owner_uid_matches(Some(&[]), "aaaa"));
    }

    #[test]
    fn spec_matches_requires_ref_and_arch() {
        let data = serde_json::json!({
            "spec": { "image": { "ref": "quay.io/a/b@sha256:x" }, "artifacts": { "arch": "amd64" } }
        });
        assert!(spec_matches(&data, "quay.io/a/b@sha256:x", "amd64"));
        assert!(!spec_matches(&data, "quay.io/a/c@sha256:y", "amd64"));
        assert!(!spec_matches(&data, "quay.io/a/b@sha256:x", "arm64"));
    }

    // ----------------------------------------------------------------------
    // SEC-004: checksum threading
    // ----------------------------------------------------------------------

    #[test]
    fn compute_status_threads_the_source_checksum_through() {
        let view = KairosArtifactStatusView {
            phase: Some("Ready".to_string()),
            message: None,
        };
        let s = compute_raw_disk_artifact_status(
            "x-build",
            &view,
            Some("sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"),
        );
        assert_eq!(
            s.checksum.as_deref(),
            Some("sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08")
        );
    }

    #[test]
    fn compute_status_without_a_source_checksum_publishes_none() {
        let view = KairosArtifactStatusView::default();
        let s = compute_raw_disk_artifact_status("x-build", &view, None);
        assert!(s.checksum.is_none());
    }
}
