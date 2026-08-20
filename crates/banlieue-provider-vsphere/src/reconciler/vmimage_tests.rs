// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::vmimage`].
//!
//! These tests target the pure helpers and `compute_template_status` (which
//! takes `&dyn VSphereClient`, so `FakeClient` drives it without contacting
//! kube or vCenter).

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use banlieue_api::banlieue::{
        Architecture, BuildArtifactKind, BuildArtifactPhase, BuildArtifactStatus, DiskController,
        FailureDomain, FailureDomainAttributes, GuestAgent, ImageSource, ImageSourceKind,
        NicAdapter, OsFamily, Provider, ProviderConnection, ProviderSpec, ProviderStatus, VMImage,
        VMImageSpec, VMImageTemplate, VMImageTemplateDisk, VMImageTemplateNic,
    };
    use banlieue_api::common::{DiskProvisioning, Firmware, LocalObjectReference};
    use kube::api::ObjectMeta;

    use crate::client::{Datacenter, FakeClient, Inventory, VSphereClient};

    use super::super::{
        ImportForce, ImportJobIdentity, ImportJobInputs, build_import_job, compute_template_status,
        find_vsphere_source, gate_on_build_artifact, import_job_name, reasons, zone_from_job,
    };

    fn job_name(image: &str, provider: &str, failure_domain: &str) -> String {
        import_job_name(&ImportJobIdentity {
            image,
            provider,
            failure_domain,
        })
    }

    fn dc(name: &str) -> Datacenter {
        Datacenter {
            name: name.to_string(),
            moref: format!("datacenter-{name}"),
        }
    }

    fn provider(name: &str, namespace: &str) -> Provider {
        let mut raw = BTreeMap::new();
        raw.insert("datacenter".to_string(), "dc-east".to_string());
        raw.insert("cluster".to_string(), "cluster-a".to_string());

        Provider {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                ..Default::default()
            },
            spec: ProviderSpec {
                provider_class_ref: LocalObjectReference {
                    name: "vsphere".to_string(),
                },
                connection: ProviderConnection {
                    endpoint: "https://vc".to_string(),
                    credentials_ref: LocalObjectReference {
                        name: "creds".to_string(),
                    },
                    insecure_skip_tls_verify: true,
                    ca_bundle: None,
                },
                capabilities: Default::default(),
                paused: false,
                use_content_library: false,
                failure_domain_name_overrides: Vec::new(),
            },
            status: Some(ProviderStatus {
                failure_domains: vec![FailureDomain {
                    name: format!("{name}-dc-east-cluster-a"),
                    labels: Default::default(),
                    attributes: FailureDomainAttributes {
                        raw,
                        ..Default::default()
                    },
                }],
                conditions: vec![],
                workload: None,
                observed_generation: Some(1),
            }),
        }
    }

    fn vsphere_image_source(template_name: &str) -> ImageSource {
        ImageSource {
            provider_class: "vsphere".to_string(),
            kind: ImageSourceKind::Template,
            reference: template_name.to_string(),
            import_from: None,
            checksum: None,
        }
    }

    fn fake_client_with(template: Option<(&str, &str)>) -> FakeClient {
        let mut builder = Inventory::builder().with_dc("dc-east");
        if let Some((dc_name, tname)) = template {
            builder = builder.with_template(dc_name, tname);
        }
        FakeClient::new(builder.build())
    }

    fn as_client(c: &FakeClient) -> &dyn VSphereClient {
        c
    }

    // ---------- find_vsphere_source --------------------------------------

    #[test]
    fn find_vsphere_source_picks_first_vsphere_template() {
        let sources = vec![
            ImageSource {
                provider_class: "proxmox".to_string(),
                kind: ImageSourceKind::Template,
                reference: "9000".to_string(),
                import_from: None,
                checksum: None,
            },
            vsphere_image_source("ubuntu-22.04"),
        ];
        let picked = find_vsphere_source(&sources).unwrap();
        assert_eq!(picked.reference, "ubuntu-22.04");
    }

    #[test]
    fn find_vsphere_source_also_picks_url_sources() {
        // Url sources (banlieue-imagebuilder pipeline, ADR-0010) are now a
        // supported vsphere source kind, not skipped.
        let sources = vec![ImageSource {
            provider_class: "vsphere".to_string(),
            kind: ImageSourceKind::Url,
            reference: String::new(),
            import_from: Some("quay.io/kairos/ubuntu:24.04".to_string()),
            checksum: None,
        }];
        let picked = find_vsphere_source(&sources).unwrap();
        assert_eq!(picked.kind, ImageSourceKind::Url);
    }

    #[test]
    fn find_vsphere_source_returns_none_for_backing_file_only() {
        // BackingFile is a libvirt-shaped concept; vsphere never declares one.
        let sources = vec![ImageSource {
            provider_class: "vsphere".to_string(),
            kind: ImageSourceKind::BackingFile,
            reference: "/var/lib/libvirt/ubuntu.qcow2".to_string(),
            import_from: None,
            checksum: None,
        }];
        assert!(find_vsphere_source(&sources).is_none());
    }

    #[test]
    fn find_vsphere_source_returns_none_for_other_provider_classes() {
        let sources = vec![ImageSource {
            provider_class: "libvirt".to_string(),
            kind: ImageSourceKind::Template,
            reference: "/var/lib/libvirt/ubuntu.qcow2".to_string(),
            import_from: None,
            checksum: None,
        }];
        assert!(find_vsphere_source(&sources).is_none());
    }

    // ---------- compute_template_status ----------------------------------

    #[tokio::test]
    async fn compute_template_status_returns_ready_when_template_present() {
        let client = fake_client_with(Some(("dc-east", "ubuntu-22.04")));
        let dcs = vec![dc("dc-east")];
        let row = compute_template_status(
            as_client(&client),
            &dcs,
            "ubuntu-22.04",
            &provider("prov-east", "banlieue"),
        )
        .await;
        assert!(row.ready);
        assert_eq!(row.reason.as_deref(), Some(reasons::RECONCILED));
        assert_eq!(row.provider_name, "prov-east");
        assert_eq!(row.provider_namespace, "banlieue");
        assert_eq!(
            row.resolved_ref.as_deref(),
            Some("[dc-east] ubuntu-22.04"),
            "resolved_ref should follow vSphere [datacenter] template-name convention"
        );
    }

    #[tokio::test]
    async fn compute_template_status_returns_not_found_when_template_absent() {
        let client = fake_client_with(None); // DC seeded but no template
        let dcs = vec![dc("dc-east")];
        let row = compute_template_status(
            as_client(&client),
            &dcs,
            "ubuntu-22.04",
            &provider("p", "ns"),
        )
        .await;
        assert!(!row.ready);
        assert_eq!(row.reason.as_deref(), Some(reasons::TEMPLATE_NOT_FOUND));
        assert!(row.message.as_deref().unwrap().contains("ubuntu-22.04"));
    }

    #[tokio::test]
    async fn compute_template_status_returns_not_found_with_no_datacenters() {
        // Defensive: if for some reason no DCs are passed in (e.g. Provider
        // status went stale and live walk is empty too), don't claim ready.
        let client = fake_client_with(Some(("dc-east", "ubuntu-22.04")));
        let row = compute_template_status(
            as_client(&client),
            &[],
            "ubuntu-22.04",
            &provider("p", "ns"),
        )
        .await;
        assert!(!row.ready);
        assert_eq!(row.reason.as_deref(), Some(reasons::TEMPLATE_NOT_FOUND));
        assert!(
            row.message.as_deref().unwrap().contains("no datacenters"),
            "message should explain why: {:?}",
            row.message
        );
    }

    // ---------- gate_on_build_artifact (Url source, ADR-0020) -------------

    fn iso_artifact(phase: BuildArtifactPhase) -> BuildArtifactStatus {
        BuildArtifactStatus {
            kind: BuildArtifactKind::Iso,
            phase,
            os_artifact_ref: "img-build".to_string(),
            os_artifact_uid: None,
            pvc_ref: Some(LocalObjectReference {
                name: "img-build-artifacts".to_string(),
            }),
            file: Some("img-build.iso".to_string()),
            reason: None,
            message: None,
            checksum: None,
        }
    }

    #[test]
    fn gate_no_build_artifact_yet_is_build_pending() {
        let (reason, _msg) = gate_on_build_artifact(None).unwrap_err();
        assert_eq!(reason, reasons::BUILD_PENDING);
    }

    #[test]
    fn gate_pending_or_building_is_build_pending() {
        for phase in [BuildArtifactPhase::Pending, BuildArtifactPhase::Building] {
            let a = iso_artifact(phase);
            let (reason, _) = gate_on_build_artifact(Some(&a)).unwrap_err();
            assert_eq!(reason, reasons::BUILD_PENDING);
        }
    }

    #[test]
    fn gate_failed_surfaces_build_failed_with_message() {
        let mut a = iso_artifact(BuildArtifactPhase::Failed);
        a.message = Some("pull failed: manifest unknown".to_string());
        let (reason, msg) = gate_on_build_artifact(Some(&a)).unwrap_err();
        assert_eq!(reason, reasons::BUILD_FAILED);
        assert_eq!(msg, "pull failed: manifest unknown");
    }

    #[test]
    fn gate_ready_iso_is_ok() {
        let a = iso_artifact(BuildArtifactPhase::Ready);
        let got = gate_on_build_artifact(Some(&a)).expect("ready iso passes the gate");
        assert_eq!(got.file.as_deref(), Some("img-build.iso"));
    }

    #[test]
    fn gate_ready_but_cloud_image_is_wrong_kind() {
        // vSphere imports only ISOs; a raw cloudImage artifact must be rejected
        // rather than fed into the ISO import path.
        let mut a = iso_artifact(BuildArtifactPhase::Ready);
        a.kind = BuildArtifactKind::CloudImage;
        let (reason, _) = gate_on_build_artifact(Some(&a)).unwrap_err();
        assert_eq!(reason, reasons::WRONG_ARTIFACT_KIND);
    }

    // ---------- import Job planning ---------------------------------------

    #[test]
    fn import_job_name_is_deterministic_and_dns_safe() {
        let n = job_name(
            "kairos-rhel98",
            "vcenter-ssc",
            "vcenter-ssc-dc-east-cluster-a",
        );
        assert_eq!(
            n,
            "import-kairos-rhel98-vcenter-ssc-vcenter-ssc-dc-east-cluster-a"
        );
        // Long inputs stay within the 63-char Kubernetes name cap.
        let long = job_name(&"x".repeat(40), &"y".repeat(40), &"z".repeat(40));
        assert!(long.len() <= 63);
        assert!(long.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn import_job_name_does_not_collide_across_long_failure_domains_sharing_a_prefix() {
        // Found live: real vCenter failure-domain names (datacenter + cluster,
        // hyphenated) routinely exceed 63 chars and share everything except a
        // short trailing hash suffix — naive `.take(63)` truncated all three
        // zones below to the identical name, so only one of three per-zone
        // import Jobs ever actually got created. Names below are synthetic
        // placeholders shaped like the real ones, not the real ones.
        let image = "example-kairos-0.1.0";
        let provider = "vcenter-example";
        let a = job_name(
            image,
            provider,
            "vcenter-example-dc-alpha-cluster-nonreplicated-storage-pool-aaaa1111",
        );
        let b = job_name(
            image,
            provider,
            "vcenter-example-dc-alpha-cluster-nonreplicated-storage-pool-bbbb2222",
        );
        let c = job_name(
            image,
            provider,
            "vcenter-example-dc-alpha-cluster-nonreplicated-storage-pool-cccc3333",
        );
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
        for n in [&a, &b, &c] {
            assert!(n.len() <= 63, "{n} exceeds 63 chars");
            assert!(n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
            assert!(!n.starts_with('-'));
            assert!(!n.ends_with('-'));
        }
    }

    #[test]
    fn import_job_name_is_stable_for_the_same_inputs() {
        let a = job_name("img", "prov", &"z".repeat(80));
        let b = job_name("img", "prov", &"z".repeat(80));
        assert_eq!(a, b);
    }

    #[test]
    fn build_import_job_runs_the_image_import_subcommand_with_pvc_mounted() {
        let p = provider("vc", "banlieue-system");
        let artifact = iso_artifact(BuildArtifactPhase::Ready);
        let job = build_import_job(&ImportJobInputs {
            job_name: "import-x",
            namespace: "banlieue-imagebuild",
            image: "ghcr.io/firestoned/banlieue:local-dev",
            service_account: Some("banlieue-import"),
            vmimage: "kairos-rhel98",
            provider: &p,
            failure_domain: "vc-dc-east-cluster-a",
            artifact: &artifact,
            tolerations: &[],
            force_upload: false,
            force_create: false,
            nics: &[],
            disk: None,
            cpus: None,
            memory_mib: None,
            firmware: None,
            guest_id: None,
            root_folder: None,
            install_timeout_seconds: None,
            auto_manage_install: None,
        });

        assert_eq!(job["kind"], "Job");
        assert_eq!(job["apiVersion"], "batch/v1");
        assert_eq!(job["metadata"]["namespace"], "banlieue-imagebuild");
        let args = job["spec"]["template"]["spec"]["containers"][0]["args"]
            .as_array()
            .unwrap();
        let args: Vec<&str> = args.iter().map(|a| a.as_str().unwrap()).collect();
        assert!(args.starts_with(&["provider", "vsphere", "image-import"]));
        assert!(args.contains(&"--failure-domain"));
        assert!(args.contains(&"vc-dc-east-cluster-a"));
        assert!(args.contains(&"/artifacts/img-build.iso"));
        // Artifacts PVC mounted read-only at /artifacts.
        assert_eq!(
            job["spec"]["template"]["spec"]["volumes"][0]["persistentVolumeClaim"]["claimName"],
            "img-build-artifacts"
        );
        assert_eq!(
            job["spec"]["template"]["spec"]["serviceAccountName"],
            "banlieue-import"
        );
    }

    // ------------------------------------------------------------------
    // ADR-0027: import Job owned by the OSArtifact whose PVC it mounts
    // ------------------------------------------------------------------

    #[test]
    fn build_import_job_has_no_owner_reference_when_os_artifact_uid_unknown() {
        let p = provider("vc", "banlieue-system");
        let artifact = iso_artifact(BuildArtifactPhase::Ready);
        assert!(artifact.os_artifact_uid.is_none());
        let job = build_import_job(&ImportJobInputs {
            job_name: "import-x",
            namespace: "banlieue-imagebuild",
            image: "img",
            service_account: None,
            vmimage: "kairos-rhel98",
            provider: &p,
            failure_domain: "vc-dc-east-cluster-a",
            artifact: &artifact,
            tolerations: &[],
            force_upload: false,
            force_create: false,
            nics: &[],
            disk: None,
            cpus: None,
            memory_mib: None,
            firmware: None,
            guest_id: None,
            root_folder: None,
            install_timeout_seconds: None,
            auto_manage_install: None,
        });
        assert!(job["metadata"]["ownerReferences"].is_null());
    }

    #[test]
    fn build_import_job_is_owned_by_the_os_artifact_when_uid_known() {
        let p = provider("vc", "banlieue-system");
        let artifact = BuildArtifactStatus {
            os_artifact_uid: Some("11111111-2222-3333-4444-555555555555".to_string()),
            ..iso_artifact(BuildArtifactPhase::Ready)
        };
        let job = build_import_job(&ImportJobInputs {
            job_name: "import-x",
            namespace: "banlieue-imagebuild",
            image: "img",
            service_account: None,
            vmimage: "kairos-rhel98",
            provider: &p,
            failure_domain: "vc-dc-east-cluster-a",
            artifact: &artifact,
            tolerations: &[],
            force_upload: false,
            force_create: false,
            nics: &[],
            disk: None,
            cpus: None,
            memory_mib: None,
            firmware: None,
            guest_id: None,
            root_folder: None,
            install_timeout_seconds: None,
            auto_manage_install: None,
        });
        let owner = &job["metadata"]["ownerReferences"][0];
        assert_eq!(owner["apiVersion"], "build.kairos.io/v1alpha2");
        assert_eq!(owner["kind"], "OSArtifact");
        assert_eq!(owner["name"], "img-build");
        assert_eq!(owner["uid"], "11111111-2222-3333-4444-555555555555");
    }

    #[test]
    fn build_import_job_threads_the_checksum_when_present() {
        let p = provider("vc", "banlieue-system");
        let mut artifact = iso_artifact(BuildArtifactPhase::Ready);
        artifact.checksum = Some("sha256:abc".to_string());
        let job = build_import_job(&ImportJobInputs {
            job_name: "import-x",
            namespace: "banlieue-imagebuild",
            image: "img",
            service_account: None,
            vmimage: "kairos-rhel98",
            provider: &p,
            failure_domain: "vc-dc-east-cluster-a",
            artifact: &artifact,
            tolerations: &[],
            force_upload: false,
            force_create: false,
            nics: &[],
            disk: None,
            cpus: None,
            memory_mib: None,
            firmware: None,
            guest_id: None,
            root_folder: None,
            install_timeout_seconds: None,
            auto_manage_install: None,
        });
        let args: Vec<String> = job["spec"]["template"]["spec"]["containers"][0]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        assert!(args.iter().any(|a| a == "--checksum"));
        assert!(args.iter().any(|a| a == "sha256:abc"));
    }

    #[test]
    fn build_import_job_emits_force_flags_independently() {
        let p = provider("vc", "banlieue-system");
        let artifact = iso_artifact(BuildArtifactPhase::Ready);
        let args_of = |upload: bool, create: bool| -> Vec<String> {
            let job = build_import_job(&ImportJobInputs {
                job_name: "import-x",
                namespace: "banlieue-imagebuild",
                image: "img",
                service_account: None,
                vmimage: "kairos-rhel98",
                provider: &p,
                failure_domain: "vc-dc-east-cluster-a",
                artifact: &artifact,
                tolerations: &[],
                force_upload: upload,
                force_create: create,
                nics: &[],
                disk: None,
                cpus: None,
                memory_mib: None,
                firmware: None,
                guest_id: None,
                root_folder: None,
                install_timeout_seconds: None,
                auto_manage_install: None,
            });
            job["spec"]["template"]["spec"]["containers"][0]["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap().to_string())
                .collect()
        };
        let has = |a: &[String], f: &str| a.iter().any(|x| x == f);

        let none = args_of(false, false);
        assert!(!has(&none, "--force-upload") && !has(&none, "--force-create"));

        let up = args_of(true, false);
        assert!(has(&up, "--force-upload") && !has(&up, "--force-create"));

        let cr = args_of(false, true);
        assert!(!has(&cr, "--force-upload") && has(&cr, "--force-create"));

        let both = args_of(true, true);
        assert!(has(&both, "--force-upload") && has(&both, "--force-create"));
    }

    #[test]
    fn import_force_reads_the_full_template_off_the_image() {
        let mut img = VMImage {
            metadata: ObjectMeta {
                name: Some("kairos-rhel98".to_string()),
                ..Default::default()
            },
            spec: VMImageSpec {
                os_family: OsFamily::Linux,
                os_distribution: "rhel".to_string(),
                os_version: "9.8".to_string(),
                architecture: Architecture::Amd64,
                guest_agent: GuestAgent::CloudInit,
                sources: vec![vsphere_image_source("kairos-rhel98")],
                cloud_config: None,
                template: Some(VMImageTemplate {
                    root_folder: Some("templates/kairos".to_string()),
                    network: vec![VMImageTemplateNic {
                        network: Some("vmnet-prod".to_string()),
                        adapter: Some(NicAdapter::E1000),
                        pci_slot: Some(224),
                    }],
                    disk: Some(VMImageTemplateDisk {
                        size: Some(120),
                        provisioning: DiskProvisioning::Thick,
                        controller: DiskController::Pvscsi,
                    }),
                    cpus: Some(4),
                    memory_mib: Some(8192),
                    firmware: Some(Firmware::EfiSecure),
                    guest_id: Some("rhel9_64Guest".to_string()),
                    force_upload: true,
                    force_create: false,
                    install_timeout_seconds: Some(900),
                    auto_manage_install: Some(false),
                    retain_on_delete: false,
                }),
                iso_overlay: None,
            },
            status: None,
        };

        let force = ImportForce::from_image(&img);
        assert!(force.upload && !force.create);
        assert_eq!(force.nics.len(), 1);
        assert_eq!(force.nics[0].network.as_deref(), Some("vmnet-prod"));
        assert_eq!(force.nics[0].adapter, Some(NicAdapter::E1000));
        assert_eq!(force.nics[0].pci_slot, Some(224));
        assert_eq!(force.cpus, Some(4));
        assert_eq!(force.memory_mib, Some(8192));
        assert_eq!(force.firmware, Some(Firmware::EfiSecure));
        assert_eq!(force.guest_id.as_deref(), Some("rhel9_64Guest"));
        assert_eq!(force.root_folder.as_deref(), Some("templates/kairos"));
        assert_eq!(force.disk.as_ref().and_then(|d| d.size), Some(120));
        assert_eq!(force.install_timeout_seconds, Some(900));
        assert_eq!(force.auto_manage_install, Some(false));

        // No template → all knobs None, no force.
        img.spec.template = None;
        let empty = ImportForce::from_image(&img);
        assert!(!empty.upload && !empty.create);
        assert!(empty.nics.is_empty() && empty.disk.is_none() && empty.firmware.is_none());
        assert!(empty.install_timeout_seconds.is_none());
        assert!(empty.auto_manage_install.is_none());
    }

    #[test]
    fn build_import_job_threads_all_template_hardware_knobs() {
        let p = provider("vc", "banlieue-system");
        let artifact = iso_artifact(BuildArtifactPhase::Ready);
        let disk = VMImageTemplateDisk {
            size: Some(80),
            provisioning: DiskProvisioning::EagerZeroed,
            controller: DiskController::LsiLogicSas,
        };
        let efi_secure = Firmware::EfiSecure;
        let nics = vec![VMImageTemplateNic {
            network: Some("vmnet-prod".to_string()),
            adapter: Some(NicAdapter::E1000e),
            pci_slot: Some(256),
        }];
        let job = build_import_job(&ImportJobInputs {
            job_name: "import-x",
            namespace: "banlieue-imagebuild",
            image: "img",
            service_account: None,
            vmimage: "kairos-rhel98",
            provider: &p,
            failure_domain: "vc-dc-east-cluster-a",
            artifact: &artifact,
            tolerations: &[],
            force_upload: false,
            force_create: false,
            nics: &nics,
            disk: Some(&disk),
            cpus: Some(8),
            memory_mib: Some(16384),
            firmware: Some(&efi_secure),
            guest_id: Some("rhel9_64Guest"),
            root_folder: Some("templates/kairos"),
            install_timeout_seconds: Some(900),
            auto_manage_install: Some(false),
        });
        let args: Vec<String> = job["spec"]["template"]["spec"]["containers"][0]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        // Each knob is emitted as a flag/value pair the image-import CLI parses.
        let pair = |flag: &str, value: &str| args.windows(2).any(|w| w[0] == flag && w[1] == value);
        assert!(
            pair("--nic", "network=vmnet-prod,adapter=e1000e,pciSlot=256"),
            "{args:?}"
        );
        assert!(pair("--disk-gb", "80"), "{args:?}");
        assert!(pair("--disk-type", "eagerZeroed"), "{args:?}");
        assert!(pair("--disk-controller", "lsiLogicSas"), "{args:?}");
        assert!(pair("--cpus", "8"), "{args:?}");
        assert!(pair("--memory-mib", "16384"), "{args:?}");
        assert!(pair("--firmware", "efi-secure"), "{args:?}");
        assert!(pair("--guest-id", "rhel9_64Guest"), "{args:?}");
        assert!(pair("--root-folder", "templates/kairos"), "{args:?}");
        assert!(pair("--install-timeout-seconds", "900"), "{args:?}");
        assert!(pair("--auto-manage-install", "false"), "{args:?}");
    }

    #[test]
    fn build_import_job_emits_one_nic_flag_per_declared_nic() {
        // ADR-0031: a template with several NICs threads one --nic per
        // entry, not parallel repeated --network/--network-adapter flags.
        let p = provider("vc", "banlieue-system");
        let artifact = iso_artifact(BuildArtifactPhase::Ready);
        let nics = vec![
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
        ];
        let job = build_import_job(&ImportJobInputs {
            job_name: "import-x",
            namespace: "banlieue-imagebuild",
            image: "img",
            service_account: None,
            vmimage: "kairos-rhel98",
            provider: &p,
            failure_domain: "vc-dc-east-cluster-a",
            artifact: &artifact,
            tolerations: &[],
            force_upload: false,
            force_create: false,
            nics: &nics,
            disk: None,
            cpus: None,
            memory_mib: None,
            firmware: None,
            guest_id: None,
            root_folder: None,
            install_timeout_seconds: None,
            auto_manage_install: None,
        });
        let args: Vec<String> = job["spec"]["template"]["spec"]["containers"][0]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        let nic_values: Vec<&str> = args
            .windows(2)
            .filter(|w| w[0] == "--nic")
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(
            nic_values,
            vec![
                "network=vmnet-prod,adapter=vmxnet3,pciSlot=192",
                "network=vmnet-mgmt"
            ]
        );
    }

    #[test]
    fn build_import_job_omits_install_timeout_flag_when_unset() {
        let p = provider("vc", "banlieue-system");
        let artifact = iso_artifact(BuildArtifactPhase::Ready);
        let job = build_import_job(&ImportJobInputs {
            job_name: "import-x",
            namespace: "banlieue-imagebuild",
            image: "img",
            service_account: None,
            vmimage: "kairos-rhel98",
            provider: &p,
            failure_domain: "vc-dc-east-cluster-a",
            artifact: &artifact,
            tolerations: &[],
            force_upload: false,
            force_create: false,
            nics: &[],
            disk: None,
            cpus: None,
            memory_mib: None,
            firmware: None,
            guest_id: None,
            root_folder: None,
            install_timeout_seconds: None,
            auto_manage_install: None,
        });
        let args: Vec<String> = job["spec"]["template"]["spec"]["containers"][0]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        assert!(!args.iter().any(|a| a == "--install-timeout-seconds"));
        assert!(!args.iter().any(|a| a == "--auto-manage-install"));
    }

    #[test]
    fn force_reimport_requested_reads_the_annotation() {
        use super::super::force_reimport_requested;

        let mut img = VMImage {
            metadata: ObjectMeta {
                name: Some("kairos-rhel98".to_string()),
                ..Default::default()
            },
            spec: VMImageSpec {
                os_family: OsFamily::Linux,
                os_distribution: "rhel".to_string(),
                os_version: "9.8".to_string(),
                architecture: Architecture::Amd64,
                guest_agent: GuestAgent::CloudInit,
                sources: vec![vsphere_image_source("kairos-rhel98")],
                cloud_config: None,
                template: None,
                iso_overlay: None,
            },
            status: None,
        };
        assert!(!force_reimport_requested(&img));

        img.metadata.annotations = Some(
            [("banlieue.io/force-reimport".to_string(), "true".to_string())]
                .into_iter()
                .collect(),
        );
        assert!(force_reimport_requested(&img));

        img.metadata.annotations = Some(
            [(
                "banlieue.io/force-reimport".to_string(),
                "false".to_string(),
            )]
            .into_iter()
            .collect(),
        );
        assert!(!force_reimport_requested(&img));
    }

    #[test]
    fn clear_force_reimport_patch_nulls_only_that_annotation() {
        use super::super::clear_force_reimport_patch;

        let patch = clear_force_reimport_patch();
        assert_eq!(
            patch,
            serde_json::json!({
                "metadata": {
                    "annotations": {
                        "banlieue.io/force-reimport": null
                    }
                }
            }),
            "must be a JSON Merge Patch that deletes only this one annotation key"
        );
    }

    #[test]
    fn zone_from_job_translates_success_running_and_failure() {
        use k8s_openapi::api::batch::v1::{Job, JobStatus};

        let ready = Job {
            status: Some(JobStatus {
                succeeded: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };
        let z = zone_from_job(
            "fd-1",
            "kairos-hadron",
            "templates/fd-1",
            "import-x",
            &ready,
        );
        assert!(z.ready);
        assert_eq!(z.reason.as_deref(), Some(reasons::RECONCILED));
        // resolved_ref is the bare template name — NEVER the Job's own k8s
        // name — since that's what a name-based template lookup needs
        // (found live: using the Job name here made every clone fail with
        // "template not found").
        assert_eq!(z.resolved_ref.as_deref(), Some("kairos-hadron"));
        assert_eq!(z.template_folder.as_deref(), Some("templates/fd-1"));

        let failed = Job {
            status: Some(JobStatus {
                failed: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };
        let z = zone_from_job(
            "fd-1",
            "kairos-hadron",
            "templates/fd-1",
            "import-x",
            &failed,
        );
        assert!(!z.ready);
        assert_eq!(z.reason.as_deref(), Some(reasons::IMPORT_FAILED));

        let running = Job {
            status: Some(JobStatus::default()),
            ..Default::default()
        };
        let z = zone_from_job(
            "fd-1",
            "kairos-hadron",
            "templates/fd-1",
            "import-x",
            &running,
        );
        assert!(!z.ready);
        assert_eq!(z.reason.as_deref(), Some(reasons::IMPORTING));
    }

    // ---------- Hooks into the rest of the type system -------------------

    // Smoke: VMImage construction (rules out future field-rename drift breaking
    // these tests silently).
    #[test]
    fn vmimage_minimal_construct() {
        let _ = VMImage {
            metadata: ObjectMeta {
                name: Some("ubuntu-22-04".to_string()),
                ..Default::default()
            },
            spec: VMImageSpec {
                os_family: OsFamily::Linux,
                os_distribution: "ubuntu".to_string(),
                os_version: "22.04".to_string(),
                architecture: Architecture::Amd64,
                guest_agent: GuestAgent::CloudInit,
                sources: vec![vsphere_image_source("ubuntu-22.04")],
                cloud_config: None,
                template: None,
                iso_overlay: None,
            },
            status: None,
        };
    }
}
