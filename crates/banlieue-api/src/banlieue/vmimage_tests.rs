// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `vmimage.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use kube::CustomResourceExt;

    fn sample_image_source(provider_class: &str) -> ImageSource {
        ImageSource {
            provider_class: provider_class.to_string(),
            kind: ImageSourceKind::Template,
            reference: "ubuntu-22.04-cloudinit".to_string(),
            import_from: None,
            checksum: None,
        }
    }

    fn minimal_vmimage_spec() -> VMImageSpec {
        VMImageSpec {
            os_family: OsFamily::Linux,
            os_distribution: "ubuntu".to_string(),
            os_version: "22.04".to_string(),
            architecture: Architecture::Amd64,
            guest_agent: GuestAgent::default(),
            sources: vec![sample_image_source("vsphere")],
            cloud_configs: vec![],
            template: None,
            iso_overlay: None,
        }
    }

    // ----------------------------------------------------------------------
    // Enums
    // ----------------------------------------------------------------------

    #[test]
    fn os_family_all_variants_round_trip() {
        for (variant, expected) in [
            (OsFamily::Linux, "linux"),
            (OsFamily::Windows, "windows"),
            (OsFamily::Bsd, "bsd"),
            (OsFamily::Other, "other"),
        ] {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json, serde_json::json!(expected));
            let back: OsFamily = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn os_family_rejects_unknown_variant() {
        let err = serde_json::from_str::<OsFamily>(r#""macos""#);
        assert!(err.is_err());
    }

    #[test]
    fn architecture_all_variants_round_trip() {
        for (variant, expected) in [
            (Architecture::Amd64, "amd64"),
            (Architecture::Arm64, "arm64"),
        ] {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json, serde_json::json!(expected));
            let back: Architecture = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn architecture_rejects_unknown_variant() {
        let err = serde_json::from_str::<Architecture>(r#""riscv64""#);
        assert!(err.is_err());
    }

    #[test]
    fn guest_agent_default_is_cloud_init() {
        assert_eq!(GuestAgent::default(), GuestAgent::CloudInit);
    }

    #[test]
    fn guest_agent_all_variants_use_kebab_case() {
        for (variant, expected) in [
            (GuestAgent::CloudInit, "cloud-init"),
            (GuestAgent::Ignition, "ignition"),
            (GuestAgent::Sysprep, "sysprep"),
            (GuestAgent::None, "none"),
        ] {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json, serde_json::json!(expected));
            let back: GuestAgent = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn image_source_kind_all_variants_round_trip() {
        for (variant, expected) in [
            (ImageSourceKind::Template, "Template"),
            (ImageSourceKind::BackingFile, "BackingFile"),
            (ImageSourceKind::Url, "Url"),
        ] {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json, serde_json::json!(expected));
            let back: ImageSourceKind = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn image_source_kind_rejects_unknown_variant() {
        let err = serde_json::from_str::<ImageSourceKind>(r#""Snapshot""#);
        assert!(err.is_err());
    }

    // ----------------------------------------------------------------------
    // ImageSource — `ref` rename and optional fields
    // ----------------------------------------------------------------------

    #[test]
    fn image_source_serializes_reference_as_ref() {
        let src = sample_image_source("vsphere");
        let json = serde_json::to_value(&src).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("ref"), "field must rename to `ref`");
        assert!(!obj.contains_key("reference"));
        assert_eq!(obj["ref"], "ubuntu-22.04-cloudinit");
    }

    #[test]
    fn image_source_minimal_omits_optional_fields() {
        let src = sample_image_source("vsphere");
        let json = serde_json::to_value(&src).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("importFrom"));
        assert!(!obj.contains_key("checksum"));
    }

    #[test]
    fn image_source_with_import_and_checksum_round_trip() {
        let src = ImageSource {
            provider_class: "proxmox".to_string(),
            kind: ImageSourceKind::Url,
            reference: "ignored".to_string(),
            import_from: Some("https://cloud-images.ubuntu.com/u.qcow2".to_string()),
            checksum: Some("sha256:deadbeef".to_string()),
        };
        let json = serde_json::to_value(&src).unwrap();
        assert_eq!(
            json["importFrom"],
            "https://cloud-images.ubuntu.com/u.qcow2"
        );
        assert_eq!(json["checksum"], "sha256:deadbeef");
        let back: ImageSource = serde_json::from_value(json).unwrap();
        assert_eq!(back, src);
    }

    #[test]
    fn image_source_missing_ref_fails() {
        let err =
            serde_json::from_str::<ImageSource>(r#"{"providerClass":"vsphere","kind":"Template"}"#);
        assert!(err.is_err());
    }

    #[test]
    fn image_source_missing_provider_class_fails() {
        let err = serde_json::from_str::<ImageSource>(r#"{"kind":"Template","ref":"t"}"#);
        assert!(err.is_err());
    }

    // ----------------------------------------------------------------------
    // VMImageSpec
    // ----------------------------------------------------------------------

    #[test]
    fn vmimage_spec_minimal_round_trip() {
        let s = minimal_vmimage_spec();
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["osFamily"], "linux");
        assert_eq!(json["osDistribution"], "ubuntu");
        assert_eq!(json["osVersion"], "22.04");
        assert_eq!(json["architecture"], "amd64");
        let back: VMImageSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn vmimage_spec_guest_agent_default_when_omitted() {
        let json = serde_json::json!({
            "osFamily": "linux",
            "osDistribution": "ubuntu",
            "osVersion": "22.04",
            "architecture": "amd64",
            "sources": [{
                "providerClass": "vsphere",
                "kind": "Template",
                "ref": "ubuntu-22.04-cloudinit"
            }]
        });
        let s: VMImageSpec = serde_json::from_value(json).unwrap();
        assert_eq!(s.guest_agent, GuestAgent::CloudInit);
    }

    #[test]
    fn vmimage_spec_missing_sources_fails() {
        let err = serde_json::from_str::<VMImageSpec>(
            r#"{"osFamily":"linux","osDistribution":"u","osVersion":"22","architecture":"amd64"}"#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn vmimage_spec_with_multiple_provider_sources_round_trip() {
        let s = VMImageSpec {
            sources: vec![
                sample_image_source("vsphere"),
                ImageSource {
                    provider_class: "proxmox".to_string(),
                    kind: ImageSourceKind::Template,
                    reference: "9000".to_string(),
                    import_from: None,
                    checksum: None,
                },
                ImageSource {
                    provider_class: "libvirt".to_string(),
                    kind: ImageSourceKind::BackingFile,
                    reference: "/var/lib/libvirt/images/ubuntu.qcow2".to_string(),
                    import_from: None,
                    checksum: None,
                },
            ],
            ..minimal_vmimage_spec()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["sources"].as_array().unwrap().len(), 3);
        let back: VMImageSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    // ----------------------------------------------------------------------
    // VMImageStatus / ImagePerProviderStatus
    // ----------------------------------------------------------------------

    #[test]
    fn vmimage_status_default_omits_everything() {
        let s = VMImageStatus::default();
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json, serde_json::json!({}));
    }

    #[test]
    fn image_per_provider_status_minimal_round_trip() {
        let p = ImagePerProviderStatus {
            provider_name: "vsphere-dc1".to_string(),
            provider_namespace: "infra".to_string(),
            ready: true,
            resolved_ref: None,
            reason: None,
            message: None,
            zones: vec![],
        };
        let json = serde_json::to_value(&p).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("resolvedRef"));
        assert!(!obj.contains_key("reason"));
        assert!(!obj.contains_key("message"));
        assert!(!obj.contains_key("zones"));
        let back: ImagePerProviderStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn image_per_provider_status_with_reason_and_message_round_trip() {
        let p = ImagePerProviderStatus {
            provider_name: "p".to_string(),
            provider_namespace: "ns".to_string(),
            ready: false,
            resolved_ref: Some("[dc1] folder/ubuntu".to_string()),
            reason: Some("ImagePending".to_string()),
            message: Some("Importing from URL".to_string()),
            zones: vec![],
        };
        let json = serde_json::to_value(&p).unwrap();
        let back: ImagePerProviderStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn image_per_provider_status_missing_ready_fails() {
        let err = serde_json::from_str::<ImagePerProviderStatus>(
            r#"{"providerName":"p","providerNamespace":"n"}"#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn image_per_provider_status_omitted_zones_defaults_empty() {
        let json = serde_json::json!({
            "providerName": "p",
            "providerNamespace": "n",
            "ready": true
        });
        let back: ImagePerProviderStatus = serde_json::from_value(json).unwrap();
        assert!(back.zones.is_empty());
    }

    // ----------------------------------------------------------------------
    // ZoneImageStatus — per-zone import progress within a Provider
    // ----------------------------------------------------------------------

    #[test]
    fn zone_image_status_minimal_round_trip() {
        let z = ZoneImageStatus {
            name: "az1".to_string(),
            ready: false,
            resolved_ref: None,
            template_folder: None,
            reason: Some("Importing".to_string()),
            message: None,
        };
        let json = serde_json::to_value(&z).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("resolvedRef"));
        assert!(!obj.contains_key("templateFolder"));
        assert!(!obj.contains_key("message"));
        let back: ZoneImageStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, z);
    }

    #[test]
    fn zone_image_status_with_template_folder_round_trip() {
        // resolved_ref is the bare template name; template_folder is the
        // per-zone folder it lives in — two structured fields, not one
        // decorated/parsed string.
        let z = ZoneImageStatus {
            name: "cluster-01".to_string(),
            ready: true,
            resolved_ref: Some("hadron-kairos-v0.1.0".to_string()),
            template_folder: Some("templates/cluster-01".to_string()),
            reason: Some("Reconciled".to_string()),
            message: None,
        };
        let json = serde_json::to_value(&z).unwrap();
        assert_eq!(json["resolvedRef"], "hadron-kairos-v0.1.0");
        assert_eq!(json["templateFolder"], "templates/cluster-01");
        let back: ZoneImageStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, z);
    }

    #[test]
    fn image_per_provider_status_with_zones_round_trip() {
        let p = ImagePerProviderStatus {
            provider_name: "vsphere-devnonprod".to_string(),
            provider_namespace: "infra".to_string(),
            ready: false,
            resolved_ref: None,
            reason: Some("Importing".to_string()),
            message: None,
            zones: vec![
                ZoneImageStatus {
                    name: "az1".to_string(),
                    ready: true,
                    resolved_ref: Some("kairos-ubuntu-2404".to_string()),
                    template_folder: Some("templates/az1".to_string()),
                    reason: Some("Reconciled".to_string()),
                    message: None,
                },
                ZoneImageStatus {
                    name: "az2".to_string(),
                    ready: false,
                    resolved_ref: None,
                    template_folder: None,
                    reason: Some("Importing".to_string()),
                    message: Some("uploading to datastore2".to_string()),
                },
            ],
        };
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["zones"].as_array().unwrap().len(), 2);
        let back: ImagePerProviderStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, p);
    }

    // ----------------------------------------------------------------------
    // BuildArtifactKind / BuildArtifactPhase / BuildArtifactStatus /
    // VMImageStatus.buildArtifact (ADR-0020: typed, kairos-aligned)
    // ----------------------------------------------------------------------

    #[test]
    fn build_artifact_kind_serializes_to_kairos_vocabulary() {
        // Values MUST match kairos-operator's OSArtifactKind strings so the
        // vocabulary is not banlieue-invented.
        assert_eq!(
            serde_json::to_value(BuildArtifactKind::CloudImage).unwrap(),
            serde_json::json!("cloudImage")
        );
        assert_eq!(
            serde_json::to_value(BuildArtifactKind::Iso).unwrap(),
            serde_json::json!("iso")
        );
        let back: BuildArtifactKind = serde_json::from_str(r#""iso""#).unwrap();
        assert_eq!(back, BuildArtifactKind::Iso);
    }

    #[test]
    fn build_artifact_phase_all_variants_round_trip() {
        for (variant, expected) in [
            (BuildArtifactPhase::Pending, "Pending"),
            (BuildArtifactPhase::Building, "Building"),
            (BuildArtifactPhase::Ready, "Ready"),
            (BuildArtifactPhase::Failed, "Failed"),
        ] {
            let json = serde_json::to_value(&variant).unwrap();
            assert_eq!(json, serde_json::json!(expected));
            let back: BuildArtifactPhase = serde_json::from_value(json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn build_artifact_phase_rejects_unknown_variant() {
        // In particular: kairos-operator's own `Exporting` / `Error` phases are
        // NOT valid banlieue BuildArtifactPhase values — banlieue-imagebuilder
        // maps Exporting->Building and Error->Failed before writing this field;
        // the raw kairos strings must never leak through unmapped.
        let err = serde_json::from_str::<BuildArtifactPhase>(r#""Exporting""#);
        assert!(err.is_err());
        let err = serde_json::from_str::<BuildArtifactPhase>(r#""Error""#);
        assert!(err.is_err());
    }

    #[test]
    fn build_artifact_status_pending_minimal_round_trip() {
        let s = BuildArtifactStatus {
            kind: BuildArtifactKind::CloudImage,
            phase: BuildArtifactPhase::Pending,
            os_artifact_ref: "kairos-ubuntu-2404-build".to_string(),
            os_artifact_uid: None,
            pvc_ref: None,
            file: None,
            reason: None,
            message: None,
            checksum: None,
        };
        let json = serde_json::to_value(&s).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("pvcRef"));
        assert!(!obj.contains_key("file"));
        assert_eq!(json["kind"], "cloudImage");
        let back: BuildArtifactStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn build_artifact_status_iso_ready_round_trip() {
        let s = BuildArtifactStatus {
            kind: BuildArtifactKind::Iso,
            phase: BuildArtifactPhase::Ready,
            os_artifact_ref: "kairos-rhel98-build".to_string(),
            os_artifact_uid: None,
            pvc_ref: Some(LocalObjectReference {
                name: "kairos-rhel98-build-artifacts".to_string(),
            }),
            file: Some("kairos-rhel98-build.iso".to_string()),
            reason: Some("Reconciled".to_string()),
            message: None,
            checksum: None,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["kind"], "iso");
        assert_eq!(json["pvcRef"]["name"], "kairos-rhel98-build-artifacts");
        assert_eq!(json["file"], "kairos-rhel98-build.iso");
        let back: BuildArtifactStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    // ADR-0027: os_artifact_uid — omitted when unknown, round-trips when set.
    #[test]
    fn build_artifact_status_os_artifact_uid_omitted_when_none() {
        let s = BuildArtifactStatus {
            kind: BuildArtifactKind::Iso,
            phase: BuildArtifactPhase::Pending,
            os_artifact_ref: "kairos-rhel98-build".to_string(),
            os_artifact_uid: None,
            pvc_ref: None,
            file: None,
            reason: None,
            message: None,
            checksum: None,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert!(!json.as_object().unwrap().contains_key("osArtifactUid"));
    }

    #[test]
    fn build_artifact_status_os_artifact_uid_round_trips_when_known() {
        let s = BuildArtifactStatus {
            kind: BuildArtifactKind::Iso,
            phase: BuildArtifactPhase::Building,
            os_artifact_ref: "kairos-rhel98-build".to_string(),
            os_artifact_uid: Some("11111111-2222-3333-4444-555555555555".to_string()),
            pvc_ref: None,
            file: None,
            reason: None,
            message: None,
            checksum: None,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json["osArtifactUid"],
            "11111111-2222-3333-4444-555555555555"
        );
        let back: BuildArtifactStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn build_artifact_status_missing_kind_fails() {
        let err = serde_json::from_str::<BuildArtifactStatus>(
            r#"{"phase":"Pending","osArtifactRef":"kairos-ubuntu-2404-build"}"#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn build_artifact_status_missing_phase_fails() {
        let err = serde_json::from_str::<BuildArtifactStatus>(
            r#"{"kind":"iso","osArtifactRef":"kairos-ubuntu-2404-build"}"#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn vmimage_status_omits_build_artifact_when_none() {
        let s = VMImageStatus::default();
        assert!(s.build_artifact.is_none());
        let json = serde_json::to_value(&s).unwrap();
        assert!(!json.as_object().unwrap().contains_key("buildArtifact"));
    }

    #[test]
    fn vmimage_status_with_build_artifact_round_trip() {
        let s = VMImageStatus {
            build_artifact: Some(BuildArtifactStatus {
                kind: BuildArtifactKind::Iso,
                phase: BuildArtifactPhase::Building,
                os_artifact_ref: "kairos-ubuntu-2404-build".to_string(),
                os_artifact_uid: None,
                pvc_ref: None,
                file: None,
                reason: None,
                message: None,
                checksum: None,
            }),
            ..VMImageStatus::default()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["buildArtifact"]["phase"], "Building");
        assert_eq!(json["buildArtifact"]["kind"], "iso");
        let back: VMImageStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    // ----------------------------------------------------------------------
    // VMImageSpec.cloudConfigs (ADR-0037)
    // ----------------------------------------------------------------------

    #[test]
    fn vmimage_spec_omits_cloud_configs_when_empty() {
        let s = minimal_vmimage_spec();
        assert!(s.cloud_configs.is_empty());
        let json = serde_json::to_value(&s).unwrap();
        assert!(!json.as_object().unwrap().contains_key("cloudConfigs"));
    }

    #[test]
    fn vmimage_spec_with_single_cloud_config_round_trip() {
        use crate::common::{CloudConfigSource, KeySelector};
        let s = VMImageSpec {
            cloud_configs: vec![CloudConfigSource {
                secret_ref: Some(KeySelector {
                    name: "kairos-base-cloud-config".to_string(),
                    key: None,
                }),
            }],
            ..minimal_vmimage_spec()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json["cloudConfigs"][0]["secretRef"]["name"],
            "kairos-base-cloud-config"
        );
        let back: VMImageSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn vmimage_spec_with_layered_cloud_configs_round_trip() {
        use crate::common::{CloudConfigSource, KeySelector};
        let s = VMImageSpec {
            cloud_configs: vec![
                CloudConfigSource {
                    secret_ref: Some(KeySelector {
                        name: "kairos-base-cloud-config".to_string(),
                        key: None,
                    }),
                },
                CloudConfigSource {
                    secret_ref: Some(KeySelector {
                        name: "kairos-crowdstrike-overlay".to_string(),
                        key: Some("90_crowdstrike.yaml".to_string()),
                    }),
                },
            ],
            ..minimal_vmimage_spec()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["cloudConfigs"].as_array().unwrap().len(), 2);
        assert_eq!(
            json["cloudConfigs"][0]["secretRef"]["name"],
            "kairos-base-cloud-config"
        );
        assert_eq!(
            json["cloudConfigs"][1]["secretRef"]["name"],
            "kairos-crowdstrike-overlay"
        );
        assert_eq!(
            json["cloudConfigs"][1]["secretRef"]["key"],
            "90_crowdstrike.yaml"
        );
        let back: VMImageSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    // ----------------------------------------------------------------------
    // VMImageTemplate.installTimeoutSeconds (ADR-0021)
    // ----------------------------------------------------------------------

    #[test]
    fn vmimage_template_omits_install_timeout_when_none() {
        let t = VMImageTemplate::default();
        assert!(t.install_timeout_seconds.is_none());
        let json = serde_json::to_value(&t).unwrap();
        assert!(
            !json
                .as_object()
                .unwrap()
                .contains_key("installTimeoutSeconds")
        );
    }

    #[test]
    fn vmimage_template_with_install_timeout_round_trip() {
        let t = VMImageTemplate {
            install_timeout_seconds: Some(1800),
            ..VMImageTemplate::default()
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["installTimeoutSeconds"], 1800);
        let back: VMImageTemplate = serde_json::from_value(json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn vmimage_template_defaults_to_immediate_install_mode() {
        let t = VMImageTemplate::default();
        assert_eq!(t.install_mode, InstallMode::Immediate);
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["installMode"], "immediate");
    }

    #[test]
    fn vmimage_template_missing_install_mode_deserializes_to_immediate() {
        let t: VMImageTemplate = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(t.install_mode, InstallMode::Immediate);
    }

    #[test]
    fn vmimage_template_deferred_install_mode_round_trip() {
        // ADR-0040: the sanctioned mode for a tpmEnabled: true VMClass — the
        // install runs once per clone, at that clone's own first boot.
        let t = VMImageTemplate {
            install_mode: InstallMode::Deferred,
            ..VMImageTemplate::default()
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["installMode"], "deferred");
        let back: VMImageTemplate = serde_json::from_value(json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn vmimage_template_manual_install_mode_round_trip() {
        let t = VMImageTemplate {
            install_mode: InstallMode::Manual,
            ..VMImageTemplate::default()
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["installMode"], "manual");
        let back: VMImageTemplate = serde_json::from_value(json).unwrap();
        assert_eq!(back, t);
    }

    // ----------------------------------------------------------------------
    // VMImageTemplate.retainOnDelete (ADR-0028)
    // ----------------------------------------------------------------------

    #[test]
    fn vmimage_template_defaults_to_not_retaining_on_delete() {
        let t = VMImageTemplate::default();
        assert!(!t.retain_on_delete);
    }

    #[test]
    fn vmimage_template_omits_retain_on_delete_when_false() {
        let t = VMImageTemplate::default();
        let json = serde_json::to_value(&t).unwrap();
        assert!(!json.as_object().unwrap().contains_key("retainOnDelete"));
    }

    #[test]
    fn vmimage_template_with_retain_on_delete_true_round_trip() {
        let t = VMImageTemplate {
            retain_on_delete: true,
            ..VMImageTemplate::default()
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["retainOnDelete"], true);
        let back: VMImageTemplate = serde_json::from_value(json).unwrap();
        assert_eq!(back, t);
    }

    // ----------------------------------------------------------------------
    // VMImageTemplate.network: Vec<VMImageTemplateNic> (ADR-0031)
    // ----------------------------------------------------------------------

    #[test]
    fn vmimage_template_defaults_to_no_nics() {
        let t = VMImageTemplate::default();
        assert!(t.network.is_empty());
    }

    #[test]
    fn vmimage_template_omits_network_when_empty() {
        let t = VMImageTemplate::default();
        let json = serde_json::to_value(&t).unwrap();
        assert!(!json.as_object().unwrap().contains_key("network"));
    }

    #[test]
    fn vmimage_template_nic_defaults_are_all_none() {
        let nic = VMImageTemplateNic::default();
        assert!(nic.network.is_none());
        assert!(nic.adapter.is_none());
        assert!(nic.pci_slot.is_none());
    }

    #[test]
    fn vmimage_template_with_multiple_nics_round_trip() {
        let t = VMImageTemplate {
            network: vec![
                VMImageTemplateNic {
                    network: Some("vmnet-prod".to_string()),
                    adapter: Some(NicAdapter::Vmxnet3),
                    pci_slot: Some(192),
                },
                VMImageTemplateNic {
                    network: Some("vmnet-mgmt".to_string()),
                    adapter: None,
                    pci_slot: None,
                },
            ],
            ..VMImageTemplate::default()
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["network"][0]["network"], "vmnet-prod");
        assert_eq!(json["network"][0]["adapter"], "vmxnet3");
        assert_eq!(json["network"][0]["pciSlot"], 192);
        assert_eq!(json["network"][1]["network"], "vmnet-mgmt");
        assert!(json["network"][1].get("adapter").is_none());
        assert!(json["network"][1].get("pciSlot").is_none());
        let back: VMImageTemplate = serde_json::from_value(json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn cloud_config_source_validate_requires_a_source() {
        use crate::common::{CloudConfigSource, KeySelector};
        assert!(CloudConfigSource::default().validate().is_err());
        let ok = CloudConfigSource {
            secret_ref: Some(KeySelector {
                name: "cc".to_string(),
                key: None,
            }),
        };
        assert!(ok.validate().is_ok());
    }

    // ----------------------------------------------------------------------
    // VMImageSpec.isoOverlay (ADR-0022)
    // ----------------------------------------------------------------------

    #[test]
    fn vmimage_spec_omits_iso_overlay_when_none() {
        let s = minimal_vmimage_spec();
        assert!(s.iso_overlay.is_none());
        let json = serde_json::to_value(&s).unwrap();
        assert!(!json.as_object().unwrap().contains_key("isoOverlay"));
    }

    #[test]
    fn vmimage_spec_with_iso_overlay_round_trip() {
        let s = VMImageSpec {
            iso_overlay: Some(IsoOverlaySource {
                secret_ref: LocalObjectReference {
                    name: "kairos-iso-overlay".to_string(),
                },
                files: vec![IsoOverlayFile {
                    key: "grub.cfg".to_string(),
                    path: "boot/grub2/grub.cfg".to_string(),
                }],
            }),
            ..minimal_vmimage_spec()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json["isoOverlay"]["secretRef"]["name"],
            "kairos-iso-overlay"
        );
        assert_eq!(json["isoOverlay"]["files"][0]["key"], "grub.cfg");
        assert_eq!(
            json["isoOverlay"]["files"][0]["path"],
            "boot/grub2/grub.cfg"
        );
        let back: VMImageSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn iso_overlay_source_missing_secret_ref_fails() {
        let err = serde_json::from_str::<IsoOverlaySource>(
            r#"{"files":[{"key":"grub.cfg","path":"boot/grub2/grub.cfg"}]}"#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn iso_overlay_file_missing_path_fails() {
        let err = serde_json::from_str::<IsoOverlayFile>(r#"{"key":"grub.cfg"}"#);
        assert!(err.is_err());
    }

    // ----------------------------------------------------------------------
    // CRD generation
    // ----------------------------------------------------------------------

    #[test]
    fn vmimage_crd_metadata_matches_kube_attributes() {
        let crd = VMImage::crd();
        assert_eq!(crd.spec.group, "banlieue.io");
        assert_eq!(crd.spec.names.kind, "VMImage");
        assert_eq!(crd.spec.names.plural, "vmimages");
        // VMImage is cluster-scoped (no `namespaced` attribute on the macro).
        assert_eq!(crd.spec.scope, "Cluster");
    }

    #[test]
    fn sources_rejects_duplicate_provider_classes_at_admission() {
        // `sources[]` is one entry per backend binding for this catalog
        // entry (ADR: "one name, many backends") — at most one per
        // providerClass. `x-kubernetes-list-type: map` isn't just an SSA
        // merge hint: the API server enforces list-map-key uniqueness at
        // admission, rejecting a second `sources[]` entry for a
        // providerClass that already has one, instead of `find_url_source`
        // / `find_vsphere_source` silently picking whichever came first.
        let crd = VMImage::crd();
        let json = serde_json::to_value(&crd).unwrap();
        let sources_schema = &json["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]
            ["spec"]["properties"]["sources"];
        assert_eq!(sources_schema["x-kubernetes-list-type"], "map");
        assert_eq!(
            sources_schema["x-kubernetes-list-map-keys"][0],
            "providerClass"
        );
    }
}
