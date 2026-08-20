// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `vmimage.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{Architecture, BuildArtifactKind, ImageSource, ImageSourceKind};
    use banlieue_api::common::{CloudConfigSource, KeySelector};
    use banlieue_provider_sdk::scheduling::BuildScheduling;

    /// A vSphere `Url` source (→ `iso` artifact).
    fn url_source(import_from: &str) -> ImageSource {
        ImageSource {
            provider_class: "vsphere".to_string(),
            kind: ImageSourceKind::Url,
            reference: "ignored".to_string(),
            import_from: Some(import_from.to_string()),
            checksum: None,
        }
    }

    /// A libvirt `Url` source (→ `cloudImage` artifact).
    fn libvirt_url_source(import_from: &str) -> ImageSource {
        ImageSource {
            provider_class: "libvirt".to_string(),
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
    // artifact_kind_for_class (ADR-0020)
    // ----------------------------------------------------------------------

    #[test]
    fn artifact_kind_vsphere_is_iso_others_are_cloud_image() {
        assert_eq!(artifact_kind_for_class("vsphere"), BuildArtifactKind::Iso);
        assert_eq!(
            artifact_kind_for_class("libvirt"),
            BuildArtifactKind::CloudImage
        );
        assert_eq!(
            artifact_kind_for_class("proxmox"),
            BuildArtifactKind::CloudImage
        );
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
    fn desired_os_artifact_requests_iso_for_vsphere() {
        let source = url_source("quay.io/kairos/rhel:9.8-core-amd64-generic-v3.7.2");
        let obj = desired_os_artifact(
            "kairos-rhel98-build",
            "banlieue-imagebuild",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::Iso,
            None,
            None,
            &BuildScheduling::default(),
            None,
            &ImporterImage::default(),
        );
        assert_eq!(obj["apiVersion"], "build.kairos.io/v1alpha2");
        assert_eq!(obj["kind"], "OSArtifact");
        assert_eq!(obj["metadata"]["name"], "kairos-rhel98-build");
        assert_eq!(obj["metadata"]["namespace"], "banlieue-imagebuild");
        assert_eq!(
            obj["spec"]["image"]["ref"],
            "quay.io/kairos/rhel:9.8-core-amd64-generic-v3.7.2"
        );
        assert_eq!(obj["spec"]["artifacts"]["iso"], true);
        assert_eq!(obj["spec"]["artifacts"]["arch"], "amd64");
        // Only the ISO is requested — never the raw cloud image or others.
        assert!(obj["spec"]["artifacts"]["cloudImage"].is_null());
        assert!(obj["spec"]["artifacts"]["azureImage"].is_null());
        // No cloud-config source → cloudConfigRef omitted.
        assert!(
            !obj["spec"]["artifacts"]
                .as_object()
                .unwrap()
                .contains_key("cloudConfigRef")
        );
    }

    #[test]
    fn desired_os_artifact_requests_cloud_image_for_libvirt() {
        let source = libvirt_url_source("quay.io/kairos/ubuntu:24.04-core-amd64-generic-v3.7.2");
        let obj = desired_os_artifact(
            "kairos-ubuntu-2404-build",
            "banlieue-imagebuild",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::CloudImage,
            None,
            None,
            &BuildScheduling::default(),
            None,
            &ImporterImage::default(),
        );
        assert_eq!(obj["spec"]["artifacts"]["cloudImage"], true);
        assert_eq!(obj["spec"]["artifacts"]["arch"], "amd64");
        assert!(obj["spec"]["artifacts"]["iso"].is_null());
    }

    #[test]
    fn desired_os_artifact_passes_cloud_config_secret_ref() {
        let source = url_source("quay.io/kairos/rhel:9.8");
        let cc = CloudConfigSource {
            secret_ref: Some(KeySelector {
                name: "kairos-base-cloud-config".to_string(),
                key: None,
            }),
        };
        let obj = desired_os_artifact(
            "kairos-rhel98-build",
            "banlieue-imagebuild",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::Iso,
            Some(&cc),
            None,
            &BuildScheduling::default(),
            None,
            &ImporterImage::default(),
        );
        assert_eq!(
            obj["spec"]["artifacts"]["cloudConfigRef"]["name"],
            "kairos-base-cloud-config"
        );
        // Key defaults to the kairos convention when omitted.
        assert_eq!(
            obj["spec"]["artifacts"]["cloudConfigRef"]["key"],
            "cloud-config.yaml"
        );
    }

    #[test]
    fn desired_os_artifact_honours_explicit_cloud_config_key() {
        let source = url_source("quay.io/kairos/rhel:9.8");
        let cc = CloudConfigSource {
            secret_ref: Some(KeySelector {
                name: "cc".to_string(),
                key: Some("90_base.yaml".to_string()),
            }),
        };
        let obj = desired_os_artifact(
            "x-build",
            "ns",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::Iso,
            Some(&cc),
            None,
            &BuildScheduling::default(),
            None,
            &ImporterImage::default(),
        );
        assert_eq!(
            obj["spec"]["artifacts"]["cloudConfigRef"]["key"],
            "90_base.yaml"
        );
    }

    #[test]
    fn desired_os_artifact_omits_empty_scheduling_keys() {
        // Regression: the OSArtifact CRD types nodeSelector as object and
        // tolerations as array; a literal `null` is rejected with a 422. With
        // no scheduling configured the keys must be ABSENT, not null — and
        // `.is_null()` cannot tell the two apart, so assert on the spec map's
        // keys directly.
        let source = url_source("registry.example/img:v1");
        let obj = desired_os_artifact(
            "img-build",
            "banlieue-imagebuild",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::Iso,
            None,
            None,
            &BuildScheduling::default(),
            None,
            &ImporterImage::default(),
        );
        let spec = obj["spec"].as_object().expect("spec is an object");
        assert!(
            !spec.contains_key("nodeSelector"),
            "empty nodeSelector must be omitted, not null"
        );
        assert!(
            !spec.contains_key("tolerations"),
            "empty tolerations must be omitted, not null"
        );
        // No owner → ownerReferences omitted too (same CRD/type rule).
        assert!(
            !obj["metadata"]
                .as_object()
                .unwrap()
                .contains_key("ownerReferences"),
        );
    }

    #[test]
    fn desired_os_artifact_includes_scheduling_when_set() {
        let source = url_source("registry.example/img:v1");
        let scheduling = BuildScheduling {
            node_selector: [("banlieue.io/imagebuild".to_string(), "true".to_string())]
                .into_iter()
                .collect(),
            tolerations: Vec::new(),
        };
        let obj = desired_os_artifact(
            "img-build",
            "banlieue-imagebuild",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::Iso,
            None,
            None,
            &scheduling,
            None,
            &ImporterImage::default(),
        );
        assert_eq!(
            obj["spec"]["nodeSelector"]["banlieue.io/imagebuild"], "true",
            "configured nodeSelector must be present as an object"
        );
        // tolerations still empty → still omitted.
        assert!(!obj["spec"].as_object().unwrap().contains_key("tolerations"));
    }

    #[test]
    fn desired_os_artifact_arm64() {
        let source = url_source("quay.io/kairos/ubuntu:24.04-arm64");
        let obj = desired_os_artifact(
            "x-build",
            "ns",
            &source,
            &Architecture::Arm64,
            &BuildArtifactKind::Iso,
            None,
            None,
            &BuildScheduling::default(),
            None,
            &ImporterImage::default(),
        );
        assert_eq!(obj["spec"]["artifacts"]["arch"], "arm64");
    }

    // ----------------------------------------------------------------------
    // desired_os_artifact — isoOverlay (ADR-0022)
    // ----------------------------------------------------------------------

    #[test]
    fn desired_os_artifact_wires_iso_overlay_volume() {
        use banlieue_api::banlieue::{IsoOverlayFile, IsoOverlaySource};
        use banlieue_api::common::LocalObjectReference;

        let source = url_source("quay.io/kairos/rhel:9.8");
        let overlay = IsoOverlaySource {
            secret_ref: LocalObjectReference {
                name: "kairos-iso-overlay".to_string(),
            },
            files: vec![IsoOverlayFile {
                key: "grub.cfg".to_string(),
                path: "boot/grub2/grub.cfg".to_string(),
            }],
        };
        let obj = desired_os_artifact(
            "kairos-rhel98-build",
            "banlieue-imagebuild",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::Iso,
            None,
            None,
            &BuildScheduling::default(),
            Some(&overlay),
            &ImporterImage::default(),
        );
        // overlayISOVolume must point at the emptyDir, never the raw Secret
        // volume directly — kairos-io/kairos#4324: auroraboot's overlay-copy
        // step corrupts the ISO's real /boot when the overlay dir contains
        // kubelet's Secret-mount symlinks, so the Secret is first dereferenced
        // into a plain emptyDir by an importer (see below).
        assert_eq!(obj["spec"]["artifacts"]["overlayISOVolume"], "iso-overlay");
        assert_eq!(obj["spec"]["volumes"][0]["name"], "iso-overlay-source");
        assert_eq!(
            obj["spec"]["volumes"][0]["secret"]["secretName"],
            "kairos-iso-overlay"
        );
        assert_eq!(
            obj["spec"]["volumes"][0]["secret"]["items"][0]["key"],
            "grub.cfg"
        );
        assert_eq!(
            obj["spec"]["volumes"][0]["secret"]["items"][0]["path"],
            "boot/grub2/grub.cfg"
        );
        assert_eq!(obj["spec"]["volumes"][1]["name"], "iso-overlay");
        assert!(obj["spec"]["volumes"][1]["emptyDir"].is_object());
    }

    #[test]
    fn desired_os_artifact_wires_iso_overlay_materialize_importer() {
        use banlieue_api::banlieue::{IsoOverlayFile, IsoOverlaySource};
        use banlieue_api::common::LocalObjectReference;

        let source = url_source("quay.io/kairos/rhel:9.8");
        let overlay = IsoOverlaySource {
            secret_ref: LocalObjectReference {
                name: "kairos-iso-overlay".to_string(),
            },
            files: vec![IsoOverlayFile {
                key: "grub.cfg".to_string(),
                path: "boot/grub2/grub.cfg".to_string(),
            }],
        };
        let obj = desired_os_artifact(
            "kairos-rhel98-build",
            "banlieue-imagebuild",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::Iso,
            None,
            None,
            &BuildScheduling::default(),
            Some(&overlay),
            &ImporterImage::default(),
        );
        let importer = &obj["spec"]["importers"][0];
        assert_eq!(importer["name"], "iso-overlay-materialize");
        let mounts = importer["volumeMounts"].as_array().unwrap();
        assert_eq!(mounts[0]["name"], "iso-overlay-source");
        assert_eq!(mounts[0]["mountPath"], "/overlay-src");
        assert_eq!(mounts[0]["readOnly"], true);
        assert_eq!(mounts[1]["name"], "iso-overlay");
        assert_eq!(mounts[1]["mountPath"], "/overlay-dst");
        // Dereferences symlinks (-L) and skips kubelet's `..data`/`..<timestamp>`
        // bookkeeping entries (`-not -name '.*'`) so only the declared overlay
        // files land in the emptyDir.
        let command = importer["command"].as_array().unwrap();
        let script = command.last().unwrap().as_str().unwrap();
        assert!(script.contains("-not -name '.*'"));
        assert!(script.contains("cp -rL"));
        assert!(script.contains("/overlay-src"));
        assert!(script.contains("/overlay-dst"));
    }

    #[test]
    fn desired_os_artifact_uses_the_configured_importer_image() {
        use banlieue_api::banlieue::{IsoOverlayFile, IsoOverlaySource};
        use banlieue_api::common::LocalObjectReference;

        let source = url_source("quay.io/kairos/rhel:9.8");
        let overlay = IsoOverlaySource {
            secret_ref: LocalObjectReference {
                name: "kairos-iso-overlay".to_string(),
            },
            files: vec![IsoOverlayFile {
                key: "grub.cfg".to_string(),
                path: "boot/grub2/grub.cfg".to_string(),
            }],
        };
        let importer_image =
            ImporterImage::from_flags("mirror.internal/library/busybox:1.36@sha256:abc123", &[]);
        let obj = desired_os_artifact(
            "kairos-rhel98-build",
            "banlieue-imagebuild",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::Iso,
            None,
            None,
            &BuildScheduling::default(),
            Some(&overlay),
            &importer_image,
        );
        assert_eq!(
            obj["spec"]["importers"][0]["image"],
            "mirror.internal/library/busybox:1.36@sha256:abc123"
        );
    }

    #[test]
    fn desired_os_artifact_sets_image_pull_secrets_when_configured() {
        let source = url_source("quay.io/kairos/rhel:9.8");
        let importer_image = ImporterImage::from_flags(
            ISO_OVERLAY_IMPORTER_IMAGE,
            &["mirror-pull-secret".to_string(), "other-secret".to_string()],
        );
        let obj = desired_os_artifact(
            "kairos-rhel98-build",
            "banlieue-imagebuild",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::Iso,
            None,
            None,
            &BuildScheduling::default(),
            None,
            &importer_image,
        );
        // Pod-wide, independent of iso_overlay — the main build container's
        // image may come from the same mirror.
        assert_eq!(
            obj["spec"]["imagePullSecrets"][0]["name"],
            "mirror-pull-secret"
        );
        assert_eq!(obj["spec"]["imagePullSecrets"][1]["name"], "other-secret");
    }

    #[test]
    fn desired_os_artifact_omits_image_pull_secrets_when_unset() {
        let source = url_source("quay.io/kairos/rhel:9.8");
        let obj = desired_os_artifact(
            "kairos-rhel98-build",
            "banlieue-imagebuild",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::Iso,
            None,
            None,
            &BuildScheduling::default(),
            None,
            &ImporterImage::default(),
        );
        assert!(
            !obj["spec"]
                .as_object()
                .unwrap()
                .contains_key("imagePullSecrets")
        );
    }

    #[test]
    fn desired_os_artifact_omits_volumes_when_no_overlay() {
        let source = url_source("quay.io/kairos/rhel:9.8");
        let obj = desired_os_artifact(
            "kairos-rhel98-build",
            "banlieue-imagebuild",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::Iso,
            None,
            None,
            &BuildScheduling::default(),
            None,
            &ImporterImage::default(),
        );
        let spec = obj["spec"].as_object().unwrap();
        assert!(!spec.contains_key("volumes"));
        assert!(
            !obj["spec"]["artifacts"]
                .as_object()
                .unwrap()
                .contains_key("overlayISOVolume")
        );
    }

    #[test]
    fn desired_os_artifact_omits_volumes_when_overlay_has_no_files() {
        use banlieue_api::banlieue::IsoOverlaySource;
        use banlieue_api::common::LocalObjectReference;

        let source = url_source("quay.io/kairos/rhel:9.8");
        let overlay = IsoOverlaySource {
            secret_ref: LocalObjectReference {
                name: "empty".to_string(),
            },
            files: vec![],
        };
        let obj = desired_os_artifact(
            "kairos-rhel98-build",
            "banlieue-imagebuild",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::Iso,
            None,
            None,
            &BuildScheduling::default(),
            Some(&overlay),
            &ImporterImage::default(),
        );
        assert!(!obj["spec"].as_object().unwrap().contains_key("volumes"));
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
        assert_eq!(map_kairos_phase("Pending"), BuildArtifactPhase::Pending);
    }

    #[test]
    fn map_kairos_phase_building_and_exporting_both_map_to_building() {
        assert_eq!(map_kairos_phase("Building"), BuildArtifactPhase::Building);
        assert_eq!(map_kairos_phase("Exporting"), BuildArtifactPhase::Building);
    }

    #[test]
    fn map_kairos_phase_ready() {
        assert_eq!(map_kairos_phase("Ready"), BuildArtifactPhase::Ready);
    }

    #[test]
    fn map_kairos_phase_error_fails_closed() {
        assert_eq!(map_kairos_phase("Error"), BuildArtifactPhase::Failed);
    }

    #[test]
    fn map_kairos_phase_unrecognized_fails_closed() {
        // An unknown phase string from a future kairos-operator release must
        // never be silently treated as progress.
        assert_eq!(
            map_kairos_phase("SomeFuturePhase"),
            BuildArtifactPhase::Failed
        );
    }

    // ----------------------------------------------------------------------
    // compute_build_artifact_status
    // ----------------------------------------------------------------------

    #[test]
    fn compute_status_missing_phase_defaults_pending() {
        let view = KairosArtifactStatusView::default();
        let s = compute_build_artifact_status("x-build", BuildArtifactKind::Iso, &view, None, None);
        assert_eq!(s.phase, BuildArtifactPhase::Pending);
        assert_eq!(s.kind, BuildArtifactKind::Iso);
        assert_eq!(s.os_artifact_ref, "x-build");
        assert!(s.pvc_ref.is_none());
        assert!(s.file.is_none());
    }

    #[test]
    fn compute_status_building_has_no_pvc_yet() {
        let view = KairosArtifactStatusView {
            phase: Some("Building".to_string()),
            message: Some("pulling image".to_string()),
        };
        let s = compute_build_artifact_status("x-build", BuildArtifactKind::Iso, &view, None, None);
        assert_eq!(s.phase, BuildArtifactPhase::Building);
        assert!(s.pvc_ref.is_none());
        assert!(s.file.is_none());
        assert_eq!(s.message.as_deref(), Some("pulling image"));
    }

    #[test]
    fn compute_status_ready_iso_populates_pvc_and_iso_file() {
        let view = KairosArtifactStatusView {
            phase: Some("Ready".to_string()),
            message: None,
        };
        let s = compute_build_artifact_status(
            "kairos-rhel98-build",
            BuildArtifactKind::Iso,
            &view,
            None,
            None,
        );
        assert_eq!(s.phase, BuildArtifactPhase::Ready);
        assert_eq!(s.pvc_ref.unwrap().name, "kairos-rhel98-build-artifacts");
        assert_eq!(s.file.unwrap(), "kairos-rhel98-build.iso");
    }

    #[test]
    fn compute_status_ready_cloud_image_uses_raw_extension() {
        let view = KairosArtifactStatusView {
            phase: Some("Ready".to_string()),
            message: None,
        };
        let s = compute_build_artifact_status(
            "kairos-ubuntu-2404-build",
            BuildArtifactKind::CloudImage,
            &view,
            None,
            None,
        );
        assert_eq!(
            s.pvc_ref.unwrap().name,
            "kairos-ubuntu-2404-build-artifacts"
        );
        assert_eq!(s.file.unwrap(), "kairos-ubuntu-2404-build.raw");
    }

    #[test]
    fn compute_status_failed_carries_message_no_pvc() {
        let view = KairosArtifactStatusView {
            phase: Some("Error".to_string()),
            message: Some("pull failed: manifest unknown".to_string()),
        };
        let s = compute_build_artifact_status("x-build", BuildArtifactKind::Iso, &view, None, None);
        assert_eq!(s.phase, BuildArtifactPhase::Failed);
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
            let s =
                compute_build_artifact_status("x-build", BuildArtifactKind::Iso, &view, None, None);
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
            "banlieue-imagebuild",
            &source,
            &Architecture::Amd64,
            &BuildArtifactKind::Iso,
            None,
            Some(OwnerRef {
                name: "kairos-ubuntu-2404",
                uid: "9f2b1c7e-1234-4cde-9abc-def012345678",
            }),
            &BuildScheduling::default(),
            None,
            &ImporterImage::default(),
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
    fn spec_matches_requires_ref_arch_and_kind() {
        let data = serde_json::json!({
            "spec": {
                "image": { "ref": "quay.io/a/b@sha256:x" },
                "artifacts": { "arch": "amd64", "iso": true }
            }
        });
        assert!(spec_matches(
            &data,
            "quay.io/a/b@sha256:x",
            "amd64",
            &BuildArtifactKind::Iso
        ));
        assert!(!spec_matches(
            &data,
            "quay.io/a/c@sha256:y",
            "amd64",
            &BuildArtifactKind::Iso
        ));
        assert!(!spec_matches(
            &data,
            "quay.io/a/b@sha256:x",
            "arm64",
            &BuildArtifactKind::Iso
        ));
        // Same ref+arch but the requested kind changed (iso live, cloudImage
        // wanted) → must NOT match, forcing a rebuild.
        assert!(!spec_matches(
            &data,
            "quay.io/a/b@sha256:x",
            "amd64",
            &BuildArtifactKind::CloudImage
        ));
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
        let s = compute_build_artifact_status(
            "x-build",
            BuildArtifactKind::Iso,
            &view,
            Some("sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"),
            None,
        );
        assert_eq!(
            s.checksum.as_deref(),
            Some("sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08")
        );
    }

    #[test]
    fn compute_status_without_a_source_checksum_publishes_none() {
        let view = KairosArtifactStatusView::default();
        let s = compute_build_artifact_status("x-build", BuildArtifactKind::Iso, &view, None, None);
        assert!(s.checksum.is_none());
    }

    // ----------------------------------------------------------------------
    // ADR-0027: os_artifact_uid threading (per-zone import Job owner ref)
    // ----------------------------------------------------------------------

    #[test]
    fn compute_status_carries_os_artifact_uid_when_known() {
        let view = KairosArtifactStatusView {
            phase: Some("Building".to_string()),
            message: None,
        };
        let s = compute_build_artifact_status(
            "x-build",
            BuildArtifactKind::Iso,
            &view,
            None,
            Some("11111111-2222-3333-4444-555555555555"),
        );
        assert_eq!(
            s.os_artifact_uid.as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
    }

    #[test]
    fn compute_status_without_a_known_uid_publishes_none() {
        let view = KairosArtifactStatusView::default();
        let s = compute_build_artifact_status("x-build", BuildArtifactKind::Iso, &view, None, None);
        assert!(s.os_artifact_uid.is_none());
    }
}
