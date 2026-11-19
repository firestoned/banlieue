// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `job.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{BuildArtifactKind, BuildArtifactPhase};
    use banlieue_api::common::LocalObjectReference;
    use serde_json::json;

    const UID: &str = "0b7c5f0e-2f4a-4c1e-9d59-5f3f2a1c8e11";

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
            first_boot_sealing_layer: false,
            prelaid_disk_size_gi_b: None,
        }
    }

    #[test]
    fn job_parts_come_from_the_build_and_its_os_artifact() {
        let parts = job_parts("prelay", "kairos", &artifact(), UID);
        assert_eq!(parts.pvc, "kairos-build-artifacts");
        assert_eq!(parts.file, "kairos-build.raw");
        assert_eq!(
            json!(parts.labels),
            json!({
                "app.kubernetes.io/name": "banlieue",
                "app.kubernetes.io/component": "prelay",
                "banlieue.io/vmimage": "kairos",
            })
        );
        let owners = parts.owner_references.expect("an owner");
        assert_eq!(owners[0]["name"], "kairos-build");
        assert_eq!(owners[0]["uid"], UID);
    }

    /// A build that names no PVC or file yields empty strings, as both Job
    /// builders always rendered it.
    #[test]
    fn job_parts_default_a_missing_pvc_and_file_to_empty() {
        let mut artifact = artifact();
        artifact.pvc_ref = None;
        artifact.file = None;
        let parts = job_parts("registry-push", "kairos", &artifact, UID);
        assert_eq!((parts.pvc.as_str(), parts.file.as_str()), ("", ""));
    }
}
