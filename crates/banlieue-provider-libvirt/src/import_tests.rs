// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `import` subcommand's decision logic.
//!
//! The whole of `import` that can be wrong without a host — which volume to
//! write, which pool to write it into, and whether the work is already done —
//! is factored into pure functions and tested here. What is left is I/O.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_libvirt::UUID_LEN;

    fn pool(name: &str, seed: u8) -> StoragePool {
        StoragePool {
            name: name.to_string(),
            uuid: [seed; UUID_LEN],
        }
    }

    fn vol(pool: &str, name: &str) -> StorageVol {
        StorageVol {
            pool: pool.to_string(),
            name: name.to_string(),
            key: format!("/var/lib/libvirt/images/{name}"),
        }
    }

    // ---------- volume naming -------------------------------------------

    #[test]
    fn volume_defaults_to_the_vmimage_name_with_a_raw_suffix() {
        // The artifact is a raw disk (ADR-0011), so the name should say so —
        // an operator running `virsh vol-list` sees the format without asking.
        assert_eq!(volume_name("kairos-ubuntu", None), "kairos-ubuntu.raw");
    }

    #[test]
    fn an_explicit_volume_name_wins() {
        assert_eq!(
            volume_name("kairos-ubuntu", Some("golden-24.04.raw")),
            "golden-24.04.raw"
        );
    }

    #[test]
    fn the_raw_suffix_is_not_doubled() {
        assert_eq!(volume_name("base.raw", None), "base.raw");
    }

    #[test]
    fn volume_naming_is_deterministic() {
        // Re-running the Job must target the volume the first run created, or
        // `already_imported` can never be true and every retry re-uploads.
        assert_eq!(volume_name("img", None), volume_name("img", None));
    }

    // ---------- pool selection ------------------------------------------

    #[test]
    fn find_pool_returns_the_named_pool() {
        let pools = [pool("default", 1), pool("fast", 2)];
        let found = find_pool(&pools, "fast").expect("pool is present");
        assert_eq!(found.name, "fast");
        assert_eq!(found.uuid, [2; UUID_LEN]);
    }

    #[test]
    fn a_missing_pool_error_lists_what_the_host_does_have() {
        // The Job runs unattended; its failure message is the only diagnostic
        // an operator gets, so it must name the alternatives.
        let pools = [pool("default", 1), pool("fast", 2)];
        let err = find_pool(&pools, "nope").unwrap_err().to_string();
        assert!(err.contains("nope"), "{err} must name the pool asked for");
        assert!(err.contains("default"), "{err} must list available pools");
        assert!(err.contains("fast"), "{err} must list available pools");
    }

    #[test]
    fn a_host_with_no_pools_at_all_still_errors_clearly() {
        let err = find_pool(&[], "default").unwrap_err().to_string();
        assert!(err.contains("default"));
    }

    // ---------- idempotency ---------------------------------------------

    #[test]
    fn an_existing_volume_is_left_alone() {
        // backoffLimit is 1, so the retry re-runs this binary against a pool
        // that may already hold a complete volume. Re-uploading gigabytes —
        // or worse, failing because create returns "already exists" — would
        // make the retry useless.
        let vols = [vol("default", "other.raw"), vol("default", "img.raw")];
        match plan(&vols, "img.raw") {
            ImportPlan::AlreadyPresent { key } => {
                assert!(key.ends_with("img.raw"), "key {key} identifies the volume");
            }
            ImportPlan::Upload => panic!("must not re-upload an existing volume"),
        }
    }

    #[test]
    fn an_absent_volume_is_uploaded() {
        let vols = [vol("default", "other.raw")];
        assert!(matches!(plan(&vols, "img.raw"), ImportPlan::Upload));
    }

    #[test]
    fn an_empty_pool_is_uploaded_into() {
        assert!(matches!(plan(&[], "img.raw"), ImportPlan::Upload));
    }

    #[test]
    fn volume_matching_is_exact_not_a_prefix() {
        // `img.raw` must not be considered present because `img.raw.bak` is.
        let vols = [vol("default", "img.raw.bak")];
        assert!(matches!(plan(&vols, "img.raw"), ImportPlan::Upload));
    }

    // ---------- source sizing -------------------------------------------

    #[tokio::test]
    async fn source_length_is_the_files_real_size() {
        // The volume is created with the artifact's exact capacity: too small
        // truncates the guest disk, too large wastes the pool.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("disk.raw");
        tokio::fs::write(&path, vec![0u8; 4096])
            .await
            .expect("write source");

        assert_eq!(source_length(&path).await.expect("stat"), 4096);
    }

    #[tokio::test]
    async fn a_missing_source_names_the_path_it_could_not_read() {
        let err = source_length(std::path::Path::new("/nonexistent/disk.raw"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("/nonexistent/disk.raw"), "{err}");
    }

    #[tokio::test]
    async fn an_empty_source_is_rejected_before_touching_the_host() {
        // A zero-byte artifact means the build produced nothing; creating a
        // 0-capacity volume would "succeed" and leave an unbootable image.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("empty.raw");
        tokio::fs::write(&path, b"").await.expect("write source");

        let err = source_length(&path).await.unwrap_err().to_string();
        assert!(err.contains("empty"), "{err} must explain the zero length");
    }

    // ---------- checksum verification (SEC-004) --------------------------

    /// sha256("test") — well-known vector.
    const TEST_SHA256: &str =
        "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    /// sha512("test") — well-known vector.
    const TEST_SHA512: &str = "sha512:ee26b0dd4af7e749aa1a8ee3c10ae9923f618980772e473f8819a5d4940e0db27ac185f8a0e1d5f84f88bc887fd67b143732c304cc5fa9ad8e6f57f50028a8ff";

    async fn write_source(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("disk.raw");
        tokio::fs::write(&path, bytes).await.expect("write source");
        (dir, path)
    }

    #[tokio::test]
    async fn a_matching_sha256_passes() {
        let (_dir, path) = write_source(b"test").await;
        verify_checksum(&path, TEST_SHA256)
            .await
            .expect("must verify");
    }

    #[tokio::test]
    async fn a_matching_sha512_passes() {
        let (_dir, path) = write_source(b"test").await;
        verify_checksum(&path, TEST_SHA512)
            .await
            .expect("must verify");
    }

    #[tokio::test]
    async fn a_mismatch_fails_closed_naming_both_digests() {
        let (_dir, path) = write_source(b"tampered").await;
        let err = verify_checksum(&path, TEST_SHA256)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("mismatch"), "{err}");
        assert!(
            err.contains(TEST_SHA256),
            "{err} must name the expected value"
        );
    }

    #[tokio::test]
    async fn an_unsupported_algorithm_fails_closed() {
        // "md5" is a real algorithm — skipping unknown ones would let a
        // declared-but-unchecked checksum through, defeating the field.
        let (_dir, path) = write_source(b"test").await;
        let err = verify_checksum(&path, "md5:098f6bcd4621d373cade4e832627b4f6")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsupported algorithm"), "{err}");
    }

    #[tokio::test]
    async fn a_malformed_checksum_is_rejected() {
        let (_dir, path) = write_source(b"test").await;
        let err = verify_checksum(&path, "not-a-checksum")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("<alg>:<hex>"), "{err}");
    }
}
