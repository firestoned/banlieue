// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `vmimage.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{Architecture, BuildArtifactKind, ImageSource, ImageSourceKind};
    use banlieue_api::common::{CloudConfigSource, KeySelector};
    use banlieue_provider_sdk::scheduling::BuildScheduling;

    /// `desired_os_artifact`'s arguments, defaulted to what most tests pass:
    /// an amd64 `iso` build in the build namespace with nothing optional set.
    struct OsArtifactArgs<'a> {
        name: &'a str,
        namespace: &'a str,
        source: &'a ImageSource,
        architecture: Architecture,
        kind: BuildArtifactKind,
        cloud_config: Option<&'a CloudConfigSource>,
        owner: Option<OwnerRef<'a>>,
        scheduling: BuildScheduling,
        iso_overlay: Option<&'a IsoOverlaySource>,
        importer_image: ImporterImage,
        trusted_boot: Option<&'a TrustedBootSource>,
    }

    impl<'a> OsArtifactArgs<'a> {
        fn new(source: &'a ImageSource) -> Self {
            Self {
                name: "kairos-rhel98-build",
                namespace: "banlieue-imagebuild",
                source,
                architecture: Architecture::Amd64,
                kind: BuildArtifactKind::Iso,
                cloud_config: None,
                owner: None,
                scheduling: BuildScheduling::default(),
                iso_overlay: None,
                importer_image: ImporterImage::default(),
                trusted_boot: None,
            }
        }

        fn render(&self) -> Value {
            desired_os_artifact(
                self.name,
                self.namespace,
                self.source,
                &self.architecture,
                &self.kind,
                self.cloud_config,
                self.owner,
                &self.scheduling,
                self.iso_overlay,
                &self.importer_image,
                self.trusted_boot,
            )
        }
    }

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

    /// ADR-0065 Decision 3: a Cloud Hypervisor `Deferred` install boots the
    /// installer ISO onto an empty disk, so its build is an ISO; an
    /// `Immediate` one clones a raw disk.
    #[test]
    fn a_kvm_deferred_image_builds_an_iso() {
        use banlieue_api::banlieue::InstallMode;
        assert_eq!(
            artifact_kind_for("cloud-hypervisor", &InstallMode::Deferred),
            BuildArtifactKind::Iso
        );
        assert_eq!(
            artifact_kind_for("cloud-hypervisor", &InstallMode::Immediate),
            BuildArtifactKind::CloudImage
        );
        assert_eq!(
            artifact_kind_for("vsphere", &InstallMode::Immediate),
            BuildArtifactKind::Iso
        );
        // libvirt attaches the same installer as a CD-ROM.
        assert_eq!(
            artifact_kind_for("libvirt", &InstallMode::Deferred),
            BuildArtifactKind::Iso
        );
        assert_eq!(
            artifact_kind_for("libvirt", &InstallMode::Immediate),
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
        let obj = OsArtifactArgs::new(&source).render();
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
        let obj = OsArtifactArgs {
            name: "kairos-ubuntu-2404-build",
            kind: BuildArtifactKind::CloudImage,
            ..OsArtifactArgs::new(&source)
        }
        .render();
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
        let obj = OsArtifactArgs {
            cloud_config: Some(&cc),
            ..OsArtifactArgs::new(&source)
        }
        .render();
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
        let obj = OsArtifactArgs {
            name: "x-build",
            namespace: "ns",
            cloud_config: Some(&cc),
            ..OsArtifactArgs::new(&source)
        }
        .render();
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
        let obj = OsArtifactArgs {
            name: "img-build",
            ..OsArtifactArgs::new(&source)
        }
        .render();
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
        let obj = OsArtifactArgs {
            name: "img-build",
            scheduling: scheduling.clone(),
            ..OsArtifactArgs::new(&source)
        }
        .render();
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
        let obj = OsArtifactArgs {
            name: "x-build",
            namespace: "ns",
            architecture: Architecture::Arm64,
            ..OsArtifactArgs::new(&source)
        }
        .render();
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
        let obj = OsArtifactArgs {
            iso_overlay: Some(&overlay),
            ..OsArtifactArgs::new(&source)
        }
        .render();
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
        let obj = OsArtifactArgs {
            iso_overlay: Some(&overlay),
            ..OsArtifactArgs::new(&source)
        }
        .render();
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
        let obj = OsArtifactArgs {
            iso_overlay: Some(&overlay),
            importer_image: importer_image.clone(),
            ..OsArtifactArgs::new(&source)
        }
        .render();
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
        let obj = OsArtifactArgs {
            importer_image: importer_image.clone(),
            ..OsArtifactArgs::new(&source)
        }
        .render();
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
        let obj = OsArtifactArgs::new(&source).render();
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
        let obj = OsArtifactArgs::new(&source).render();
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
        let obj = OsArtifactArgs {
            iso_overlay: Some(&overlay),
            ..OsArtifactArgs::new(&source)
        }
        .render();
        assert!(!obj["spec"].as_object().unwrap().contains_key("volumes"));
    }

    // ----------------------------------------------------------------------
    // desired_os_artifact — trustedBoot / UKI wiring (ADR-0051)
    // ----------------------------------------------------------------------

    #[test]
    fn desired_os_artifact_wires_trusted_boot_uki() {
        use banlieue_api::banlieue::TrustedBootSource;
        use banlieue_api::common::LocalObjectReference;

        let source = url_source("quay.io/kairos/rhel:9.8");
        let trusted_boot = TrustedBootSource {
            secret_ref: LocalObjectReference {
                name: "kairos-trusted-boot-keys".to_string(),
            },
        };
        let obj = OsArtifactArgs {
            trusted_boot: Some(&trusted_boot),
            ..OsArtifactArgs::new(&source)
        }
        .render();
        // Requests the UKI artifact, not the plain `iso` flag — auroraboot
        // build-uki instead of build-iso, since the base rootfs is a Unified
        // Kernel Image with no discrete initrd for build-iso to find.
        assert!(
            !obj["spec"]["artifacts"]
                .as_object()
                .unwrap()
                .contains_key("iso")
        );
        assert_eq!(obj["spec"]["artifacts"]["uki"]["iso"], true);
        assert_eq!(
            obj["spec"]["artifacts"]["uki"]["keysVolume"],
            "trusted-boot-keys"
        );
        assert_eq!(obj["spec"]["volumes"][0]["name"], "trusted-boot-keys");
        assert_eq!(
            obj["spec"]["volumes"][0]["secret"]["secretName"],
            "kairos-trusted-boot-keys"
        );
    }

    #[test]
    fn desired_os_artifact_requests_plain_iso_without_trusted_boot() {
        let source = url_source("quay.io/kairos/rhel:9.8");
        let obj = OsArtifactArgs::new(&source).render();
        assert_eq!(obj["spec"]["artifacts"]["iso"], true);
        assert!(
            !obj["spec"]["artifacts"]
                .as_object()
                .unwrap()
                .contains_key("uki")
        );
        assert!(!obj["spec"].as_object().unwrap().contains_key("volumes"));
    }

    #[test]
    fn desired_os_artifact_trusted_boot_combines_with_iso_overlay_volumes() {
        use banlieue_api::banlieue::{IsoOverlayFile, IsoOverlaySource, TrustedBootSource};
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
        let trusted_boot = TrustedBootSource {
            secret_ref: LocalObjectReference {
                name: "kairos-trusted-boot-keys".to_string(),
            },
        };
        let obj = OsArtifactArgs {
            iso_overlay: Some(&overlay),
            trusted_boot: Some(&trusted_boot),
            ..OsArtifactArgs::new(&source)
        }
        .render();
        let volumes = obj["spec"]["volumes"].as_array().unwrap();
        let names: Vec<&str> = volumes
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"iso-overlay-source"));
        assert!(names.contains(&"iso-overlay"));
        assert!(names.contains(&"trusted-boot-keys"));
        assert_eq!(obj["spec"]["artifacts"]["overlayISOVolume"], "iso-overlay");
        assert_eq!(
            obj["spec"]["artifacts"]["uki"]["keysVolume"],
            "trusted-boot-keys"
        );
    }

    #[test]
    fn desired_os_artifact_trusted_boot_without_cloud_config_skips_iso_overlay() {
        use banlieue_api::banlieue::TrustedBootSource;
        use banlieue_api::common::LocalObjectReference;

        let source = url_source("quay.io/kairos/rhel:9.8");
        let trusted_boot = TrustedBootSource {
            secret_ref: LocalObjectReference {
                name: "kairos-trusted-boot-keys".to_string(),
            },
        };
        let obj = OsArtifactArgs {
            trusted_boot: Some(&trusted_boot),
            ..OsArtifactArgs::new(&source)
        }
        .render();
        assert!(
            !obj["spec"]["artifacts"]
                .as_object()
                .unwrap()
                .contains_key("overlayISOVolume")
        );
        let volumes = obj["spec"]["volumes"].as_array().unwrap();
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0]["name"], "trusted-boot-keys");
    }

    #[test]
    fn desired_os_artifact_trusted_boot_with_cloud_config_skips_cloud_config_ref_and_uses_iso_overlay()
     {
        use banlieue_api::banlieue::TrustedBootSource;
        use banlieue_api::common::LocalObjectReference;

        let source = url_source("quay.io/kairos/rhel:9.8");
        let cc = CloudConfigSource {
            secret_ref: Some(KeySelector {
                name: "kairos-base-cloud-config".to_string(),
                key: None,
            }),
        };
        let trusted_boot = TrustedBootSource {
            secret_ref: LocalObjectReference {
                name: "kairos-trusted-boot-keys".to_string(),
            },
        };
        let obj = OsArtifactArgs {
            cloud_config: Some(&cc),
            trusted_boot: Some(&trusted_boot),
            ..OsArtifactArgs::new(&source)
        }
        .render();
        // kairos-operator's buildUKICommand unconditionally appends
        // `--cloud-config` whenever cloudConfigRef is set, but
        // `auroraboot build-uki` has no such flag ("flag provided but not
        // defined: -cloud-config", confirmed live) — so cloudConfigRef must
        // never be set alongside trustedBoot. Instead the cloud-config is
        // baked in via the *same* overlayISOVolume mechanism as isoOverlay
        // (ADR-0022), placed at the ISO root as `config.yaml` — the exact
        // path/filename `auroraboot build-iso --cloud-config` itself writes
        // to internally (confirmed against AuroraBoot's source), which
        // kairos-agent's installer already scans for via
        // `/run/initramfs/live` (the ISO's own live-boot mount).
        assert!(
            !obj["spec"]["artifacts"]
                .as_object()
                .unwrap()
                .contains_key("cloudConfigRef")
        );
        assert_eq!(obj["spec"]["artifacts"]["overlayISOVolume"], "iso-overlay");
        let volumes = obj["spec"]["volumes"].as_array().unwrap();
        let names: Vec<&str> = volumes
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"trusted-boot-cloud-config-source"));
        assert!(names.contains(&"iso-overlay"));
        assert!(names.contains(&"trusted-boot-keys"));
        let source_volume = volumes
            .iter()
            .find(|v| v["name"] == "trusted-boot-cloud-config-source")
            .unwrap();
        assert_eq!(
            source_volume["secret"]["secretName"],
            "kairos-base-cloud-config"
        );
        assert_eq!(
            source_volume["secret"]["items"][0]["key"],
            "cloud-config.yaml"
        );
        assert_eq!(source_volume["secret"]["items"][0]["path"], "config.yaml");
        let importer = obj["spec"]["importers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["name"] == "trusted-boot-cloud-config-materialize")
            .expect("expected the trusted-boot-cloud-config-materialize importer");
        let mounts = importer["volumeMounts"].as_array().unwrap();
        assert_eq!(mounts[0]["name"], "trusted-boot-cloud-config-source");
        assert_eq!(mounts[1]["name"], "iso-overlay");
    }

    #[test]
    fn desired_os_artifact_iso_overlay_and_trusted_boot_cloud_config_importers_both_present() {
        use banlieue_api::banlieue::{IsoOverlayFile, IsoOverlaySource, TrustedBootSource};
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
        let cc = CloudConfigSource {
            secret_ref: Some(KeySelector {
                name: "kairos-base-cloud-config".to_string(),
                key: None,
            }),
        };
        let trusted_boot = TrustedBootSource {
            secret_ref: LocalObjectReference {
                name: "kairos-trusted-boot-keys".to_string(),
            },
        };
        let obj = OsArtifactArgs {
            cloud_config: Some(&cc),
            iso_overlay: Some(&overlay),
            trusted_boot: Some(&trusted_boot),
            ..OsArtifactArgs::new(&source)
        }
        .render();
        // Both importers must survive — a naive `spec.insert("importers", ...)`
        // per mechanism would let the second silently clobber the first. Both
        // write into the *same* shared "iso-overlay" emptyDir (only one
        // overlayISOVolume is possible), at non-colliding paths
        // (boot/grub2/grub.cfg vs. config.yaml).
        let importer_names: Vec<&str> = obj["spec"]["importers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap())
            .collect();
        assert!(importer_names.contains(&"iso-overlay-materialize"));
        assert!(importer_names.contains(&"trusted-boot-cloud-config-materialize"));
        assert_eq!(importer_names.len(), 2);
        // Only one "iso-overlay" emptyDir, not a duplicate per mechanism.
        let overlay_emptydir_count = obj["spec"]["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["name"] == "iso-overlay")
            .count();
        assert_eq!(overlay_emptydir_count, 1);
    }

    // ----------------------------------------------------------------------
    // missing_trusted_boot_keys (ADR-0051 Decision #3 — preflight validation)
    // ----------------------------------------------------------------------

    #[test]
    fn missing_trusted_boot_keys_empty_when_all_six_present() {
        let present: std::collections::BTreeSet<String> = [
            "PK.auth",
            "KEK.auth",
            "db.auth",
            "db.key",
            "db.pem",
            "tpm2-pcr-private.pem",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert!(missing_trusted_boot_keys(&present).is_empty());
    }

    #[test]
    fn missing_trusted_boot_keys_reports_all_six_when_secret_empty() {
        let present = std::collections::BTreeSet::new();
        assert_eq!(
            missing_trusted_boot_keys(&present),
            vec![
                "PK.auth",
                "KEK.auth",
                "db.auth",
                "db.key",
                "db.pem",
                "tpm2-pcr-private.pem",
            ]
        );
    }

    #[test]
    fn missing_trusted_boot_keys_reports_only_the_absent_ones_in_fixed_order() {
        let present: std::collections::BTreeSet<String> =
            ["db.pem", "PK.auth", "unrelated-extra-key"]
                .into_iter()
                .map(str::to_string)
                .collect();
        assert_eq!(
            missing_trusted_boot_keys(&present),
            vec!["KEK.auth", "db.auth", "db.key", "tpm2-pcr-private.pem"]
        );
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
        let s =
            compute_build_artifact_status("x-build", BuildArtifactKind::Iso, &view, None, false);
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
        let s =
            compute_build_artifact_status("x-build", BuildArtifactKind::Iso, &view, None, false);
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
            false,
        );
        assert_eq!(s.phase, BuildArtifactPhase::Ready);
        assert_eq!(s.pvc_ref.unwrap().name, "kairos-rhel98-build-artifacts");
        assert_eq!(s.file.unwrap(), "kairos-rhel98-build.iso");
    }

    #[test]
    fn compute_status_ready_trusted_boot_iso_appends_uki_suffix() {
        // kairos-operator's buildUKICommand names the auroraboot --name flag
        // `<osArtifactName>-uki` unconditionally for a UKI build (unless
        // spec.nameOverride.uki is set — not something banlieue sets), and
        // AuroraBoot writes exactly `<name>.iso` for output-type iso. So the
        // real file on the PVC is `<osArtifactName>-uki.iso`, not
        // `<osArtifactName>.iso` — confirmed against a live import failure
        // ("cannot read .../debian-tpm-dev-v0.4.0-build.iso") plus
        // kairos-operator/AuroraBoot source (ADR-0051 Decision #5).
        let view = KairosArtifactStatusView {
            phase: Some("Ready".to_string()),
            message: None,
        };
        let s = compute_build_artifact_status(
            "kairos-rhel98-build",
            BuildArtifactKind::Iso,
            &view,
            None,
            true,
        );
        assert_eq!(s.phase, BuildArtifactPhase::Ready);
        assert_eq!(s.pvc_ref.unwrap().name, "kairos-rhel98-build-artifacts");
        assert_eq!(s.file.unwrap(), "kairos-rhel98-build-uki.iso");
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
            false,
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
        let s =
            compute_build_artifact_status("x-build", BuildArtifactKind::Iso, &view, None, false);
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
            let s = compute_build_artifact_status(
                "x-build",
                BuildArtifactKind::Iso,
                &view,
                None,
                false,
            );
            assert_eq!(s.reason.as_deref(), Some(expected_reason));
        }
    }

    // ----------------------------------------------------------------------
    // SEC-005: owner reference + staleness binding
    // ----------------------------------------------------------------------

    #[test]
    fn desired_os_artifact_carries_the_vmimage_owner_reference() {
        let source = url_source("quay.io/kairos/ubuntu:24.04-core-amd64-generic-v3.7.2");
        let obj = OsArtifactArgs {
            name: "kairos-ubuntu-2404-build",
            owner: Some(OwnerRef {
                name: "kairos-ubuntu-2404",
                uid: "9f2b1c7e-1234-4cde-9abc-def012345678",
            }),
            ..OsArtifactArgs::new(&source)
        }
        .render();
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
            &BuildArtifactKind::Iso,
            None,
        ));
        assert!(!spec_matches(
            &data,
            "quay.io/a/c@sha256:y",
            "amd64",
            &BuildArtifactKind::Iso,
            None,
        ));
        assert!(!spec_matches(
            &data,
            "quay.io/a/b@sha256:x",
            "arm64",
            &BuildArtifactKind::Iso,
            None,
        ));
        // Same ref+arch but the requested kind changed (iso live, cloudImage
        // wanted) → must NOT match, forcing a rebuild.
        assert!(!spec_matches(
            &data,
            "quay.io/a/b@sha256:x",
            "amd64",
            &BuildArtifactKind::CloudImage,
            None,
        ));
    }

    #[test]
    fn spec_matches_detects_trusted_boot_drift() {
        use banlieue_api::banlieue::TrustedBootSource;
        use banlieue_api::common::LocalObjectReference;

        let plain_iso = serde_json::json!({
            "spec": {
                "image": { "ref": "quay.io/a/b@sha256:x" },
                "artifacts": { "arch": "amd64", "iso": true }
            }
        });
        let uki_iso = serde_json::json!({
            "spec": {
                "image": { "ref": "quay.io/a/b@sha256:x" },
                "artifacts": {
                    "arch": "amd64",
                    "uki": { "iso": true, "keysVolume": "trusted-boot-keys" }
                }
            }
        });
        let trusted_boot = TrustedBootSource {
            secret_ref: LocalObjectReference {
                name: "kairos-trusted-boot-keys".to_string(),
            },
        };

        // Live artifact is plain iso, but trustedBoot is now requested ->
        // stale, must rebuild.
        assert!(!spec_matches(
            &plain_iso,
            "quay.io/a/b@sha256:x",
            "amd64",
            &BuildArtifactKind::Iso,
            Some(&trusted_boot),
        ));
        // Live artifact is already uki.iso and trustedBoot is requested ->
        // current, no rebuild needed.
        assert!(spec_matches(
            &uki_iso,
            "quay.io/a/b@sha256:x",
            "amd64",
            &BuildArtifactKind::Iso,
            Some(&trusted_boot),
        ));
        // Live artifact is uki.iso but trustedBoot was removed -> stale,
        // must rebuild back to plain iso.
        assert!(!spec_matches(
            &uki_iso,
            "quay.io/a/b@sha256:x",
            "amd64",
            &BuildArtifactKind::Iso,
            None,
        ));
    }

    // ----------------------------------------------------------------------
    // SEC-004: checksum threading
    // ----------------------------------------------------------------------

    /// ADR-0010 (amendment 2026-11-26): the checksum is measured once the
    /// build is Ready (`digest.rs`), never copied from the source; a status
    /// computed from the `OSArtifact` alone carries none.
    #[test]
    fn compute_status_never_carries_a_checksum() {
        for phase in [None, Some("Building"), Some("Ready")] {
            let view = KairosArtifactStatusView {
                phase: phase.map(str::to_string),
                message: None,
            };
            let s = compute_build_artifact_status(
                "x-build",
                BuildArtifactKind::Iso,
                &view,
                None,
                false,
            );
            assert!(s.checksum.is_none(), "{phase:?}");
        }
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
            Some("11111111-2222-3333-4444-555555555555"),
            false,
        );
        assert_eq!(
            s.os_artifact_uid.as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
    }

    #[test]
    fn compute_status_without_a_known_uid_publishes_none() {
        let view = KairosArtifactStatusView::default();
        let s =
            compute_build_artifact_status("x-build", BuildArtifactKind::Iso, &view, None, false);
        assert!(s.os_artifact_uid.is_none());
    }

    fn live(owned: bool, current: bool, terminating: bool) -> Option<LiveArtifactView> {
        Some(LiveArtifactView {
            owned,
            current,
            terminating,
            volume_lost: false,
        })
    }

    // ----- ADR-0115: a volume whose node was replaced is a stale artifact ---

    fn time(rfc3339: &str) -> k8s_openapi::apimachinery::pkg::apis::meta::v1::Time {
        serde_json::from_value(json!(rfc3339)).expect("valid RFC 3339 time")
    }

    fn pv_with_affinity(terms: serde_json::Value) -> k8s_openapi::api::core::v1::PersistentVolume {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "PersistentVolume",
            "metadata": {"name": "pvc-x"},
            "spec": {"nodeAffinity": {"required": {"nodeSelectorTerms": terms}}},
        }))
        .expect("valid PV")
    }

    fn hostname_term(op: &str, values: &[&str]) -> serde_json::Value {
        json!({"matchExpressions": [
            {"key": "kubernetes.io/hostname", "operator": op, "values": values}
        ]})
    }

    #[test]
    fn a_pv_pinned_to_one_hostname_names_it() {
        let pv = pv_with_affinity(json!([hostname_term("In", &["worker-a.foo.io"])]));
        assert_eq!(pinned_hostname(&pv).as_deref(), Some("worker-a.foo.io"));
    }

    #[test]
    fn a_pv_without_node_affinity_names_no_host() {
        let pv: k8s_openapi::api::core::v1::PersistentVolume = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "PersistentVolume",
            "metadata": {"name": "pvc-x"}, "spec": {},
        }))
        .unwrap();
        assert_eq!(pinned_hostname(&pv), None);
    }

    #[test]
    fn a_pv_pinned_on_another_label_names_no_host() {
        let pv = pv_with_affinity(json!([{"matchExpressions": [
            {"key": "topology.kubernetes.io/zone", "operator": "In", "values": ["z1"]}
        ]}]));
        assert_eq!(pinned_hostname(&pv), None);
    }

    /// `NotIn` and `Exists` do not pin to a host; guessing one would make the
    /// gate compare the wrong node.
    #[test]
    fn a_pv_with_a_non_in_operator_names_no_host() {
        let pv = pv_with_affinity(json!([hostname_term("NotIn", &["worker-a.foo.io"])]));
        assert_eq!(pinned_hostname(&pv), None);
    }

    /// More than one host is ambiguous, so the check never guesses.
    #[test]
    fn a_pv_pinned_to_several_hostnames_names_no_host() {
        let pv = pv_with_affinity(json!([hostname_term(
            "In",
            &["worker-a.foo.io", "worker-b.foo.io"]
        )]));
        assert_eq!(pinned_hostname(&pv), None);
    }

    #[test]
    fn a_volume_is_lost_when_its_node_is_gone() {
        assert!(volume_lost(&time("2026-11-01T10:00:00Z"), &[]));
    }

    #[test]
    fn a_volume_is_lost_when_its_node_was_recreated_after_it() {
        let pv = time("2026-11-01T10:00:00Z");
        assert!(volume_lost(&pv, &[time("2026-11-04T14:40:33Z")]));
    }

    #[test]
    fn a_volume_survives_when_its_node_is_older() {
        let pv = time("2026-11-04T10:00:00Z");
        assert!(!volume_lost(&pv, &[time("2026-11-01T14:40:33Z")]));
    }

    /// Equal timestamps (a node and a PV made in the same second) must not
    /// read as a replacement: only strictly newer does.
    #[test]
    fn a_volume_survives_when_node_and_pv_share_a_timestamp() {
        let pv = time("2026-11-04T10:00:00Z");
        assert!(!volume_lost(&pv, &[time("2026-11-04T10:00:00Z")]));
    }

    #[test]
    fn a_volume_survives_if_any_matching_node_predates_it() {
        let pv = time("2026-11-04T10:00:00Z");
        let nodes = [time("2026-11-04T14:00:00Z"), time("2026-10-01T00:00:00Z")];
        assert!(!volume_lost(&pv, &nodes));
    }

    #[test]
    fn gate_deletes_an_artifact_whose_volume_was_lost() {
        let view = Some(LiveArtifactView {
            owned: true,
            current: true,
            terminating: false,
            volume_lost: true,
        });
        assert_eq!(artifact_gate(view, false), ArtifactGate::DeleteForRebuild);
    }

    /// Teardown already in flight wins: never delete twice.
    #[test]
    fn gate_waits_on_a_terminating_artifact_even_if_its_volume_was_lost() {
        let view = Some(LiveArtifactView {
            owned: true,
            current: true,
            terminating: true,
            volume_lost: true,
        });
        assert_eq!(artifact_gate(view, false), ArtifactGate::AwaitTeardown);
    }

    #[test]
    fn gate_proceeds_when_nothing_exists() {
        assert_eq!(artifact_gate(None, false), ArtifactGate::Proceed);
    }

    #[test]
    fn gate_proceeds_for_a_live_owned_current_artifact() {
        assert_eq!(
            artifact_gate(live(true, true, false), false),
            ArtifactGate::Proceed
        );
    }

    #[test]
    fn gate_deletes_a_stale_artifact() {
        assert_eq!(
            artifact_gate(live(true, false, false), false),
            ArtifactGate::DeleteForRebuild
        );
    }

    #[test]
    fn gate_deletes_a_foreign_artifact() {
        assert_eq!(
            artifact_gate(live(false, true, false), false),
            ArtifactGate::DeleteForRebuild
        );
    }

    /// The race: a stale artifact already being torn down must NOT be
    /// deleted again, and must NOT be SSA-applied over — recreating the
    /// same name adopts the dying PVC, which GC then removes.
    #[test]
    fn gate_waits_while_the_old_artifact_is_terminating() {
        for (owned, current) in [(true, true), (true, false), (false, true), (false, false)] {
            assert_eq!(
                artifact_gate(live(owned, current, true), false),
                ArtifactGate::AwaitTeardown,
                "owned={owned} current={current}"
            );
        }
    }

    /// The artifact object is gone but its PVC is still being finalised:
    /// a new artifact created now would adopt it.
    #[test]
    fn gate_waits_while_the_old_pvc_is_terminating() {
        assert_eq!(artifact_gate(None, true), ArtifactGate::AwaitTeardown);
    }

    #[test]
    fn delete_params_use_foreground_propagation() {
        let params = rebuild_delete_params();
        assert_eq!(
            params.propagation_policy,
            Some(kube::api::PropagationPolicy::Foreground)
        );
    }

    // ---- merge_with_sealing (ADR-0088) -----------------------------------

    const OPERATOR: &str = "users:\n  - name: kairos\n";

    #[test]
    fn without_sealing_the_merge_is_exactly_what_it_was() {
        let merged = merge_with_sealing(&[OPERATOR], None).unwrap();
        assert_eq!(
            merged,
            MergedCloudConfig::Ready {
                yaml: crate::cloud_config_merge::merge_cloud_configs(&[OPERATOR]).unwrap(),
                sealing_layer: false,
            }
        );
    }

    #[test]
    fn with_sealing_the_layer_is_merged_last_and_reported() {
        let sealing = banlieue_api::common::FirstBootSealing::default();
        let req = SealingRequest {
            sealing: &sealing,
            kind: &BuildArtifactKind::Iso,
            install_mode: banlieue_api::banlieue::InstallMode::Immediate,
        };
        let MergedCloudConfig::Ready {
            yaml,
            sealing_layer,
        } = merge_with_sealing(&[OPERATOR], Some(req)).unwrap()
        else {
            panic!("expected a merged cloud-config");
        };
        assert!(sealing_layer);
        assert!(yaml.starts_with("#cloud-config\n"), "yip needs the header");
        assert!(yaml.contains("name: kairos"), "operator content survives");
        assert!(yaml.contains(crate::first_boot_sealing::POLICY_FILE));
    }

    /// The layer alone is a cloud-config: an image with no operator layers
    /// still needs a merged Secret once it declares sealing.
    #[test]
    fn sealing_without_operator_layers_still_produces_a_cloud_config() {
        let sealing = banlieue_api::common::FirstBootSealing::default();
        let req = SealingRequest {
            sealing: &sealing,
            kind: &BuildArtifactKind::Iso,
            install_mode: banlieue_api::banlieue::InstallMode::Immediate,
        };
        let MergedCloudConfig::Ready {
            yaml,
            sealing_layer,
        } = merge_with_sealing(&[], Some(req)).unwrap()
        else {
            panic!("expected a merged cloud-config");
        };
        assert!(sealing_layer);
        assert!(yaml.contains(crate::first_boot_sealing::REPORT_STEP_NAME));
    }

    #[test]
    fn a_conflicting_operator_layer_refuses_the_build_instead_of_erroring() {
        let sealing = banlieue_api::common::FirstBootSealing::default();
        let req = SealingRequest {
            sealing: &sealing,
            kind: &BuildArtifactKind::Iso,
            install_mode: banlieue_api::banlieue::InstallMode::Immediate,
        };
        let operator = "install:\n  encrypted_partitions:\n    - COS_PERSISTENT\n";
        match merge_with_sealing(&[operator], Some(req)).unwrap() {
            MergedCloudConfig::SealingRefused(e) => {
                assert_eq!(
                    e.reason(),
                    crate::first_boot_sealing::REASON_FIRST_BOOT_SEALING_CONFLICT
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn sealing_refused_status_is_a_failed_build_naming_the_reason() {
        let err = crate::first_boot_sealing::SealingLayerError::Conflict("why".to_string());
        let s = sealing_refused_status("img-build", BuildArtifactKind::Iso, &err);
        assert_eq!(s.phase, BuildArtifactPhase::Failed);
        assert_eq!(
            s.reason.as_deref(),
            Some(crate::first_boot_sealing::REASON_FIRST_BOOT_SEALING_CONFLICT)
        );
        assert_eq!(s.message.as_deref(), Some("why"));
        assert!(!s.first_boot_sealing_layer);
        assert_eq!(
            s.checksum, None,
            "nothing was built, so nothing was measured"
        );
    }

    // ---- the Azure guest layer and helper bundle (ADR-0171) ---------------

    const HELPER: &str = "registry.internal:5000/banlieue-azure-guest@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn sealing_req(sealing: &banlieue_api::common::FirstBootSealing) -> SealingRequest<'_> {
        SealingRequest {
            sealing,
            kind: &BuildArtifactKind::CloudImage,
            install_mode: banlieue_api::banlieue::InstallMode::Immediate,
        }
    }

    fn ready_yaml(m: MergedCloudConfig) -> (String, bool) {
        let MergedCloudConfig::Ready {
            yaml,
            sealing_layer,
        } = m
        else {
            panic!("expected a merged cloud-config, got {m:?}");
        };
        (yaml, sealing_layer)
    }

    /// The names of a stage's steps, in order.
    fn step_names(yaml: &str, stage: &str) -> Vec<String> {
        let doc: Value = serde_saphyr::from_str(yaml).expect("YAML");
        doc["stages"][stage]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Byte for byte: an image without an azure source merges exactly as
    /// before ADR-0171, with or without operator layers or sealing.
    #[test]
    fn without_an_azure_source_the_merge_is_byte_identical() {
        let sealing = banlieue_api::common::FirstBootSealing::default();
        for docs in [&[][..], &[OPERATOR][..]] {
            assert_eq!(
                merge_build_layers(docs, None, false).unwrap(),
                merge_with_sealing(docs, None).unwrap()
            );
            assert_eq!(
                merge_build_layers(docs, Some(sealing_req(&sealing)), false).unwrap(),
                merge_with_sealing(docs, Some(sealing_req(&sealing))).unwrap()
            );
        }
    }

    #[test]
    fn with_an_azure_source_the_layer_alone_is_a_cloud_config() {
        let (yaml, sealing_layer) = ready_yaml(merge_build_layers(&[], None, true).unwrap());
        assert!(!sealing_layer);
        assert!(yaml.starts_with("#cloud-config\n"), "yip needs the header");
        assert!(yaml.contains(crate::azure_guest::HELPER_PATH));
    }

    /// Lists concatenate in merge order, so the Azure steps come after every
    /// operator step and after the sealing layer's report step in `boot`.
    #[test]
    fn the_azure_layer_is_merged_last() {
        let operator = "stages:\n  boot:\n    - name: operator-step\n      commands: [\"true\"]\n  initramfs:\n    - name: operator-initramfs\n";
        let sealing = banlieue_api::common::FirstBootSealing::default();
        let (yaml, sealing_layer) = ready_yaml(
            merge_build_layers(&[OPERATOR, operator], Some(sealing_req(&sealing)), true).unwrap(),
        );
        assert!(sealing_layer);
        assert!(yaml.contains("name: kairos"), "operator content survives");
        assert_eq!(
            step_names(&yaml, "boot"),
            [
                "operator-step",
                crate::first_boot_sealing::REPORT_STEP_NAME,
                crate::azure_guest::STEP_CUSTOM_DATA,
            ]
        );
        let initramfs = step_names(&yaml, "initramfs");
        assert_eq!(
            initramfs.first().map(String::as_str),
            Some("operator-initramfs")
        );
        assert_eq!(
            initramfs.last().map(String::as_str),
            Some(crate::azure_guest::STEP_READY_UNIT)
        );
    }

    #[test]
    fn a_sealing_refusal_still_refuses_with_an_azure_source() {
        let sealing = banlieue_api::common::FirstBootSealing::default();
        let req = SealingRequest {
            sealing: &sealing,
            kind: &BuildArtifactKind::Iso,
            install_mode: banlieue_api::banlieue::InstallMode::Immediate,
        };
        let operator = "install:\n  encrypted_partitions:\n    - COS_PERSISTENT\n";
        assert!(matches!(
            merge_build_layers(&[operator], Some(req), true).unwrap(),
            MergedCloudConfig::SealingRefused(_)
        ));
    }

    #[test]
    fn bundles_carry_the_helper_only_for_an_azure_image() {
        assert_eq!(build_bundles(true, Some(HELPER)), [HELPER.to_string()]);
        assert!(build_bundles(false, Some(HELPER)).is_empty());
        assert!(build_bundles(true, None).is_empty());
    }

    /// An image without an azure source renders the same OSArtifact as
    /// before ADR-0171: no `bundles` key at all.
    #[test]
    fn no_bundles_leaves_the_os_artifact_unchanged() {
        let source = libvirt_url_source("quay.io/kairos/x@sha256:abc");
        let mut args = OsArtifactArgs::new(&source);
        args.kind = BuildArtifactKind::CloudImage;
        let before = args.render();
        let mut after = before.clone();
        with_bundles(&mut after, &[]);
        assert_eq!(before, after);
        assert!(bundles_match(&after, &[]));
    }

    #[test]
    fn bundles_are_added_to_the_artifacts_spec() {
        let source = libvirt_url_source("quay.io/kairos/x@sha256:abc");
        let mut args = OsArtifactArgs::new(&source);
        args.kind = BuildArtifactKind::CloudImage;
        let mut desired = args.render();
        let bundles = build_bundles(true, Some(HELPER));
        with_bundles(&mut desired, &bundles);
        assert_eq!(desired["spec"]["artifacts"]["bundles"], json!([HELPER]));
        assert_eq!(desired["spec"]["artifacts"]["cloudImage"], json!(true));
        assert!(bundles_match(&desired, &bundles));
    }

    /// Adding or removing an azure source, or a new helper digest, changes
    /// what is in the image, so the live OSArtifact no longer matches and is
    /// rebuilt.
    #[test]
    fn a_change_of_bundles_is_judged_stale() {
        let with = json!({"spec": {"artifacts": {"bundles": [HELPER]}}});
        let without = json!({"spec": {"artifacts": {"cloudImage": true}}});
        let other = json!({"spec": {"artifacts": {"bundles": ["r@sha256:00"]}}});
        let helper = build_bundles(true, Some(HELPER));
        assert!(bundles_match(&with, &helper));
        assert!(!bundles_match(&without, &helper));
        assert!(!bundles_match(&with, &[]));
        assert!(!bundles_match(&other, &helper));
        assert!(bundles_match(&without, &[]));
    }

    #[test]
    fn azure_guest_refused_status_is_a_failed_build_naming_the_flag() {
        let s = azure_guest_refused_status("img-build", BuildArtifactKind::CloudImage);
        assert_eq!(s.phase, BuildArtifactPhase::Failed);
        assert_eq!(s.reason.as_deref(), Some(REASON_AZURE_GUEST_IMAGE_UNSET));
        assert!(
            s.message
                .as_deref()
                .is_some_and(|m| m.contains("--azure-guest-image"))
        );
        assert_eq!(s.checksum, None);
    }
}
