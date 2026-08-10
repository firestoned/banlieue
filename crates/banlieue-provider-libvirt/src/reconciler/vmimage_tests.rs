// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `VMImage` reconciler.
//!
//! The decision logic (gating, pool selection, Job naming, Job manifest, Job
//! status translation) is factored into pure functions, so all of it is tested
//! without kube, TLS, or a libvirt host.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{
        BuildArtifactKind, FailureDomain, FailureDomainAttributes, ProviderCapabilities,
        ProviderConnection, ProviderSpec, ProviderStatus,
    };
    use banlieue_api::common::LocalObjectReference;
    use banlieue_provider_sdk::scheduling::parse_tolerations;
    use k8s_openapi::api::batch::v1::{Job, JobStatus};
    use kube::api::ObjectMeta;

    fn artifact(phase: BuildArtifactPhase) -> BuildArtifactStatus {
        BuildArtifactStatus {
            kind: BuildArtifactKind::CloudImage,
            phase,
            os_artifact_ref: "img-build".into(),
            pvc_ref: Some(LocalObjectReference {
                name: "img-build-artifacts".into(),
            }),
            file: Some("img-build.raw".into()),
            reason: None,
            message: None,
            checksum: None,
        }
    }

    fn provider_with_pools(pools: &str) -> Provider {
        Provider {
            metadata: ObjectMeta {
                name: Some("libvirt-1".into()),
                namespace: Some("banlieue-system".into()),
                ..Default::default()
            },
            spec: ProviderSpec {
                provider_class_ref: LocalObjectReference {
                    name: PROVIDER_CLASS_NAME.into(),
                },
                connection: ProviderConnection {
                    endpoint: "qemu+tls://libvirt-host.example/system".into(),
                    credentials_ref: LocalObjectReference {
                        name: "libvirt-creds".into(),
                    },
                    insecure_skip_tls_verify: false,
                    ca_bundle: None,
                },
                capabilities: ProviderCapabilities::default(),
                paused: false,
                use_content_library: false,
            },
            status: Some(ProviderStatus {
                failure_domains: vec![FailureDomain {
                    name: "libvirt-1".into(),
                    labels: Default::default(),
                    attributes: FailureDomainAttributes {
                        raw: [("pools".to_string(), pools.to_string())]
                            .into_iter()
                            .collect(),
                        ..Default::default()
                    },
                }],
                ..Default::default()
            }),
        }
    }

    /// A Provider that DECLARES storage classes and has had them verified,
    /// alongside the full discovered pool list in `raw`.
    fn provider_declaring(declared: &[(&str, &str)], discovered: &str) -> Provider {
        let mut p = provider_with_pools(discovered);
        p.spec.capabilities.storage_classes = declared
            .iter()
            .map(|(name, pool)| banlieue_api::banlieue::StorageClassMapping {
                name: (*name).to_string(),
                target: [("pool".to_string(), (*pool).to_string())]
                    .into_iter()
                    .collect(),
            })
            .collect();
        if let Some(st) = p.status.as_mut() {
            st.failure_domains[0].attributes.available_storage_classes =
                declared.iter().map(|(n, _)| (*n).to_string()).collect();
        }
        p
    }

    fn source(kind: ImageSourceKind) -> ImageSource {
        ImageSource {
            provider_class: PROVIDER_CLASS_NAME.into(),
            kind,
            reference: "/var/lib/libvirt/images/base.qcow2".into(),
            import_from: Some("quay.io/kairos/ubuntu:x".into()),
            checksum: None,
        }
    }

    // ---------- source selection ----------------------------------------

    #[test]
    fn picks_libvirt_url_and_backing_file_sources_only() {
        assert!(find_libvirt_source(&[source(ImageSourceKind::Url)]).is_some());
        assert!(find_libvirt_source(&[source(ImageSourceKind::BackingFile)]).is_some());
        // Template is a vSphere concept.
        assert!(find_libvirt_source(&[source(ImageSourceKind::Template)]).is_none());
        // Another provider's class is not ours.
        let mut other = source(ImageSourceKind::Url);
        other.provider_class = "vsphere".into();
        assert!(find_libvirt_source(&[other]).is_none());
    }

    // ---------- gating on the shared raw disk ---------------------------

    #[test]
    fn gate_blocks_until_the_raw_disk_is_ready() {
        // Absent entirely.
        let (reason, _) = gate_on_raw_disk(None).unwrap_err();
        assert_eq!(reason, reasons::BUILD_PENDING);

        for phase in [BuildArtifactPhase::Pending, BuildArtifactPhase::Building] {
            let a = artifact(phase);
            let (reason, _) = gate_on_raw_disk(Some(&a)).unwrap_err();
            assert_eq!(reason, reasons::BUILD_PENDING);
        }
    }

    #[test]
    fn gate_surfaces_a_failed_build_with_its_message() {
        let mut a = artifact(BuildArtifactPhase::Failed);
        a.message = Some("pull failed: manifest unknown".into());
        let (reason, message) = gate_on_raw_disk(Some(&a)).unwrap_err();
        assert_eq!(reason, reasons::BUILD_FAILED);
        assert_eq!(message, "pull failed: manifest unknown");
    }

    #[test]
    fn gate_passes_once_ready() {
        let a = artifact(BuildArtifactPhase::Ready);
        assert!(gate_on_raw_disk(Some(&a)).is_ok());
    }

    // ---------- pool selection ------------------------------------------

    #[test]
    fn target_pools_are_the_declared_ones_not_everything_on_the_host() {
        // `raw.pools` is discovery output — every pool libvirtd reports.
        // Importing into all of them writes gigabytes into pools the admin
        // never declared as a capability, which is the opposite of
        // "capabilities are declared, discovery is a status-time concern".
        let p = provider_declaring(
            &[("standard", "default"), ("bootstrap", "k0s-bootstrap")],
            "default,boot,k0s-bootstrap,images",
        );
        let pools = target_pools(&p);
        assert!(pools.contains(&"default".to_string()));
        assert!(pools.contains(&"k0s-bootstrap".to_string()));
        assert!(
            !pools.contains(&"boot".to_string()),
            "boot exists on the host but was never declared: {pools:?}"
        );
        assert!(
            !pools.contains(&"images".to_string()),
            "images exists on the host but was never declared: {pools:?}"
        );
    }

    #[test]
    fn an_undeclared_or_unverified_class_contributes_no_pool() {
        // A class the probe could not find is dropped from
        // availableStorageClasses; importing into its pool would target
        // something known not to be there.
        let mut p = provider_declaring(&[("standard", "default")], "default");
        if let Some(st) = p.status.as_mut() {
            st.failure_domains[0]
                .attributes
                .available_storage_classes
                .clear();
        }
        assert!(target_pools(&p).is_empty());
    }

    #[test]
    fn two_classes_targeting_one_pool_import_once() {
        // Otherwise the same multi-gigabyte transfer runs twice into the same
        // place, and the second Job races the first.
        let p = provider_declaring(&[("fast", "default"), ("cheap", "default")], "default");
        assert_eq!(target_pools(&p), vec!["default"]);
    }

    #[test]
    fn a_provider_without_status_offers_no_pools() {
        let mut p = provider_with_pools("default");
        p.status = None;
        assert!(target_pools(&p).is_empty());
    }

    // ---------- job naming ----------------------------------------------

    #[test]
    fn job_names_are_deterministic_and_kubernetes_legal() {
        // Deterministic so a re-reconcile adopts the running Job rather than
        // starting a second copy of a multi-gigabyte transfer.
        let a = import_job_name("kairos-ubuntu", "libvirt-1", "default");
        let b = import_job_name("kairos-ubuntu", "libvirt-1", "default");
        assert_eq!(a, b);
        assert!(a.len() <= 63);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{a} must be a legal object name"
        );
    }

    #[test]
    fn job_names_differ_per_pool_and_provider() {
        let base = import_job_name("img", "prov", "default");
        assert_ne!(base, import_job_name("img", "prov", "boot"));
        assert_ne!(base, import_job_name("img", "other", "default"));
    }

    #[test]
    fn long_inputs_are_truncated_to_the_name_limit() {
        let n = import_job_name(&"i".repeat(80), &"p".repeat(80), &"x".repeat(80));
        assert_eq!(n.len(), 63);
    }

    // ---------- job manifest --------------------------------------------

    const IMPORT_IMAGE: &str = "ghcr.io/example/banlieue:v1";

    const IMPORT_SA: &str = "banlieue-provider-libvirt-libvirt-1";

    fn inputs<'a>(
        job_name: &'a str,
        namespace: &'a str,
        vmimage: &'a str,
        provider: &'a Provider,
        artifact: &'a BuildArtifactStatus,
    ) -> ImportJobInputs<'a> {
        ImportJobInputs {
            job_name,
            namespace,
            image: IMPORT_IMAGE,
            service_account: Some(IMPORT_SA),
            vmimage,
            provider,
            pool: "default",
            artifact,
            tolerations: &[],
        }
    }

    #[test]
    fn import_job_mounts_the_artifacts_pvc_read_only() {
        let p = provider_with_pools("default");
        let a = artifact(BuildArtifactPhase::Ready);
        let job = build_import_job(&inputs(
            "import-x",
            "banlieue-system",
            "kairos-ubuntu",
            &p,
            &a,
        ));
        let vols = &job["spec"]["template"]["spec"]["volumes"];
        assert_eq!(
            vols[0]["persistentVolumeClaim"]["claimName"],
            "img-build-artifacts"
        );
        assert_eq!(
            vols[0]["persistentVolumeClaim"]["readOnly"], true,
            "the Job consumes the artifact and must never modify it"
        );
        let mounts = &job["spec"]["template"]["spec"]["containers"][0]["volumeMounts"];
        assert_eq!(mounts[0]["readOnly"], true);
        // The credentials Secret is NOT mounted. It lives with the Provider,
        // and the Job runs in the build namespace — a volume mount cannot
        // cross namespaces any more than the PVC can (observed as a
        // FailedMount on a real cluster). The import reads the Secret through
        // the API instead, under the read-only identity of ADR-0016 §4, which
        // also keeps it out of the filesystem of a pod running in a namespace
        // with no admission floor.
        assert!(
            vols.as_array().is_some_and(|v| v.len() == 1),
            "only the artifacts PVC should be mounted, got {vols}"
        );
        assert!(
            !job.to_string().contains("secretName"),
            "no Secret may be projected into the import pod"
        );
    }

    #[test]
    fn import_job_runs_the_banlieue_binary_not_a_third_party_image() {
        // ADR-0011: no external tools image, so the data path stays inside
        // banlieue's own SBOM/signing chain.
        let p = provider_with_pools("default");
        let a = artifact(BuildArtifactPhase::Ready);
        let job = build_import_job(&inputs("import-x", "ns", "img", &p, &a));
        let c = &job["spec"]["template"]["spec"]["containers"][0];
        assert_eq!(c["image"], IMPORT_IMAGE);
        let args: Vec<String> = c["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(&args[0..3], &["provider", "libvirt", "import"]);
        assert!(args.contains(&"/artifacts/img-build.raw".to_string()));
        assert!(args.contains(&"default".to_string()));
    }

    #[test]
    fn import_job_does_not_retry_indefinitely() {
        // A partial upload is only resumable by starting over; retrying
        // forever would hammer the host.
        let p = provider_with_pools("default");
        let a = artifact(BuildArtifactPhase::Ready);
        let job = build_import_job(&inputs("j", "ns", "vm", &p, &a));
        assert_eq!(job["spec"]["backoffLimit"], 1);
        assert_eq!(job["spec"]["template"]["spec"]["restartPolicy"], "Never");
    }

    #[test]
    fn import_job_runs_unprivileged() {
        let p = provider_with_pools("default");
        let a = artifact(BuildArtifactPhase::Ready);
        let job = build_import_job(&inputs("j", "ns", "vm", &p, &a));
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(pod["securityContext"]["runAsNonRoot"], true);
        let c = &pod["containers"][0]["securityContext"];
        assert_eq!(c["allowPrivilegeEscalation"], false);
        assert_eq!(c["readOnlyRootFilesystem"], true);
        assert_eq!(c["capabilities"]["drop"][0], "ALL");
    }

    #[test]
    fn import_job_runs_as_the_controllers_own_service_account() {
        // Not a fresh identity: the operator already scoped this one to this
        // Provider and its Secret, so the Job gains nothing extra (ADR-0012).
        let p = provider_with_pools("default");
        let a = artifact(BuildArtifactPhase::Ready);
        let job = build_import_job(&inputs("j", "ns", "vm", &p, &a));
        assert_eq!(
            job["spec"]["template"]["spec"]["serviceAccountName"],
            IMPORT_SA
        );
    }

    #[test]
    fn import_job_passes_the_providers_namespace_not_the_jobs() {
        // The Job runs beside the artifacts PVC in the build namespace; the
        // Provider it must read generally lives elsewhere, and defaulting to
        // the Job's own namespace would 404 at runtime.
        let p = provider_with_pools("default");
        let a = artifact(BuildArtifactPhase::Ready);
        let job = build_import_job(&inputs("j", "build-ns", "vm", &p, &a));
        let args: Vec<String> = job["spec"]["template"]["spec"]["containers"][0]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let idx = args
            .iter()
            .position(|a| a == "--provider-namespace")
            .expect("Job must tell the import where to find its Provider");
        assert_eq!(args[idx + 1], "banlieue-system");
    }

    // ---------- job status translation ----------------------------------

    fn job_with(succeeded: Option<i32>, failed: Option<i32>) -> Job {
        Job {
            status: Some(JobStatus {
                succeeded,
                failed,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_succeeded_job_makes_the_zone_ready() {
        let z = zone_from_job("default", "import-x", &job_with(Some(1), None));
        assert!(z.ready);
        assert_eq!(z.reason.as_deref(), Some(reasons::RECONCILED));
        assert_eq!(z.resolved_ref.as_deref(), Some("default/import-x"));
    }

    #[test]
    fn a_failed_job_is_reported_not_retried_silently() {
        let z = zone_from_job("default", "import-x", &job_with(None, Some(1)));
        assert!(!z.ready);
        assert_eq!(z.reason.as_deref(), Some(reasons::IMPORT_FAILED));
    }

    #[test]
    fn a_running_job_is_importing() {
        let z = zone_from_job("default", "import-x", &job_with(None, None));
        assert!(!z.ready);
        assert_eq!(z.reason.as_deref(), Some(reasons::IMPORTING));
    }

    // ---------- build-node pinning (ADR-0016 follow-up) ------------------

    #[test]
    fn an_import_job_never_carries_a_node_selector() {
        // Placement follows the artifacts PVC. The scheduler resolves that
        // from the bound PV's own constraints — on node-local storage it
        // confines the Job to the volume's node by itself, and on
        // network-attached storage there is nothing to confine. A selector
        // here would add a constraint Kubernetes never needed, and would be
        // wrong the moment the storage is not node-local.
        let p = provider_with_pools("default");
        let a = artifact(BuildArtifactPhase::Ready);
        let tol = parse_tolerations(&["dedicated=imagebuild:NoSchedule".to_string()])
            .expect("valid toleration");
        let mut i = inputs("j", "ns", "vm", &p, &a);
        i.tolerations = &tol;
        let job = build_import_job(&i);

        let spec = &job["spec"]["template"]["spec"];
        assert!(
            spec["nodeSelector"].is_null(),
            "import Jobs must not be pinned: {}",
            spec["nodeSelector"]
        );
    }

    #[test]
    fn an_import_job_tolerates_the_taints_it_was_given() {
        // A toleration is not placement — it is permission to land where the
        // scheduler already decided, which matters when the volume sits on a
        // dedicated, tainted build node.
        let p = provider_with_pools("default");
        let a = artifact(BuildArtifactPhase::Ready);
        let tol = parse_tolerations(&["dedicated=imagebuild:NoSchedule".to_string()])
            .expect("valid toleration");
        let mut i = inputs("j", "ns", "vm", &p, &a);
        i.tolerations = &tol;
        let job = build_import_job(&i);

        let spec = &job["spec"]["template"]["spec"];
        assert_eq!(spec["tolerations"][0]["key"], "dedicated");
        assert_eq!(spec["tolerations"][0]["effect"], "NoSchedule");
    }

    #[test]
    fn no_tolerations_means_the_key_is_omitted() {
        let p = provider_with_pools("default");
        let a = artifact(BuildArtifactPhase::Ready);
        let job = build_import_job(&inputs("j", "ns", "vm", &p, &a));
        assert!(job["spec"]["template"]["spec"]["tolerations"].is_null());
    }
}
