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
        Architecture, BuildArtifactKind, BuildArtifactPhase, BuildArtifactStatus, FailureDomain,
        FailureDomainAttributes, GuestAgent, ImageSource, ImageSourceKind, OsFamily, Provider,
        ProviderConnection, ProviderSpec, ProviderStatus, VMImage, VMImageSpec,
    };
    use banlieue_api::common::LocalObjectReference;
    use kube::api::ObjectMeta;

    use crate::client::{Datacenter, FakeClient, Inventory, VSphereClient};

    use super::super::{
        ImportJobInputs, build_import_job, compute_template_status, find_vsphere_source,
        gate_on_build_artifact, import_job_name, reasons, zone_from_job,
    };

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
        let n = import_job_name(
            "kairos-rhel98",
            "vcenter-ssc",
            "vcenter-ssc-dc-east-cluster-a",
        );
        assert_eq!(
            n,
            "import-kairos-rhel98-vcenter-ssc-vcenter-ssc-dc-east-cluster-a"
        );
        // Long inputs stay within the 63-char Kubernetes name cap.
        let long = import_job_name(&"x".repeat(40), &"y".repeat(40), &"z".repeat(40));
        assert!(long.len() <= 63);
        assert!(long.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
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
            network: None,
            disk: None,
            folder: None,
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
            network: None,
            disk: None,
            folder: None,
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
                network: None,
                disk: None,
                folder: None,
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
    fn zone_from_job_translates_success_running_and_failure() {
        use k8s_openapi::api::batch::v1::{Job, JobStatus};

        let ready = Job {
            status: Some(JobStatus {
                succeeded: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };
        let z = zone_from_job("fd-1", "import-x", &ready);
        assert!(z.ready);
        assert_eq!(z.reason.as_deref(), Some(reasons::RECONCILED));
        assert_eq!(z.resolved_ref.as_deref(), Some("fd-1/import-x"));

        let failed = Job {
            status: Some(JobStatus {
                failed: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };
        let z = zone_from_job("fd-1", "import-x", &failed);
        assert!(!z.ready);
        assert_eq!(z.reason.as_deref(), Some(reasons::IMPORT_FAILED));

        let running = Job {
            status: Some(JobStatus::default()),
            ..Default::default()
        };
        let z = zone_from_job("fd-1", "import-x", &running);
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
            },
            status: None,
        };
    }
}
