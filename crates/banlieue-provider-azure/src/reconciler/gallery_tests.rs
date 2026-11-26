// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the publish decisions (ADR-0162 Decisions 4 and 8).

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use banlieue_api::common::LocalObjectReference;

    use super::super::*;

    const OWNER: &str = "11111111-2222-3333-4444-555555555555";
    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const OLD: &str = "/subscriptions/s/resourceGroups/rg-img/providers/Microsoft.Compute/galleries/banlieue/images/kairos/versions/1.0.0";
    const NEW: &str = "/subscriptions/s/resourceGroups/rg-img/providers/Microsoft.Compute/galleries/banlieue/images/kairos/versions/2.0.0";

    fn artifact(
        kind: BuildArtifactKind,
        phase: BuildArtifactPhase,
        checksum: Option<&str>,
    ) -> BuildArtifactStatus {
        BuildArtifactStatus {
            kind,
            phase,
            os_artifact_ref: "a".into(),
            os_artifact_uid: None,
            pvc_ref: Some(LocalObjectReference { name: "pvc".into() }),
            file: Some("a.raw".into()),
            reason: None,
            message: None,
            checksum: checksum.map(str::to_string),
            oci_artifact: None,
            first_boot_sealing_layer: false,
            prelaid_disk_size_gi_b: None,
        }
    }

    fn version(state: &str, owner: &str, hex: &str) -> GalleryImageVersion {
        GalleryImageVersion {
            id: NEW.into(),
            provisioning_state: state.into(),
            tags: BTreeMap::from([
                (TAG_OWNER_UID.to_string(), owner.to_string()),
                (
                    banlieue_azure::TAG_SOURCE_SHA256.to_string(),
                    hex.to_string(),
                ),
            ]),
        }
    }

    fn row(
        resolved: Option<&str>,
        digest: Option<&str>,
        superseded: &[&str],
    ) -> ImagePerProviderStatus {
        ImagePerProviderStatus {
            provider_name: "az".into(),
            provider_namespace: "banlieue-system".into(),
            ready: true,
            resolved_ref: resolved.map(str::to_string),
            reason: None,
            message: None,
            zones: Vec::new(),
            source_digest: digest.map(str::to_string),
            min_os_disk_size_gib: None,
            superseded_refs: superseded.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn a_ready_cloud_image_with_a_sha256_checksum_publishes() {
        let checksum = format!("sha256:{}", HEX.to_uppercase());
        let a = artifact(
            BuildArtifactKind::CloudImage,
            BuildArtifactPhase::Ready,
            Some(&checksum),
        );
        let build = gate_build(Some(&a)).unwrap();
        assert_eq!(build.hex, HEX);
        assert_eq!(build.digest(), format!("sha256:{HEX}"));
    }

    #[test]
    fn the_build_gate_names_why_it_is_not_publishable() {
        let checksum = format!("sha256:{HEX}");
        let cases = [
            (None, reasons::BUILD_PENDING),
            (
                Some(artifact(
                    BuildArtifactKind::CloudImage,
                    BuildArtifactPhase::Building,
                    Some(&checksum),
                )),
                reasons::BUILD_PENDING,
            ),
            (
                Some(artifact(
                    BuildArtifactKind::CloudImage,
                    BuildArtifactPhase::Failed,
                    Some(&checksum),
                )),
                reasons::BUILD_FAILED,
            ),
            (
                Some(artifact(
                    BuildArtifactKind::Iso,
                    BuildArtifactPhase::Ready,
                    Some(&checksum),
                )),
                reasons::BUILD_KIND_UNSUPPORTED,
            ),
            (
                Some(artifact(
                    BuildArtifactKind::CloudImage,
                    BuildArtifactPhase::Ready,
                    None,
                )),
                reasons::CHECKSUM_MISSING,
            ),
            (
                Some(artifact(
                    BuildArtifactKind::CloudImage,
                    BuildArtifactPhase::Ready,
                    Some("sha512:00"),
                )),
                reasons::CHECKSUM_MISSING,
            ),
        ];
        for (a, want) in cases {
            let (reason, message) = gate_build(a.as_ref()).unwrap_err();
            assert_eq!(reason, want, "{message}");
        }
        let (_, message) = gate_build(Some(&artifact(
            BuildArtifactKind::Iso,
            BuildArtifactPhase::Ready,
            Some(&checksum),
        )))
        .unwrap_err();
        assert!(message.contains("vsphere Url source"), "{message}");
    }

    #[test]
    fn only_a_full_sha256_is_a_digest() {
        assert_eq!(
            sha256_hex(Some(&format!("sha256:{HEX}"))).as_deref(),
            Some(HEX)
        );
        assert_eq!(
            sha256_hex(Some(&format!("SHA256:{HEX}"))).as_deref(),
            Some(HEX)
        );
        for bad in ["", "sha256:", "sha256:abc", "md5:00", HEX] {
            assert_eq!(sha256_hex(Some(bad)), None, "{bad}");
        }
        let not_hex = format!("sha256:{}", "z".repeat(HEX.len()));
        assert_eq!(sha256_hex(Some(&not_hex)), None);
        assert_eq!(sha256_hex(None), None);
    }

    #[test]
    fn a_version_is_classified_by_state_owner_and_digest() {
        assert_eq!(classify_version(None, OWNER, HEX), VersionState::Missing);
        assert_eq!(
            classify_version(Some(&version("Succeeded", OWNER, HEX)), OWNER, HEX),
            VersionState::Published { id: NEW.into() }
        );
        assert_eq!(
            classify_version(Some(&version("Creating", OWNER, HEX)), OWNER, HEX),
            VersionState::InProgress
        );
        assert_eq!(
            classify_version(Some(&version("Failed", OWNER, HEX)), OWNER, HEX),
            VersionState::Failed
        );
        assert!(matches!(
            classify_version(Some(&version("Succeeded", "someone-else", HEX)), OWNER, HEX),
            VersionState::Foreign { .. }
        ));
        assert!(matches!(
            classify_version(Some(&version("Succeeded", OWNER, "00")), OWNER, HEX),
            VersionState::Foreign { .. }
        ));
        // A version with no owner tag at all is never ours, even for an
        // image whose UID is unknown.
        let mut untagged = version("Succeeded", OWNER, HEX);
        untagged.tags.clear();
        assert!(matches!(
            classify_version(Some(&untagged), "", HEX),
            VersionState::Foreign { .. }
        ));
    }

    /// The declared-state table in the module docs, row by row.
    #[test]
    fn the_publish_decision_table() {
        let published = VersionState::Published { id: NEW.into() };
        let foreign = VersionState::Foreign { detail: "x".into() };
        let due = JobState::Failed { retry_due: true };
        let not_due = JobState::Failed { retry_due: false };
        let cases = [
            (
                &published,
                &JobState::Absent,
                JobAction::None,
                true,
                reasons::RECONCILED,
            ),
            (
                &published,
                &not_due,
                JobAction::None,
                true,
                reasons::RECONCILED,
            ),
            (
                &foreign,
                &JobState::Absent,
                JobAction::None,
                false,
                reasons::VERSION_CONFLICT,
            ),
            (
                &VersionState::InProgress,
                &JobState::Absent,
                JobAction::Create,
                false,
                reasons::PUBLISHING,
            ),
            (
                &VersionState::InProgress,
                &JobState::Running,
                JobAction::None,
                false,
                reasons::PUBLISHING,
            ),
            (
                &VersionState::InProgress,
                &JobState::Succeeded,
                JobAction::Delete,
                false,
                reasons::PUBLISHING,
            ),
            (
                &VersionState::Missing,
                &JobState::Absent,
                JobAction::Create,
                false,
                reasons::PUBLISHING,
            ),
            (
                &VersionState::Missing,
                &JobState::Running,
                JobAction::None,
                false,
                reasons::PUBLISHING,
            ),
            // Deleted in Azure by hand after a successful publish: republish.
            (
                &VersionState::Missing,
                &JobState::Succeeded,
                JobAction::Delete,
                false,
                reasons::PUBLISHING,
            ),
            (
                &VersionState::Failed,
                &JobState::Absent,
                JobAction::Create,
                false,
                reasons::PUBLISHING,
            ),
            (
                &VersionState::Failed,
                &JobState::Succeeded,
                JobAction::Delete,
                false,
                reasons::PUBLISHING,
            ),
            (
                &VersionState::Missing,
                &due,
                JobAction::Delete,
                false,
                reasons::PUBLISHING,
            ),
            (
                &VersionState::Missing,
                &not_due,
                JobAction::None,
                false,
                reasons::PUBLISH_FAILED,
            ),
            (
                &VersionState::InProgress,
                &not_due,
                JobAction::None,
                false,
                reasons::PUBLISH_FAILED,
            ),
        ];
        for (version, job, action, ready, reason) in cases {
            let d = decide_publish(version, job);
            assert_eq!(
                (d.action, d.ready, d.reason),
                (action, ready, reason),
                "{version:?} / {job:?}"
            );
        }
    }

    #[test]
    fn a_superseded_version_is_deleted_only_when_ours_and_unreferenced() {
        let ours = version("Succeeded", OWNER, HEX);
        let theirs = version("Succeeded", "someone-else", HEX);
        let deleting = version("Deleting", OWNER, HEX);
        let due = JobState::Failed { retry_due: true };
        let not_due = JobState::Failed { retry_due: false };
        let cases = [
            (None, false, &JobState::Absent, SupersededAction::Drop),
            (
                Some(&theirs),
                false,
                &JobState::Absent,
                SupersededAction::Drop,
            ),
            (Some(&ours), true, &JobState::Absent, SupersededAction::Keep),
            (
                Some(&deleting),
                false,
                &JobState::Absent,
                SupersededAction::Keep,
            ),
            (
                Some(&ours),
                false,
                &JobState::Absent,
                SupersededAction::CreateDeleteJob,
            ),
            (
                Some(&ours),
                false,
                &JobState::Running,
                SupersededAction::Keep,
            ),
            (
                Some(&ours),
                false,
                &JobState::Succeeded,
                SupersededAction::RetryDeleteJob,
            ),
            (Some(&ours), false, &due, SupersededAction::RetryDeleteJob),
            (Some(&ours), false, &not_due, SupersededAction::Keep),
        ];
        for (read, referenced, job, want) in cases {
            assert_eq!(
                decide_superseded(read, OWNER, referenced, job),
                want,
                "{read:?} referenced={referenced} {job:?}"
            );
        }
    }

    #[test]
    fn a_rebuilt_digest_supersedes_the_published_version() {
        let existing = row(Some(OLD), Some("sha256:old"), &[]);
        assert_eq!(
            superseded_refs(Some(&existing), Some(("kairos", "2.0.0"))),
            vec![OLD.to_string()]
        );
    }

    #[test]
    fn the_current_version_is_never_superseded() {
        // The digest flipped back: 1.0.0 was superseded and is wanted again.
        let existing = row(Some(NEW), Some("sha256:new"), &[OLD]);
        assert_eq!(
            superseded_refs(Some(&existing), Some(("KAIROS", "1.0.0"))),
            vec![NEW.to_string()]
        );
        let same = row(Some(NEW), Some("sha256:new"), &[]);
        assert!(superseded_refs(Some(&same), Some(("kairos", "2.0.0"))).is_empty());
    }

    #[test]
    fn a_template_ref_is_never_superseded() {
        // sourceDigest unset: an admin's version, not banlieue's.
        let template = row(Some(OLD), None, &[]);
        assert!(superseded_refs(Some(&template), None).is_empty());
        assert!(owned_refs_on_delete(&template).is_empty());
        assert!(superseded_refs(None, None).is_empty());
    }

    #[test]
    fn tracked_versions_are_deduplicated_in_order() {
        let existing = row(Some(OLD), Some("sha256:old"), &[NEW, &OLD.to_uppercase()]);
        assert_eq!(
            superseded_refs(Some(&existing), None),
            vec![NEW.to_string(), OLD.to_uppercase()]
        );
    }

    #[test]
    fn a_version_id_parses_into_its_parts() {
        let p = VersionPath::parse(OLD).unwrap();
        assert_eq!(p.resource_group, "rg-img");
        assert_eq!(p.gallery, "banlieue");
        assert_eq!(p.image, "kairos");
        assert_eq!(p.version, "1.0.0");
        assert!(VersionPath::parse(&OLD.to_lowercase()).is_some());
        for bad in [
            "",
            "kairos",
            "/subscriptions/s/resourceGroups/rg/providers/Microsoft.Compute/galleries/g/images/i",
            "/subscriptions/s/resourceGroups/rg/providers/Microsoft.Network/galleries/g/images/i/versions/1",
        ] {
            assert!(VersionPath::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_finalizer_waits_only_on_live_providers_with_versions_left() {
        let live = BTreeSet::from([("banlieue-system".to_string(), "az".to_string())]);
        let tracked = row(Some(OLD), Some("sha256:old"), &[]);
        let draining = row(None, None, &[OLD]);
        let done = row(None, None, &[]);
        let template = row(Some(OLD), None, &[]);
        assert!(!finalizer_can_go(std::slice::from_ref(&tracked), &live));
        assert!(!finalizer_can_go(std::slice::from_ref(&draining), &live));
        assert!(finalizer_can_go(&[done, template], &live));
        // A Provider that is gone does not hold the delete forever.
        assert!(finalizer_can_go(&[tracked], &BTreeSet::new()));
    }
}
