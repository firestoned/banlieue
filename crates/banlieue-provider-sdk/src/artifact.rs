// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Checks on a built image file before a provider imports it: that it is
//! not empty, and that it matches its declared checksum.
//!
//! The errors are data, not prose: each provider's `import` subcommand words
//! its own failure (`--source`, "the build produced no ISO"), so the variants
//! carry the path, the algorithm and the digests for the caller to format.
//! Their `Display` is a neutral default.

use std::path::{Path, PathBuf};

use sha2::Digest;
use tokio::fs::File;
use tokio::io::AsyncReadExt;

/// Bytes hashed per read: large enough that a multi-gigabyte image is a few
/// thousand reads, small enough to stay off the heap's large-object path.
pub const HASH_CHUNK_BYTES: usize = 1024 * 1024;

/// Checksum algorithm name for SHA-256.
pub const ALGORITHM_SHA256: &str = "sha256";

/// Checksum algorithm name for SHA-512.
pub const ALGORITHM_SHA512: &str = "sha512";

/// Separator between algorithm and digest in `<alg>:<hex>`.
const CHECKSUM_SEPARATOR: char = ':';

/// Why [`source_length`] refused a file.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The file's metadata could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Unreadable {
        /// The file.
        path: PathBuf,
        /// The failed `stat`.
        #[source]
        source: std::io::Error,
    },
    /// The file is zero bytes long: the build produced nothing.
    #[error("{} is empty", path.display())]
    Empty {
        /// The file.
        path: PathBuf,
    },
}

/// Why [`verify_checksum`] refused a file.
#[derive(Debug, thiserror::Error)]
pub enum ChecksumError {
    /// The expected value is not `<alg>:<hex>`.
    #[error("expected `<alg>:<hex>`, got {expected:?}")]
    Malformed {
        /// The value as given.
        expected: String,
    },
    /// The file could not be opened.
    #[error("cannot read {}: {source}", path.display())]
    Open {
        /// The file.
        path: PathBuf,
        /// The failed open.
        #[source]
        source: std::io::Error,
    },
    /// A read failed part-way through hashing.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// The failed read.
        #[source]
        source: std::io::Error,
    },
    /// The algorithm is not one this build can compute. An error and never a
    /// skip: a declared checksum that cannot be checked must block the
    /// import (SEC-004).
    #[error("unsupported algorithm {algorithm:?} (supported: sha256, sha512)")]
    Unsupported {
        /// The algorithm as given.
        algorithm: String,
    },
    /// The file's digest is not the expected one.
    #[error(
        "checksum mismatch for {}: expected {expected}, computed {algorithm}:{actual_hex}",
        path.display()
    )]
    Mismatch {
        /// The file.
        path: PathBuf,
        /// The whole expected value, `<alg>:<hex>`.
        expected: String,
        /// The algorithm that was computed.
        algorithm: &'static str,
        /// The digest the file really has, lowercase hex.
        actual_hex: String,
    },
}

/// Size of the artifact at `path`, in bytes.
///
/// # Errors
/// [`SourceError::Unreadable`] when the file cannot be read, and
/// [`SourceError::Empty`] for a zero-byte file: importing one would report
/// success and leave an unbootable image behind.
pub async fn source_length(path: &Path) -> Result<u64, SourceError> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|source| SourceError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
    let len = meta.len();
    if len == 0 {
        return Err(SourceError::Empty {
            path: path.to_path_buf(),
        });
    }
    Ok(len)
}

/// Verify the file at `path` against an expected `<alg>:<hex>` checksum.
///
/// Streams the file, so a multi-gigabyte image never sits in memory. Fails
/// **closed**: an unsupported algorithm is an error, not a skip (SEC-004).
/// The hex comparison ignores case.
///
/// # Returns
/// The algorithm that was verified, for the caller's log line.
///
/// # Errors
/// A [`ChecksumError`] naming which check failed. The order is fixed: the
/// expected value's shape, then opening the file, then the algorithm, then
/// the digest.
pub async fn verify_checksum(path: &Path, expected: &str) -> Result<&'static str, ChecksumError> {
    let (algorithm, expected_hex) =
        expected
            .split_once(CHECKSUM_SEPARATOR)
            .ok_or_else(|| ChecksumError::Malformed {
                expected: expected.to_string(),
            })?;

    let mut file = File::open(path)
        .await
        .map_err(|source| ChecksumError::Open {
            path: path.to_path_buf(),
            source,
        })?;

    let (algorithm, digest) = match algorithm {
        ALGORITHM_SHA256 => (
            ALGORITHM_SHA256,
            digest_hex::<sha2::Sha256>(&mut file).await,
        ),
        ALGORITHM_SHA512 => (
            ALGORITHM_SHA512,
            digest_hex::<sha2::Sha512>(&mut file).await,
        ),
        other => {
            return Err(ChecksumError::Unsupported {
                algorithm: other.to_string(),
            });
        }
    };
    let actual_hex = digest.map_err(|source| ChecksumError::Read {
        path: path.to_path_buf(),
        source,
    })?;

    if !actual_hex.eq_ignore_ascii_case(expected_hex) {
        return Err(ChecksumError::Mismatch {
            path: path.to_path_buf(),
            expected: expected.to_string(),
            algorithm,
            actual_hex,
        });
    }
    Ok(algorithm)
}

/// Hash the rest of `file` with `D`, in [`HASH_CHUNK_BYTES`] reads, and
/// return the digest as lowercase hex.
///
/// # Errors
/// The first failed read.
pub async fn digest_hex<D: Digest>(file: &mut File) -> std::io::Result<String> {
    let mut hasher = D::new();
    let mut buf = vec![0u8; HASH_CHUNK_BYTES];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

/// Lowercase hex encoding of a digest. `sha2`'s output (a
/// `hybrid_array::Array`, as of `digest` 0.11) does not implement `LowerHex`,
/// so `format!("{:x}", ..)` is not available for it.
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod artifact_tests;
