// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `digest.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{BuildArtifactKind, BuildArtifactPhase};
    use banlieue_api::common::LocalObjectReference;
    use k8s_openapi::api::batch::v1::JobStatus;

    const UID: &str = "0b7c5f0e-2f4a-4c1e-9d59-5f3f2a1c8e11";
    const OTHER_UID: &str = "7f1d2c3b-0000-4000-8000-000000000001";
    const IMAGE: &str = "ghcr.io/firestoned/banlieue:v0.1.0";
    const SHA256: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const SHA256_OTHER: &str =
        "sha256:aaaa456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const SHA512: &str = "sha512:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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
            checksum: None,
            oci_artifact: None,
            first_boot_sealing_layer: false,
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

    fn inputs(a: &BuildArtifactStatus, sha512: bool) -> DigestJobInputs<'_> {
        DigestJobInputs {
            job_name: "kairos-build-digest",
            namespace: "banlieue-imagebuild",
            vmimage: "kairos",
            image: IMAGE,
            artifact: a,
            os_artifact_uid: UID,
            sha512,
            tolerations: &[],
        }
    }

    fn pod_spec(job: &Value) -> &Value {
        &job["spec"]["template"]["spec"]
    }

    fn container(job: &Value) -> &Value {
        &pod_spec(job)["containers"][0]
    }

    fn measured(sha256: &str) -> MeasuredDigests {
        MeasuredDigests {
            sha256: sha256.to_string(),
            sha512: None,
        }
    }

    // ---- names ---------------------------------------------------------------

    #[test]
    fn the_job_name_derives_from_the_osartifact() {
        assert_eq!(digest_job_name("kairos-build"), "kairos-build-digest");
        assert!(digest_job_name(&"x".repeat(80)).len() <= 63);
    }

    // ---- the declared checksum -----------------------------------------------

    #[test]
    fn a_declared_sha256_or_sha512_parses_and_is_lowercased() {
        let d = parse_declared_checksum(&SHA256.to_uppercase().replacen("SHA256", "sha256", 1))
            .unwrap();
        assert_eq!(d.algorithm, ChecksumAlgorithm::Sha256);
        assert_eq!(d.to_string(), SHA256);
        let d = parse_declared_checksum(SHA512).unwrap();
        assert_eq!(d.algorithm, ChecksumAlgorithm::Sha512);
    }

    #[test]
    fn an_unsupported_or_malformed_declared_checksum_is_refused() {
        for bad in [
            "md5:0123456789abcdef0123456789abcdef",
            "sha256:0123",
            "sha256",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "sha256:zz23456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            &SHA512.replacen("sha512", "sha256", 1),
        ] {
            assert!(parse_declared_checksum(bad).is_err(), "{bad} accepted");
        }
    }

    // ---- the Job ---------------------------------------------------------------

    #[test]
    fn the_job_runs_the_digest_subcommand_on_the_built_file() {
        let a = artifact();
        let j = build_digest_job(&inputs(&a, false));
        assert_eq!(j["metadata"]["name"], "kairos-build-digest");
        assert_eq!(j["metadata"]["namespace"], "banlieue-imagebuild");
        assert_eq!(container(&j)["image"], IMAGE);
        assert_eq!(
            container(&j)["args"],
            json!([
                "imagebuilder",
                "digest",
                "--file",
                "/artifacts/kairos-build.raw"
            ])
        );
        let j = build_digest_job(&inputs(&a, true));
        assert_eq!(container(&j)["args"][4], "--sha512");
    }

    #[test]
    fn the_job_is_owned_by_the_osartifact_and_labelled() {
        let a = artifact();
        let j = build_digest_job(&inputs(&a, false));
        let owner = &j["metadata"]["ownerReferences"][0];
        assert_eq!(owner["kind"], "OSArtifact");
        assert_eq!(owner["uid"], UID);
        assert_eq!(
            j["metadata"]["labels"]["app.kubernetes.io/component"],
            "digest"
        );
        assert_eq!(j["metadata"]["labels"]["banlieue.io/vmimage"], "kairos");
    }

    /// The pod passes the provider-jobs hardening shape: no token, non-root,
    /// seccomp, read-only root, no privilege, every capability dropped, and
    /// the artifacts PVC mounted read-only.
    #[test]
    fn the_job_is_a_restricted_pod_with_a_read_only_pvc() {
        let a = artifact();
        let j = build_digest_job(&inputs(&a, false));
        let spec = pod_spec(&j);
        assert_eq!(spec["automountServiceAccountToken"], false);
        assert_eq!(spec["restartPolicy"], "Never");
        assert_eq!(spec["securityContext"]["runAsNonRoot"], true);
        assert_eq!(
            spec["securityContext"]["seccompProfile"]["type"],
            "RuntimeDefault"
        );
        let sc = &container(&j)["securityContext"];
        assert_eq!(sc["allowPrivilegeEscalation"], false);
        assert_eq!(sc["readOnlyRootFilesystem"], true);
        assert_eq!(sc["capabilities"]["drop"], json!(["ALL"]));
        assert!(sc.get("privileged").is_none());
        assert_eq!(container(&j)["volumeMounts"][0]["readOnly"], true);
        assert_eq!(
            spec["volumes"][0]["persistentVolumeClaim"],
            json!({ "claimName": "kairos-build-artifacts", "readOnly": true })
        );
        assert_eq!(j["spec"]["backoffLimit"], 1);
        assert!(j["spec"].get("ttlSecondsAfterFinished").is_none());
    }

    // ---- the result ------------------------------------------------------------

    #[test]
    fn a_result_is_sha256_then_an_optional_sha512() {
        assert_eq!(parse_digest_result(SHA256).unwrap(), measured(SHA256));
        let both = parse_digest_result(&format!("{SHA256}\n{SHA512}\n")).unwrap();
        assert_eq!(both.sha512.as_deref(), Some(SHA512));
    }

    #[test]
    fn anything_else_in_a_result_is_refused() {
        for bad in [
            String::new(),
            "sha256:0123".to_string(),
            SHA256.to_uppercase(),
            SHA512.to_string(),
            format!("{SHA256}\n{SHA256}"),
            format!("{SHA256}\n{SHA512}\nextra"),
        ] {
            assert!(parse_digest_result(&bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn the_outcome_follows_the_job_and_its_message() {
        assert_eq!(digest_outcome(&job(0, 0), None), DigestOutcome::Running);
        assert_eq!(
            digest_outcome(&job(1, 0), Some(SHA256)),
            DigestOutcome::Done(measured(SHA256))
        );
        assert_eq!(digest_outcome(&job(1, 0), None), DigestOutcome::ResultLost);
        assert!(matches!(
            digest_outcome(&job(1, 0), Some("not a digest")),
            DigestOutcome::Failed(_)
        ));
        assert!(matches!(
            digest_outcome(&job(0, 1), None),
            DigestOutcome::Failed(_)
        ));
    }

    // ---- carrying a measured digest forward ----------------------------------

    #[test]
    fn a_digest_already_recorded_for_this_build_is_carried_forward() {
        let mut previous = artifact();
        previous.checksum = Some(SHA256.to_string());
        assert_eq!(
            carried_digest(Some(&previous), UID).as_deref(),
            Some(SHA256)
        );
        assert_eq!(carried_digest(Some(&previous), OTHER_UID), None);
        assert_eq!(carried_digest(None, UID), None);

        let mut pending = previous.clone();
        pending.phase = BuildArtifactPhase::Building;
        assert_eq!(carried_digest(Some(&pending), UID), None);

        let mut prelaid = previous;
        prelaid.prelaid_disk_size_gi_b = Some(1);
        assert_eq!(carried_digest(Some(&prelaid), UID), None);
    }

    // ---- folding into status ---------------------------------------------------

    #[test]
    fn while_running_the_build_is_building_with_no_file_and_no_checksum() {
        let mut s = artifact();
        apply_digest(&mut s, &DigestOutcome::Running, None);
        assert_eq!(s.phase, BuildArtifactPhase::Building);
        assert_eq!(s.reason.as_deref(), Some(REASON_DIGEST_PENDING));
        assert_eq!(s.file, None);
        assert_eq!(s.checksum, None);

        let mut s = artifact();
        apply_digest(&mut s, &DigestOutcome::ResultLost, None);
        assert_eq!(s.phase, BuildArtifactPhase::Building);
        assert_eq!(s.reason.as_deref(), Some(REASON_DIGEST_PENDING));
    }

    #[test]
    fn done_records_the_measured_sha256_and_stays_ready() {
        let mut s = artifact();
        apply_digest(&mut s, &DigestOutcome::Done(measured(SHA256)), None);
        assert_eq!(s.phase, BuildArtifactPhase::Ready);
        assert_eq!(s.checksum.as_deref(), Some(SHA256));
        assert_eq!(s.file.as_deref(), Some("kairos-build.raw"));
    }

    #[test]
    fn a_matching_declared_checksum_records_the_measured_value() {
        let declared =
            parse_declared_checksum(&SHA256.to_uppercase().replacen("SHA256", "sha256", 1))
                .unwrap();
        let mut s = artifact();
        apply_digest(
            &mut s,
            &DigestOutcome::Done(measured(SHA256)),
            Some(&declared),
        );
        assert_eq!(s.phase, BuildArtifactPhase::Ready);
        assert_eq!(s.checksum.as_deref(), Some(SHA256));

        let declared = parse_declared_checksum(SHA512).unwrap();
        let mut s = artifact();
        let both = MeasuredDigests {
            sha256: SHA256.to_string(),
            sha512: Some(SHA512.to_string()),
        };
        apply_digest(&mut s, &DigestOutcome::Done(both), Some(&declared));
        assert_eq!(s.phase, BuildArtifactPhase::Ready);
        assert_eq!(s.checksum.as_deref(), Some(SHA256), "sha256 is recorded");
    }

    /// SEC-004: a declared checksum still fails the build closed.
    #[test]
    fn a_mismatched_declared_checksum_fails_closed() {
        let declared = parse_declared_checksum(SHA256_OTHER).unwrap();
        let mut s = artifact();
        apply_digest(
            &mut s,
            &DigestOutcome::Done(measured(SHA256)),
            Some(&declared),
        );
        assert_eq!(s.phase, BuildArtifactPhase::Failed);
        assert_eq!(s.reason.as_deref(), Some(REASON_CHECKSUM_MISMATCH));
        assert_eq!(s.file, None);
        assert_eq!(s.checksum, None);

        // A declared sha512 with no sha512 measured is never a pass.
        let declared = parse_declared_checksum(SHA512).unwrap();
        let mut s = artifact();
        apply_digest(
            &mut s,
            &DigestOutcome::Done(measured(SHA256)),
            Some(&declared),
        );
        assert_eq!(s.phase, BuildArtifactPhase::Failed);
    }

    #[test]
    fn a_carried_digest_is_ready_and_a_failure_is_failed() {
        let mut s = artifact();
        apply_digest(&mut s, &DigestOutcome::Carried(SHA256.to_string()), None);
        assert_eq!(s.phase, BuildArtifactPhase::Ready);
        assert_eq!(s.checksum.as_deref(), Some(SHA256));

        // Re-checked against a declared sha256 that changed since.
        let declared = parse_declared_checksum(SHA256_OTHER).unwrap();
        let mut s = artifact();
        apply_digest(
            &mut s,
            &DigestOutcome::Carried(SHA256.to_string()),
            Some(&declared),
        );
        assert_eq!(s.phase, BuildArtifactPhase::Failed);
        assert_eq!(s.reason.as_deref(), Some(REASON_CHECKSUM_MISMATCH));

        let mut s = artifact();
        apply_digest(&mut s, &DigestOutcome::Failed("why".to_string()), None);
        assert_eq!(s.phase, BuildArtifactPhase::Failed);
        assert_eq!(s.reason.as_deref(), Some(REASON_DIGEST_FAILED));
        assert_eq!(s.message.as_deref(), Some("why"));
        assert_eq!(s.file, None);
        assert_eq!(s.checksum, None);
    }

    #[test]
    fn an_unusable_declared_checksum_fails_the_build_before_any_job() {
        let mut s = artifact();
        apply_checksum_refusal(&mut s, "md5:00", "unsupported");
        assert_eq!(s.phase, BuildArtifactPhase::Failed);
        assert_eq!(s.reason.as_deref(), Some(REASON_CHECKSUM_UNSUPPORTED));
        assert!(s.message.as_deref().unwrap().contains("md5:00"));
        assert_eq!(s.file, None);
        assert_eq!(s.checksum, None);
    }
}
