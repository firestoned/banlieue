// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `vmimage.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{
        BuildArtifactKind, BuildArtifactPhase, BuildArtifactStatus, OciArtifactPhase,
        OciArtifactStatus,
    };
    use std::path::Path;

    const REPO: &str = "registry.internal:5000/banlieue/disks";
    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const UID: &str = "6f1c2d3e-4a5b-4c6d-8e7f-0a1b2c3d4e5f";

    fn with_registry(c: HostConfig) -> HostConfig {
        HostConfig {
            registry: Some(crate::host_config::RegistrySection {
                repository: REPO.to_string(),
                credentials_dir: Some("/etc/banlieue/registry".into()),
                plain_http: false,
                keep_unreferenced: crate::host_config::DEFAULT_KEEP_UNREFERENCED,
            }),
            ..c
        }
    }

    fn artifact(oci: Option<OciArtifactStatus>) -> BuildArtifactStatus {
        BuildArtifactStatus {
            kind: BuildArtifactKind::CloudImage,
            phase: BuildArtifactPhase::Ready,
            os_artifact_ref: "k-build".into(),
            os_artifact_uid: None,
            pvc_ref: None,
            file: None,
            reason: None,
            message: None,
            checksum: None,
            oci_artifact: oci,
        }
    }

    fn pushed(reference: &str) -> Option<OciArtifactStatus> {
        Some(OciArtifactStatus {
            phase: OciArtifactPhase::Ready,
            reference: Some(reference.to_string()),
            message: None,
        })
    }

    fn config(fast: &Path, slow: &Path) -> HostConfig {
        HostConfig::parse(&format!(
            r#"
[provider]
name = "ch-a"
namespace = "banlieue-system"
kubeconfig = "/etc/banlieue/kubeconfig"
[vmm]
binary = "/usr/local/bin/cloud-hypervisor"
version = "v53.0"
firmware = "/opt/banlieue/firmware/f/CLOUDHV.fd"
[paths]
run_root = "/run/banlieue/ch"
state_root = "/var/lib/banlieue"
[guests]
uid_base = 2000000
uid_count = 10
[storage_classes]
fast = "{}"
slow = "{}"
[network_classes]
lan = "br0"
"#,
            fast.display(),
            slow.display()
        ))
        .unwrap()
    }

    fn source(class: &str, kind: ImageSourceKind, reference: &str) -> ImageSource {
        ImageSource {
            provider_class: class.into(),
            kind,
            reference: reference.into(),
            import_from: None,
            checksum: None,
        }
    }

    #[test]
    fn only_our_provider_class_is_picked() {
        let sources = vec![
            source("libvirt", ImageSourceKind::BackingFile, "a.raw"),
            source(PROVIDER_CLASS, ImageSourceKind::BackingFile, "b.raw"),
        ];
        assert_eq!(find_source(&sources).unwrap().reference, "b.raw");
        assert!(find_source(&sources[..1]).is_none());
    }

    #[test]
    fn a_reference_path_means_its_file_name() {
        assert_eq!(image_file_name("/srv/x/images/kairos.raw"), "kairos.raw");
        assert_eq!(image_file_name("kairos.raw"), "kairos.raw");
    }

    /// Grounded in a real directory tree: only classes whose cache holds
    /// the file count.
    #[test]
    fn classes_holding_reads_each_class_cache() {
        let fast = tempfile::tempdir().unwrap();
        let slow = tempfile::tempdir().unwrap();
        std::fs::create_dir(fast.path().join(IMAGES_DIR)).unwrap();
        std::fs::write(fast.path().join(IMAGES_DIR).join("k.raw"), b"x").unwrap();
        let c = config(fast.path(), slow.path());
        assert_eq!(classes_holding(&c, "k.raw"), vec!["fast".to_string()]);
        assert!(classes_holding(&c, "other.raw").is_empty());
    }

    #[test]
    fn a_held_backing_file_is_ready_with_its_name_as_ref_and_no_path() {
        let c = config(Path::new("/srv/fast"), Path::new("/srv/slow"));
        let s = source(PROVIDER_CLASS, ImageSourceKind::BackingFile, "/any/k.raw");
        let row = compute_row(&c, &s, &["fast".into()]);
        assert!(row.ready);
        assert_eq!(row.resolved_ref.as_deref(), Some("k.raw"));
        assert_eq!(row.provider_name, "ch-a");
        assert_eq!(row.provider_namespace, "banlieue-system");
        assert!(
            !row.message.unwrap().contains('/'),
            "no host path in status"
        );
    }

    #[test]
    fn a_missing_file_is_not_ready_and_says_so() {
        let c = config(Path::new("/srv/fast"), Path::new("/srv/slow"));
        let s = source(PROVIDER_CLASS, ImageSourceKind::BackingFile, "k.raw");
        let row = compute_row(&c, &s, &[]);
        assert!(!row.ready);
        assert!(row.resolved_ref.is_none());
        assert_eq!(row.reason.as_deref(), Some(reasons::IMAGE_NOT_FOUND));
        assert!(!row.message.unwrap().contains('/'));
    }

    #[test]
    fn a_hidden_or_odd_file_name_is_refused() {
        let c = config(Path::new("/srv/fast"), Path::new("/srv/slow"));
        for bad in [".hidden", "a b.raw", "dir/"] {
            let s = source(PROVIDER_CLASS, ImageSourceKind::BackingFile, bad);
            let row = compute_row(&c, &s, &["fast".into()]);
            assert!(!row.ready, "{bad:?}");
            assert_eq!(row.reason.as_deref(), Some(reasons::INVALID_REFERENCE));
        }
    }

    /// Content-addressed: two `VMImage`s of the same bytes share one file.
    #[test]
    fn the_cache_file_is_named_by_digest() {
        assert_eq!(
            cached_image_name(&format!("sha256:{HEX}")).unwrap(),
            format!("sha256-{HEX}.raw")
        );
        assert!(cached_image_name("sha256:short").is_err());
    }

    #[test]
    fn a_url_source_without_a_host_registry_says_so() {
        let c = config(Path::new("/srv/fast"), Path::new("/srv/slow"));
        let a = artifact(pushed(&format!("{REPO}@sha256:{HEX}")));
        let err = pull_target(&c, Some(&a)).unwrap_err();
        assert_eq!(err.0, reasons::REGISTRY_NOT_CONFIGURED);
    }

    #[test]
    fn a_url_source_waits_for_the_pushed_artifact() {
        let c = with_registry(config(Path::new("/srv/fast"), Path::new("/srv/slow")));
        assert_eq!(
            pull_target(&c, None).unwrap_err().0,
            reasons::AWAITING_ARTIFACT
        );
        let pushing = artifact(Some(OciArtifactStatus {
            phase: OciArtifactPhase::Pushing,
            reference: None,
            message: None,
        }));
        assert_eq!(
            pull_target(&c, Some(&pushing)).unwrap_err().0,
            reasons::AWAITING_ARTIFACT
        );
    }

    /// Whoever can write `VMImage` status must not be able to choose what
    /// this host boots: only the host's own repository, only by digest.
    #[test]
    fn a_reference_outside_the_host_repository_is_refused() {
        let c = with_registry(config(Path::new("/srv/fast"), Path::new("/srv/slow")));
        for bad in [
            format!("evil.example.com/banlieue/disks@sha256:{HEX}"),
            format!("registry.internal:5000/other@sha256:{HEX}"),
            format!("{REPO}:latest"),
        ] {
            let a = artifact(pushed(&bad));
            assert_eq!(
                pull_target(&c, Some(&a)).unwrap_err().0,
                reasons::FOREIGN_REFERENCE,
                "{bad}"
            );
        }
    }

    #[test]
    fn a_pushed_artifact_in_the_host_repository_is_pulled_by_digest() {
        let c = with_registry(config(Path::new("/srv/fast"), Path::new("/srv/slow")));
        let a = artifact(pushed(&format!("{REPO}@sha256:{HEX}")));
        let t = pull_target(&c, Some(&a)).unwrap();
        assert_eq!(t.reference, format!("{REPO}@sha256:{HEX}"));
        assert_eq!(t.file, format!("sha256-{HEX}.raw"));
    }

    fn target() -> PullTarget {
        PullTarget {
            reference: format!("{REPO}@sha256:{HEX}"),
            file: format!("sha256-{HEX}.raw"),
        }
    }

    /// Ready only once every storage class holds it: a machine may pick
    /// any of them.
    #[test]
    fn import_rows_follow_the_cache_and_the_unit() {
        let c = with_registry(config(Path::new("/srv/fast"), Path::new("/srv/slow")));
        let t = target();

        let done = import_row(&c, &t, &["fast".into(), "slow".into()], &ImportUnit::Absent);
        assert!(done.ready);
        assert_eq!(done.resolved_ref.as_deref(), Some(t.file.as_str()));

        let partial = import_row(&c, &t, &["fast".into()], &ImportUnit::Absent);
        assert!(!partial.ready);
        assert_eq!(partial.reason.as_deref(), Some(reasons::IMPORTING));

        let running = import_row(&c, &t, &[], &ImportUnit::Running);
        assert_eq!(running.reason.as_deref(), Some(reasons::IMPORTING));

        let failed = import_row(&c, &t, &[], &ImportUnit::Failed("exit 1".into()));
        assert!(!failed.ready);
        assert_eq!(failed.reason.as_deref(), Some(reasons::IMPORT_FAILED));
        assert!(failed.message.unwrap().contains("exit 1"));
        for row in [done, running] {
            assert!(
                !row.message.unwrap_or_default().contains("/srv"),
                "no host path"
            );
        }
    }

    /// An instance of the import template, named by the VMImage's UID; the
    /// reference and cache file go in its environment file, the rest is
    /// the template's.
    #[test]
    fn the_import_unit_is_a_template_instance_with_its_target_in_the_environment() {
        let c = with_registry(config(Path::new("/srv/fast"), Path::new("/srv/slow")));
        assert_eq!(
            import_unit_name(UID).unwrap(),
            format!("banlieue-ch-import@{UID}.service")
        );
        assert!(import_unit_name("not-a-uid").is_err());

        let u = import_unit(&c, UID, &target()).unwrap();
        assert_eq!(u.name, format!("banlieue-ch-import@{UID}.service"));
        assert!(u.memory_max.is_none());
        let env = u.environment.expect("an environment file");
        assert_eq!(
            env.path,
            Path::new("/var/lib/banlieue/units").join(format!("import-{UID}.env"))
        );
        assert_eq!(
            env.path,
            import_env_file(&c, UID),
            "the file cleanup removes"
        );
        assert_eq!(
            env.render().unwrap(),
            format!("REFERENCE={REPO}@sha256:{HEX}\nFILE=sha256-{HEX}.raw\n")
        );
    }

    fn pulled(hex_digit: char) -> String {
        format!("sha256-{}.raw", hex_digit.to_string().repeat(64))
    }

    /// A VMImage as the API would return it, with this host's row.
    fn image(name: &str, resolved: Option<&str>, deleting: bool) -> VMImage {
        let mut v = serde_json::json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "VMImage",
            "metadata": { "name": name },
            "spec": { "osFamily": "linux", "osDistribution": "ubuntu", "osVersion": "24.04", "architecture": "amd64", "sources": [] },
            "status": { "perProvider": [{
                "providerName": "ch-a",
                "providerNamespace": "banlieue-system",
                "ready": resolved.is_some(),
                "resolvedRef": resolved,
            }, {
                "providerName": "other-host",
                "providerNamespace": "banlieue-system",
                "ready": true,
                "resolvedRef": pulled('e'),
            }]}
        });
        if deleting {
            v["metadata"]["deletionTimestamp"] = "2026-09-27T00:00:00Z".into();
        }
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn only_digest_named_files_are_pulled_cache_entries() {
        assert!(is_pulled_cache_name(&pulled('a')));
        assert!(
            !is_pulled_cache_name("kairos.raw"),
            "admin-placed BackingFile"
        );
        assert!(!is_pulled_cache_name(&format!("{}.partial", pulled('a'))));
        assert!(!is_pulled_cache_name("sha256-abc.raw"));
    }

    /// Referenced: this host's row of every image not being deleted, and
    /// every machine's boot image. Other hosts' rows do not count.
    #[test]
    fn referenced_files_come_from_live_images_and_machines() {
        let c = config(Path::new("/srv/fast"), Path::new("/srv/slow"));
        let images = [
            image("live", Some(&pulled('a')), false),
            image("going", Some(&pulled('b')), true),
            image("pending", None, false),
        ];
        let machine = pulled('c');
        let r = referenced_files(&c, &images, [machine.as_str()]);
        assert!(r.contains(&pulled('a')));
        assert!(!r.contains(&pulled('b')), "being deleted");
        assert!(r.contains(&pulled('c')), "a machine boots from it");
        assert!(!r.contains(&pulled('e')), "another host's row");
    }

    /// Keep the newest `keep` unreferenced pulls; never touch a referenced
    /// file or an admin's file.
    #[test]
    fn eviction_keeps_the_newest_unreferenced_and_everything_referenced() {
        use std::time::{Duration, UNIX_EPOCH};
        let at = |s: u64| UNIX_EPOCH + Duration::from_secs(s);
        let entries = vec![
            (pulled('1'), at(1)),
            (pulled('2'), at(2)),
            (pulled('3'), at(3)),
            (pulled('4'), at(4)),
            ("kairos.raw".to_string(), at(0)),
        ];
        let referenced = std::collections::BTreeSet::from([pulled('1')]);
        let evict = eviction_candidates(&entries, &referenced, 1);
        assert_eq!(evict, vec![pulled('3'), pulled('2')]);
        assert!(eviction_candidates(&entries, &referenced, 5).is_empty());
    }

    #[test]
    fn a_release_row_is_not_ready_and_says_released() {
        let c = config(Path::new("/srv/fast"), Path::new("/srv/slow"));
        let row = release_row(&c, "removed".into());
        assert!(!row.ready);
        assert!(row.resolved_ref.is_none());
        assert_eq!(row.reason.as_deref(), Some(reasons::RELEASED));
    }
}
