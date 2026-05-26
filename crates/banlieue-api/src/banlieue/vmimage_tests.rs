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
        };
        let json = serde_json::to_value(&p).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("resolvedRef"));
        assert!(!obj.contains_key("reason"));
        assert!(!obj.contains_key("message"));
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
}
