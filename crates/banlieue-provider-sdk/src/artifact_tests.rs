// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::artifact`].

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::super::*;

    /// sha256("test"), a well-known vector.
    const TEST_SHA256: &str =
        "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    /// sha512("test"), a well-known vector.
    const TEST_SHA512: &str = "sha512:ee26b0dd4af7e749aa1a8ee3c10ae9923f618980772e473f8819a5d4940e0db27ac185f8a0e1d5f84f88bc887fd67b143732c304cc5fa9ad8e6f57f50028a8ff";
    const SOURCE_BYTES: usize = 4096;
    const MISSING: &str = "/nonexistent/disk.raw";

    async fn write_source(bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("disk.raw");
        tokio::fs::write(&path, bytes).await.expect("write source");
        (dir, path)
    }

    // ---------- source_length ---------------------------------------------

    #[tokio::test]
    async fn source_length_is_the_files_real_size() {
        let (_dir, path) = write_source(&vec![0u8; SOURCE_BYTES]).await;
        assert_eq!(
            source_length(&path).await.expect("stat"),
            SOURCE_BYTES as u64
        );
    }

    #[tokio::test]
    async fn a_missing_source_names_the_path_it_could_not_read() {
        let err = source_length(Path::new(MISSING)).await.unwrap_err();
        assert!(
            matches!(&err, SourceError::Unreadable { path, .. } if path == Path::new(MISSING)),
            "{err:?}"
        );
        assert!(err.to_string().contains(MISSING), "{err}");
    }

    /// A zero-byte artifact means the build produced nothing; importing it
    /// would "succeed" and leave an unbootable image.
    #[tokio::test]
    async fn an_empty_source_is_rejected() {
        let (_dir, path) = write_source(b"").await;
        let err = source_length(&path).await.unwrap_err();
        assert!(
            matches!(&err, SourceError::Empty { path: p } if p == &path),
            "{err:?}"
        );
        assert!(err.to_string().contains("empty"), "{err}");
    }

    // ---------- verify_checksum (SEC-004) -----------------------------------

    #[tokio::test]
    async fn a_matching_sha256_passes_and_names_its_algorithm() {
        let (_dir, path) = write_source(b"test").await;
        assert_eq!(
            verify_checksum(&path, TEST_SHA256).await.expect("verifies"),
            ALGORITHM_SHA256
        );
    }

    #[tokio::test]
    async fn a_matching_sha512_passes_and_names_its_algorithm() {
        let (_dir, path) = write_source(b"test").await;
        assert_eq!(
            verify_checksum(&path, TEST_SHA512).await.expect("verifies"),
            ALGORITHM_SHA512
        );
    }

    #[tokio::test]
    async fn the_hex_comparison_ignores_case() {
        let (_dir, path) = write_source(b"test").await;
        let (alg, hex) = TEST_SHA256.split_once(':').expect("vector");
        let upper = format!("{alg}:{}", hex.to_uppercase());
        verify_checksum(&path, &upper).await.expect("verifies");
    }

    /// The mismatch carries everything a caller needs to word its own
    /// refusal: the path, the whole expected value and the real digest.
    #[tokio::test]
    async fn a_mismatch_fails_closed_carrying_both_digests() {
        let (_dir, path) = write_source(b"tampered").await;
        let err = verify_checksum(&path, TEST_SHA256).await.unwrap_err();
        let ChecksumError::Mismatch {
            path: at,
            expected,
            algorithm,
            actual_hex,
        } = &err
        else {
            panic!("expected a mismatch, got {err:?}");
        };
        assert_eq!(at, &path);
        assert_eq!(expected, TEST_SHA256);
        assert_eq!(*algorithm, ALGORITHM_SHA256);
        assert!(!TEST_SHA256.ends_with(actual_hex.as_str()));
        let text = err.to_string();
        assert!(
            text.contains("mismatch") && text.contains(TEST_SHA256),
            "{text}"
        );
    }

    /// "md5" is a real algorithm. Skipping unknown ones would let a
    /// declared-but-unchecked checksum through, defeating the field.
    #[tokio::test]
    async fn an_unsupported_algorithm_fails_closed() {
        let (_dir, path) = write_source(b"test").await;
        let err = verify_checksum(&path, "md5:098f6bcd4621d373cade4e832627b4f6")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ChecksumError::Unsupported { algorithm } if algorithm == "md5"),
            "{err:?}"
        );
        assert!(err.to_string().contains("unsupported algorithm"), "{err}");
    }

    #[tokio::test]
    async fn a_malformed_checksum_is_rejected() {
        let (_dir, path) = write_source(b"test").await;
        let err = verify_checksum(&path, "not-a-checksum").await.unwrap_err();
        assert!(
            matches!(&err, ChecksumError::Malformed { expected } if expected == "not-a-checksum"),
            "{err:?}"
        );
        assert!(err.to_string().contains("<alg>:<hex>"), "{err}");
    }

    /// The shape of the expected value is checked before the file is opened,
    /// and the file before the algorithm: the order both importers had.
    #[tokio::test]
    async fn checks_run_in_a_fixed_order() {
        let missing = Path::new(MISSING);
        assert!(matches!(
            verify_checksum(missing, "not-a-checksum").await,
            Err(ChecksumError::Malformed { .. })
        ));
        assert!(matches!(
            verify_checksum(missing, "md5:00").await,
            Err(ChecksumError::Open { .. })
        ));
    }

    // ---------- digest_hex / hex_encode ------------------------------------

    /// More than one chunk, and not a whole number of them, so the loop's
    /// final short read is exercised.
    #[tokio::test]
    async fn a_file_larger_than_one_chunk_hashes_like_the_whole() {
        use sha2::Digest;

        let bytes = vec![0x5a_u8; HASH_CHUNK_BYTES + SOURCE_BYTES];
        let (_dir, path) = write_source(&bytes).await;
        let mut file = tokio::fs::File::open(&path).await.expect("open");
        assert_eq!(
            digest_hex::<sha2::Sha256>(&mut file).await.expect("hash"),
            hex_encode(&sha2::Sha256::digest(&bytes))
        );
    }

    #[test]
    fn hex_is_lowercase_and_zero_padded() {
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xab, 0xff]), "000fabff");
        assert_eq!(hex_encode(&[]), "");
    }
}
