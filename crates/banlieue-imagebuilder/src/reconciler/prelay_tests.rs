// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `prelay.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::BuildArtifactPhase;
    use banlieue_api::common::LocalObjectReference;
    use k8s_openapi::api::batch::v1::JobStatus;
    use serde_json::Value;

    const UID: &str = "0b7c5f0e-2f4a-4c1e-9d59-5f3f2a1c8e11";
    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const IMAGE: &str = "registry.internal:5000/banlieue/prelay:v1";
    const NO_SELECTOR: &BTreeMap<String, String> = &BTreeMap::new();

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
            reason: Some("Ready".to_string()),
            message: None,
            checksum: Some(DIGEST.replace("0123", "aaaa")),
            oci_artifact: None,
            first_boot_sealing_layer: true,
            prelaid_disk_size_gi_b: None,
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

    fn inputs<'a>(a: &'a BuildArtifactStatus) -> PrelayJobInputs<'a> {
        PrelayJobInputs {
            job_name: "kairos-build-prelay",
            namespace: "banlieue-imagebuild",
            vmimage: "kairos",
            image: IMAGE,
            artifact: a,
            os_artifact_uid: UID,
            disk_size_gib: 20,
            tolerations: &[],
            node_selector: NO_SELECTOR,
        }
    }

    fn container(job: &Value) -> &Value {
        &job["spec"]["template"]["spec"]["containers"][0]
    }

    fn env(job: &Value, name: &str) -> Option<String> {
        container(job)["env"]
            .as_array()?
            .iter()
            .find(|e| e["name"] == name)
            .and_then(|e| e["value"].as_str())
            .map(str::to_string)
    }

    // ---- names --------------------------------------------------------------

    #[test]
    fn names_derive_from_the_osartifact() {
        assert_eq!(prelay_job_name("kairos-build"), "kairos-build-prelay");
        assert_eq!(
            prelaid_file_name("kairos-build"),
            "kairos-build-prelaid.raw"
        );
    }

    // ---- refusals ----------------------------------------------------------

    #[test]
    fn an_iso_build_cannot_be_prelaid() {
        assert_eq!(
            prelay_refusal(&BuildArtifactKind::Iso, Some(IMAGE)),
            Some(PrelayRefusal::NeedsCloudImage)
        );
    }

    #[test]
    fn without_a_prelay_image_the_build_is_refused_not_shipped_raw() {
        let r = prelay_refusal(&BuildArtifactKind::CloudImage, None);
        assert_eq!(r, Some(PrelayRefusal::ImageUnset));
        assert_eq!(r.unwrap().reason(), "PrelayImageUnset");
        assert_eq!(
            PrelayRefusal::NeedsCloudImage.reason(),
            "PrelayNeedsCloudImage"
        );
    }

    #[test]
    fn a_cloud_image_with_a_prelay_image_is_accepted() {
        assert_eq!(
            prelay_refusal(&BuildArtifactKind::CloudImage, Some(IMAGE)),
            None
        );
    }

    // ---- the Job -------------------------------------------------------------

    #[test]
    fn the_job_prelays_the_built_file_into_a_new_one_on_the_same_pvc() {
        let a = artifact();
        let j = build_prelay_job(&inputs(&a));
        assert_eq!(
            env(&j, "PRELAY_INPUT").as_deref(),
            Some("/artifacts/kairos-build.raw")
        );
        assert_eq!(
            env(&j, "PRELAY_OUTPUT").as_deref(),
            Some("/artifacts/kairos-build-prelaid.raw")
        );
        assert_eq!(env(&j, "PRELAY_DISK_SIZE_GIB").as_deref(), Some("20"));
        let volumes = j["spec"]["template"]["spec"]["volumes"].as_array().unwrap();
        assert!(
            volumes
                .iter()
                .any(
                    |v| v["persistentVolumeClaim"]["claimName"] == "kairos-build-artifacts"
                        && v["persistentVolumeClaim"]["readOnly"] != true
                )
        );
        assert_eq!(container(&j)["image"], IMAGE);
    }

    /// SEC-004: the built file is checked against the source's checksum
    /// before anything is done to it.
    #[test]
    fn the_job_verifies_the_built_file_first_when_a_checksum_is_set() {
        let a = artifact();
        let j = build_prelay_job(&inputs(&a));
        assert_eq!(env(&j, "PRELAY_VERIFY_CHECKSUM"), a.checksum);

        let mut unchecked = artifact();
        unchecked.checksum = None;
        let j = build_prelay_job(&inputs(&unchecked));
        assert_eq!(env(&j, "PRELAY_VERIFY_CHECKSUM"), None);
    }

    #[test]
    fn the_job_reports_its_digest_as_the_termination_message() {
        let a = artifact();
        let j = build_prelay_job(&inputs(&a));
        assert_eq!(
            env(&j, "PRELAY_RESULT_FILE").as_deref(),
            Some("/dev/termination-log")
        );
        assert_eq!(container(&j)["terminationMessagePolicy"], "File");
    }

    /// Loop devices, mounts and /dev/kvm: the build namespace already runs
    /// privileged builds (ADR-0016); the Job has no API access.
    #[test]
    fn the_job_is_privileged_without_an_api_token_and_owned_by_the_osartifact() {
        let a = artifact();
        let j = build_prelay_job(&inputs(&a));
        assert_eq!(container(&j)["securityContext"]["privileged"], true);
        assert_eq!(
            j["spec"]["template"]["spec"]["automountServiceAccountToken"],
            false
        );
        let owner = &j["metadata"]["ownerReferences"][0];
        assert_eq!(owner["uid"], UID);
        assert_eq!(owner["name"], "kairos-build");
    }

    #[test]
    fn the_job_follows_build_scheduling() {
        let a = artifact();
        let selector = BTreeMap::from([("banlieue.io/kvm".to_string(), "true".to_string())]);
        let tolerations = vec![Toleration {
            key: Some("build".to_string()),
            operator: Some("Exists".to_string()),
            effect: Some("NoSchedule".to_string()),
            ..Toleration::default()
        }];
        let mut i = inputs(&a);
        i.node_selector = &selector;
        i.tolerations = &tolerations;
        let j = build_prelay_job(&i);
        assert_eq!(
            j["spec"]["template"]["spec"]["nodeSelector"]["banlieue.io/kvm"],
            "true"
        );
        assert_eq!(
            j["spec"]["template"]["spec"]["tolerations"][0]["key"],
            "build"
        );
    }

    // ---- the result ------------------------------------------------------------

    #[test]
    fn only_a_sha256_digest_is_accepted_as_the_result() {
        assert_eq!(
            parse_prelay_result(&format!("{DIGEST}\n")),
            Ok(DIGEST.to_string())
        );
        assert!(parse_prelay_result("sha256:abc").is_err());
        assert!(parse_prelay_result("md5:0123456789abcdef0123456789abcdef").is_err());
        assert!(parse_prelay_result(&DIGEST.to_uppercase()).is_err());
        assert!(parse_prelay_result("").is_err());
    }

    #[test]
    fn the_outcome_follows_the_job() {
        assert_eq!(prelay_outcome(&job(0, 0), None), PrelayOutcome::Running);
        assert_eq!(
            prelay_outcome(&job(1, 0), Some(DIGEST)),
            PrelayOutcome::Done(DIGEST.to_string())
        );
        // Succeeded, result not read yet: still running from our side.
        assert_eq!(prelay_outcome(&job(1, 0), None), PrelayOutcome::Running);
        assert!(matches!(
            prelay_outcome(&job(1, 0), Some("garbage")),
            PrelayOutcome::Failed(_)
        ));
        assert!(matches!(
            prelay_outcome(&job(0, 1), None),
            PrelayOutcome::Failed(_)
        ));
    }

    // ---- what the artifact says -------------------------------------------------

    #[test]
    fn while_prelaying_the_artifact_is_not_ready() {
        let mut a = artifact();
        apply_prelay(&mut a, &PrelayOutcome::Running, 20);
        assert_eq!(a.phase, BuildArtifactPhase::Building);
        assert_eq!(a.reason.as_deref(), Some("Prelaying"));
        assert_eq!(a.file, None, "no consumer may start on the raw file");
        assert_eq!(a.prelaid_disk_size_gi_b, None);
    }

    #[test]
    fn once_prelaid_the_artifact_is_the_prelaid_file_with_the_builders_digest() {
        let mut a = artifact();
        apply_prelay(&mut a, &PrelayOutcome::Done(DIGEST.to_string()), 20);
        assert_eq!(a.phase, BuildArtifactPhase::Ready);
        assert_eq!(a.file.as_deref(), Some("kairos-build-prelaid.raw"));
        assert_eq!(a.checksum.as_deref(), Some(DIGEST));
        assert_eq!(a.prelaid_disk_size_gi_b, Some(20));
        assert_eq!(a.pvc_ref.as_ref().unwrap().name, "kairos-build-artifacts");
    }

    #[test]
    fn a_failed_prelay_fails_the_artifact() {
        let mut a = artifact();
        apply_prelay(&mut a, &PrelayOutcome::Failed("boom".to_string()), 20);
        assert_eq!(a.phase, BuildArtifactPhase::Failed);
        assert_eq!(a.reason.as_deref(), Some("PrelayFailed"));
        assert_eq!(a.message.as_deref(), Some("boom"));
        assert_eq!(a.file, None);
    }

    #[test]
    fn a_refusal_fails_the_artifact_with_its_reason() {
        let mut a = artifact();
        apply_refusal(&mut a, PrelayRefusal::ImageUnset);
        assert_eq!(a.phase, BuildArtifactPhase::Failed);
        assert_eq!(a.reason.as_deref(), Some("PrelayImageUnset"));
        assert!(a.message.as_deref().unwrap().contains("--prelay-image"));
        assert_eq!(a.file, None);
    }
}
