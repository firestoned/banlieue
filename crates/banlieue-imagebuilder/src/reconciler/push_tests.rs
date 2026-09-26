// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `push.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{BuildArtifactPhase, ImageSource, ImageSourceKind};
    use banlieue_api::common::LocalObjectReference;
    use k8s_openapi::api::batch::v1::JobStatus;

    const UID: &str = "0b7c5f0e-2f4a-4c1e-9d59-5f3f2a1c8e11";
    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn source(class: &str, kind: ImageSourceKind) -> ImageSource {
        ImageSource {
            provider_class: class.to_string(),
            kind,
            reference: "kairos".to_string(),
            import_from: Some("quay.io/kairos/ubuntu:24.04".to_string()),
            checksum: None,
        }
    }

    fn registry() -> RegistryConfig {
        RegistryConfig::new(
            "registry.internal:5000/banlieue/disks",
            Some("registry-push".to_string()),
            false,
            "ghcr.io/firestoned/banlieue:v0.1.0".to_string(),
        )
        .unwrap()
    }

    fn artifact() -> BuildArtifactStatus {
        BuildArtifactStatus {
            kind: BuildArtifactKind::CloudImage,
            phase: BuildArtifactPhase::Ready,
            os_artifact_ref: "kairos-build".to_string(),
            os_artifact_uid: Some(UID.to_string()),
            pvc_ref: Some(LocalObjectReference {
                name: "kairos-build-artifacts".to_string(),
            }),
            file: Some("kairos-build.raw".to_string()),
            reason: None,
            message: None,
            checksum: None,
            oci_artifact: None,
        }
    }

    fn job(succeeded: i32, failed: i32) -> Job {
        Job {
            status: Some(JobStatus {
                succeeded: Some(succeeded),
                failed: Some(failed),
                ..JobStatus::default()
            }),
            ..Job::default()
        }
    }

    /// Only a `cloud-hypervisor` `Url` source needs the registry: every
    /// other consumer mounts the PVC, so installs without one need none.
    #[test]
    fn only_a_cloud_hypervisor_url_source_needs_a_push() {
        assert!(needs_push(&[
            source("libvirt", ImageSourceKind::Url),
            source(PROVIDER_CLASS_CLOUD_HYPERVISOR, ImageSourceKind::Url),
        ]));
        assert!(!needs_push(&[source("libvirt", ImageSourceKind::Url)]));
        assert!(!needs_push(&[source(
            PROVIDER_CLASS_CLOUD_HYPERVISOR,
            ImageSourceKind::BackingFile
        )]));
    }

    /// The repository is a plain `registry/path`: a tag or digest there would
    /// be silently replaced by the per-build tag.
    #[test]
    fn the_registry_repository_is_validated_at_startup() {
        assert!(
            RegistryConfig::new("registry.internal:5000/disks", None, false, "i".into()).is_ok()
        );
        assert!(RegistryConfig::new("disks", None, false, "i".into()).is_err());
        assert!(
            RegistryConfig::new("registry.internal:5000/disks:v1", None, false, "i".into())
                .is_err()
        );
        assert!(
            RegistryConfig::new(&format!("ghcr.io/a/b@{DIGEST}"), None, false, "i".into()).is_err()
        );
    }

    /// One tag per build: a rebuild (new `OSArtifact` uid) pushes afresh
    /// instead of finding the old build's tag and skipping.
    #[test]
    fn the_target_is_tagged_by_the_osartifact_uid() {
        let t = push_target(&registry(), UID);
        assert_eq!(
            t.to_string(),
            format!("registry.internal:5000/banlieue/disks:{UID}")
        );
    }

    #[test]
    fn job_names_fit_in_a_label_value() {
        let name = push_job_name(&"x".repeat(80));
        assert!(name.len() <= banlieue_provider_sdk::naming::MAX_NAME_LEN);
        assert_eq!(push_job_name("kairos-build"), "kairos-build-push");
    }

    fn inputs<'a>(
        registry: &'a RegistryConfig,
        artifact: &'a BuildArtifactStatus,
        tolerations: &'a [Toleration],
    ) -> PushJobInputs<'a> {
        PushJobInputs {
            job_name: "kairos-build-push",
            namespace: "banlieue-imagebuild",
            vmimage: "kairos",
            generation: 3,
            registry,
            artifact,
            os_artifact_uid: UID,
            tolerations,
        }
    }

    /// The Job reads the PVC and the push Secret and nothing else: no API
    /// token, no write access to the artifact, no privilege.
    #[test]
    fn the_push_job_is_least_privilege() {
        let registry = registry();
        let artifact = artifact();
        let job = build_push_job(&inputs(&registry, &artifact, &[]));
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(pod["automountServiceAccountToken"], false);
        assert_eq!(pod["securityContext"]["runAsNonRoot"], true);
        let c = &pod["containers"][0];
        assert_eq!(c["securityContext"]["readOnlyRootFilesystem"], true);
        assert_eq!(c["securityContext"]["allowPrivilegeEscalation"], false);
        assert_eq!(c["securityContext"]["capabilities"]["drop"][0], "ALL");
        let vols = pod["volumes"].as_array().unwrap();
        let pvc = vols.iter().find(|v| v["name"] == "artifacts").unwrap();
        assert_eq!(pvc["persistentVolumeClaim"]["readOnly"], true);
        assert_eq!(
            pvc["persistentVolumeClaim"]["claimName"],
            "kairos-build-artifacts"
        );
        let creds = vols
            .iter()
            .find(|v| v["name"] == "registry-credentials")
            .unwrap();
        assert_eq!(creds["secret"]["secretName"], "registry-push");
        // Owned by the OSArtifact: a rebuild garbage-collects it (ADR-0027).
        assert_eq!(job["metadata"]["ownerReferences"][0]["uid"], UID);
    }

    #[test]
    fn the_push_job_runs_the_push_subcommand_against_the_built_file() {
        let registry = registry();
        let artifact = artifact();
        let job = build_push_job(&inputs(&registry, &artifact, &[]));
        let args: Vec<String> = serde_json::from_value(
            job["spec"]["template"]["spec"]["containers"][0]["args"].clone(),
        )
        .unwrap();
        let joined = args.join(" ");
        assert!(joined.starts_with("imagebuilder push "), "{joined}");
        assert!(joined.contains("--source /artifacts/kairos-build.raw"));
        assert!(joined.contains(&format!(
            "--target registry.internal:5000/banlieue/disks:{UID}"
        )));
        assert!(joined.contains(&format!("--artifact-type {ARTIFACT_TYPE_RAW}")));
        assert!(joined.contains(&format!("--annotation {ANNOTATION_VMIMAGE}=kairos")));
        assert!(joined.contains(&format!("--annotation {ANNOTATION_GENERATION}=3")));
        assert!(joined.contains(&format!("--credentials-dir {CREDENTIALS_MOUNT_PATH}")));
        // The compressed copy goes to the emptyDir, named explicitly.
        assert!(joined.contains("--scratch-dir /scratch"), "{joined}");
        assert!(!joined.contains("--plain-http"));
    }

    /// Anonymous registries mount nothing; `--plain-http` passes through.
    #[test]
    fn an_anonymous_plain_http_registry_mounts_no_secret() {
        let registry = RegistryConfig::new(
            "registry.internal:5000/disks",
            None,
            true,
            "img".to_string(),
        )
        .unwrap();
        let artifact = artifact();
        let job = build_push_job(&inputs(&registry, &artifact, &[]));
        let pod = &job["spec"]["template"]["spec"];
        assert!(
            pod["volumes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["name"] != "registry-credentials")
        );
        let args = pod["containers"][0]["args"].to_string();
        assert!(args.contains("--plain-http"));
        assert!(!args.contains("--credentials-dir"));
    }

    #[test]
    fn an_iso_is_pushed_as_an_iso_artifact() {
        let registry = registry();
        let mut artifact = artifact();
        artifact.kind = BuildArtifactKind::Iso;
        artifact.file = Some("kairos-build.iso".to_string());
        let job = build_push_job(&inputs(&registry, &artifact, &[]));
        let args = job["spec"]["template"]["spec"]["containers"][0]["args"].to_string();
        assert!(args.contains(ARTIFACT_TYPE_ISO));
    }

    /// The termination message is the Job's only output. It is trusted
    /// only if it names the repository the Job was told to push to, by
    /// digest.
    #[test]
    fn the_pushed_reference_must_be_a_digest_in_the_configured_repository() {
        let r = registry();
        let ok = format!("registry.internal:5000/banlieue/disks@{DIGEST}\n");
        assert_eq!(
            pushed_reference(&ok, &r).unwrap(),
            format!("registry.internal:5000/banlieue/disks@{DIGEST}")
        );
        assert!(pushed_reference("registry.internal:5000/banlieue/disks:tag", &r).is_err());
        assert!(
            pushed_reference(&format!("evil.example.com/banlieue/disks@{DIGEST}"), &r).is_err()
        );
        assert!(pushed_reference(&format!("registry.internal:5000/other@{DIGEST}"), &r).is_err());
        assert!(pushed_reference("", &r).is_err());
    }

    #[test]
    fn job_outcomes_map_to_phases() {
        let r = registry();
        let running = oci_status(&r, &job(0, 0), None);
        assert_eq!(running.phase, OciArtifactPhase::Pushing);
        assert!(running.reference.is_none());

        let failed = oci_status(&r, &job(0, 2), None);
        assert_eq!(failed.phase, OciArtifactPhase::Failed);

        let msg = format!("registry.internal:5000/banlieue/disks@{DIGEST}");
        let done = oci_status(&r, &job(1, 0), Some(&msg));
        assert_eq!(done.phase, OciArtifactPhase::Ready);
        assert_eq!(done.reference.as_deref(), Some(msg.as_str()));

        // Succeeded, but the message is missing or wrong: never Ready.
        let odd = oci_status(&r, &job(1, 0), Some("nonsense"));
        assert_eq!(odd.phase, OciArtifactPhase::Failed);
        let silent = oci_status(&r, &job(1, 0), None);
        assert_eq!(silent.phase, OciArtifactPhase::Pushing);
    }
}
