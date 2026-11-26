// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue imagebuilder digest`: measure one build artifact and exit
//! (ADR-0010, amendment 2026-11-26).
//!
//! Runs inside the Job [`crate::reconciler::digest::build_digest_job`]
//! creates, beside the artifacts PVC. It streams the file once and writes
//! `sha256:<hex>` (and, when asked, `sha512:<hex>` on a second line) to the
//! termination-message file; that message is its only output, and the
//! reconciler validates it before recording it as
//! `VMImage.status.buildArtifact.checksum`. It never talks to the
//! Kubernetes API.
//!
//! Hashing is `sha2` (RustCrypto), as everywhere else in banlieue that hashes
//! a disk: ADR-0170 bans `ring`.

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use banlieue_oci::manifest::hex;
use clap::Args;
use sha2::{Digest as _, Sha256, Sha512};
use tracing::info;

/// Kubernetes' default `terminationMessagePath`.
const TERMINATION_LOG: &str = "/dev/termination-log";
/// Bytes per read: large enough that a multi-GiB raw disk is hashed at disk
/// speed, small enough to stay well inside the Job's memory.
pub const READ_BUFFER_BYTES: usize = 1024 * 1024;
/// Prefix of a SHA-256 digest.
pub const SHA256_PREFIX: &str = "sha256:";
/// Prefix of a SHA-512 digest.
pub const SHA512_PREFIX: &str = "sha512:";

/// Arguments for `banlieue imagebuilder digest`.
#[derive(Debug, Args)]
pub struct DigestArgs {
    /// The artifact to measure.
    #[arg(long)]
    pub file: PathBuf,

    /// Also report SHA-512, for a source that declares its checksum as
    /// `sha512:<hex>`. SHA-256 is always reported.
    #[arg(long, default_value_t = false)]
    pub sha512: bool,

    /// Where to write the digests.
    #[arg(long, default_value = TERMINATION_LOG)]
    pub termination_log: PathBuf,
}

/// The digests of one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDigests {
    /// `sha256:<64 lowercase hex>`.
    pub sha256: String,
    /// `sha512:<128 lowercase hex>`, when asked for.
    pub sha512: Option<String>,
}

impl FileDigests {
    /// The termination message: SHA-256 first, then SHA-512 if present, one
    /// per line.
    #[must_use]
    pub fn to_message(&self) -> String {
        match &self.sha512 {
            Some(sha512) => format!("{}\n{sha512}", self.sha256),
            None => self.sha256.clone(),
        }
    }
}

/// Hash everything `reader` yields, in [`READ_BUFFER_BYTES`] chunks.
///
/// # Arguments
/// * `reader` - the bytes to hash.
/// * `sha512` - whether to compute SHA-512 as well.
///
/// # Errors
/// Any read error.
pub fn digest_reader(mut reader: impl Read, sha512: bool) -> std::io::Result<FileDigests> {
    let mut sha256_hasher = Sha256::new();
    let mut sha512_hasher = sha512.then(Sha512::new);
    let mut buf = vec![0u8; READ_BUFFER_BYTES];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        sha256_hasher.update(&buf[..n]);
        if let Some(h) = sha512_hasher.as_mut() {
            h.update(&buf[..n]);
        }
    }
    Ok(FileDigests {
        sha256: format!("{SHA256_PREFIX}{}", hex(&sha256_hasher.finalize())),
        sha512: sha512_hasher.map(|h| format!("{SHA512_PREFIX}{}", hex(&h.finalize()))),
    })
}

/// Measure `args.file` and write the result to `args.termination_log`.
///
/// # Errors
/// The file cannot be opened or read, or the log cannot be written; the Job
/// then fails.
pub fn run_blocking(args: &DigestArgs) -> Result<FileDigests> {
    let file = std::fs::File::open(&args.file)
        .with_context(|| format!("opening {}", args.file.display()))?;
    let digests = digest_reader(file, args.sha512)
        .with_context(|| format!("reading {}", args.file.display()))?;
    let message = digests.to_message();
    std::fs::write(&args.termination_log, &message)
        .with_context(|| format!("writing {}", args.termination_log.display()))?;
    Ok(digests)
}

/// Run `banlieue imagebuilder digest`: hash on a blocking thread, then log
/// and print the result.
///
/// # Errors
/// As [`run_blocking`].
pub async fn run(args: DigestArgs) -> Result<()> {
    info!(file = %args.file.display(), sha512 = args.sha512, "measuring build artifact");
    let digests = tokio::task::spawn_blocking(move || run_blocking(&args))
        .await
        .context("digest task")??;
    info!(sha256 = %digests.sha256, "measured");
    println!("{}", digests.to_message());
    Ok(())
}

#[cfg(test)]
#[path = "file_digest_tests.rs"]
mod file_digest_tests;
