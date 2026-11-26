// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Measuring a finished build (ADR-0010, amendment 2026-11-26).
//!
//! `VMImage.status.buildArtifact.checksum` is the `sha256:<hex>` of the file
//! the build produced, measured by banlieue, not a value copied from the
//! source: a `Url` source built from an OCI image cannot know its raw disk's
//! hash in advance, and a cloud publisher (ADR-0162 Decision 6) needs one to
//! name and verify what it uploads.
//!
//! Once an `OSArtifact` is `Ready` (and no pre-lay runs: a pre-lay Job
//! reports its own file's digest, ADR-0113), the reconciler creates a Job
//! beside the artifacts PVC that runs `banlieue imagebuilder digest`. Until
//! its result is read and validated the build is `Building`
//! ([`REASON_DIGEST_PENDING`]) with no `file` and no `checksum`, so no import
//! Job, registry push or publish starts on an unmeasured file. A checksum the
//! source declares is still verified, now here: a mismatch fails the build
//! closed (SEC-004).
//!
//! Why not reuse the push Job (ADR-0064)? It reports a manifest reference
//! whose layer digest is of the gzip-compressed copy, not of the raw bytes,
//! and it only runs for a `cloud-hypervisor` source; so a separate Job
//! measures every build the same way, and the push starts after it.
//!
//! Everything here is pure; the reconciler does the API calls.

use std::fmt;

use banlieue_api::banlieue::{BuildArtifactPhase, BuildArtifactStatus};
use banlieue_provider_sdk::naming::truncate_with_hash;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Toleration;
use serde_json::{Value, json};

use super::job::{JobParts, job_parts};
use crate::file_digest::{SHA256_PREFIX, SHA512_PREFIX};

const DIGEST_JOB_SUFFIX: &str = "-digest";
const ARTIFACTS_MOUNT_PATH: &str = "/artifacts";
const VOLUME_ARTIFACTS: &str = "artifacts";
/// The distroless nonroot uid the banlieue image runs as.
const NONROOT_UID: i64 = 65532;
const SHA256_HEX_LEN: usize = 64;
const SHA512_HEX_LEN: usize = 128;

/// `buildArtifact.reason` while the build is being measured.
pub const REASON_DIGEST_PENDING: &str = "DigestPending";
/// `buildArtifact.reason` when the digest Job failed or reported garbage.
pub const REASON_DIGEST_FAILED: &str = "DigestFailed";
/// `buildArtifact.reason` when the built file does not match the checksum
/// the source declares (SEC-004).
pub const REASON_CHECKSUM_MISMATCH: &str = "ChecksumMismatch";
/// `buildArtifact.reason` when the source declares a checksum banlieue
/// cannot verify.
pub const REASON_CHECKSUM_UNSUPPORTED: &str = "ChecksumUnsupported";

/// Deterministic digest Job name for an `OSArtifact`.
#[must_use]
pub fn digest_job_name(os_artifact_name: &str) -> String {
    truncate_with_hash(&format!("{os_artifact_name}{DIGEST_JOB_SUFFIX}"))
}

/// A hash algorithm a source's `checksum` may name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChecksumAlgorithm {
    /// `sha256:<64 hex>`.
    Sha256,
    /// `sha512:<128 hex>`.
    Sha512,
}

impl ChecksumAlgorithm {
    fn prefix(self) -> &'static str {
        match self {
            Self::Sha256 => SHA256_PREFIX,
            Self::Sha512 => SHA512_PREFIX,
        }
    }

    fn hex_len(self) -> usize {
        match self {
            Self::Sha256 => SHA256_HEX_LEN,
            Self::Sha512 => SHA512_HEX_LEN,
        }
    }
}

/// A source's declared checksum, normalised to lowercase hex.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredChecksum {
    /// Its algorithm.
    pub algorithm: ChecksumAlgorithm,
    /// Its lowercase hex.
    pub hex: String,
}

impl fmt::Display for DeclaredChecksum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.algorithm.prefix(), self.hex)
    }
}

fn is_hex_of_len(hex: &str, len: usize) -> bool {
    hex.len() == len && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_lower_hex_of_len(hex: &str, len: usize) -> bool {
    is_hex_of_len(hex, len) && !hex.bytes().any(|b| b.is_ascii_uppercase())
}

/// Parse `ImageSource.checksum`: `sha256:<64 hex>` or `sha512:<128 hex>`,
/// hex in either case.
///
/// # Errors
/// A message naming the problem for any other algorithm or length.
pub fn parse_declared_checksum(value: &str) -> Result<DeclaredChecksum, String> {
    let value = value.trim();
    let algorithm = [ChecksumAlgorithm::Sha256, ChecksumAlgorithm::Sha512]
        .into_iter()
        .find(|a| value.starts_with(a.prefix()))
        .ok_or_else(|| format!("{value:?}: only sha256:<hex> and sha512:<hex> are supported"))?;
    let hex = &value[algorithm.prefix().len()..];
    if !is_hex_of_len(hex, algorithm.hex_len()) {
        return Err(format!(
            "{value:?}: expected {} hex digits after {}",
            algorithm.hex_len(),
            algorithm.prefix()
        ));
    }
    Ok(DeclaredChecksum {
        algorithm,
        hex: hex.to_ascii_lowercase(),
    })
}

/// Everything one digest Job needs.
#[derive(Debug)]
pub struct DigestJobInputs<'a> {
    /// From [`digest_job_name`].
    pub job_name: &'a str,
    /// The build namespace, where the PVC is.
    pub namespace: &'a str,
    /// `VMImage` name, for labels.
    pub vmimage: &'a str,
    /// Image the Job runs: the banlieue image (`--push-image`).
    pub image: &'a str,
    /// The `Ready` build artifact.
    pub artifact: &'a BuildArtifactStatus,
    /// The live `OSArtifact`'s uid: the Job's owner.
    pub os_artifact_uid: &'a str,
    /// Also measure SHA-512: the source declares a `sha512:` checksum.
    pub sha512: bool,
    /// Taints the Job may tolerate: the PVC may live on a tainted build
    /// node. Placement itself follows the PVC.
    pub tolerations: &'a [Toleration],
}

/// Build the digest Job.
///
/// The push Job's shape (ADR-0064): no ServiceAccount token (it never talks
/// to the API server), non-root, seccomp `RuntimeDefault`, a read-only root
/// filesystem, no privilege escalation, every capability dropped, and the
/// artifacts PVC mounted read-only. Owned by the `OSArtifact` so a rebuild
/// garbage-collects it (ADR-0027). No `ttlSecondsAfterFinished`: the
/// termination message is its only output and must outlive the reconcile
/// that reads it. One retry, for a transient read error.
#[must_use]
pub fn build_digest_job(inputs: &DigestJobInputs<'_>) -> Value {
    let DigestJobInputs {
        job_name,
        namespace,
        vmimage,
        image,
        artifact,
        os_artifact_uid,
        sha512,
        tolerations,
    } = *inputs;

    let JobParts {
        pvc,
        file,
        labels,
        owner_references,
    } = job_parts("digest", vmimage, artifact, os_artifact_uid);

    let mut args = vec![
        "imagebuilder".to_string(),
        "digest".to_string(),
        "--file".to_string(),
        format!("{ARTIFACTS_MOUNT_PATH}/{file}"),
    ];
    if sha512 {
        args.push("--sha512".to_string());
    }

    json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": job_name,
            "namespace": namespace,
            "labels": labels,
            "ownerReferences": owner_references,
        },
        "spec": {
            "backoffLimit": 1,
            "template": {
                "metadata": { "labels": labels },
                "spec": {
                    "restartPolicy": "Never",
                    "automountServiceAccountToken": false,
                    "tolerations": (!tolerations.is_empty())
                        .then(|| serde_json::to_value(tolerations).unwrap_or(Value::Null)),
                    "securityContext": {
                        "runAsNonRoot": true,
                        "runAsUser": NONROOT_UID,
                        "seccompProfile": { "type": "RuntimeDefault" }
                    },
                    "containers": [{
                        "name": "digest",
                        "image": image,
                        "args": args,
                        "terminationMessagePolicy": "File",
                        "volumeMounts": [
                            { "name": VOLUME_ARTIFACTS, "mountPath": ARTIFACTS_MOUNT_PATH,
                              "readOnly": true }
                        ],
                        "securityContext": {
                            "allowPrivilegeEscalation": false,
                            "readOnlyRootFilesystem": true,
                            "capabilities": { "drop": ["ALL"] }
                        }
                    }],
                    "volumes": [
                        { "name": VOLUME_ARTIFACTS,
                          "persistentVolumeClaim": { "claimName": pvc, "readOnly": true } }
                    ],
                }
            }
        }
    })
}

/// What a digest Job measured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeasuredDigests {
    /// `sha256:<64 lowercase hex>`: what `buildArtifact.checksum` records.
    pub sha256: String,
    /// `sha512:<128 lowercase hex>`, when asked for.
    pub sha512: Option<String>,
}

fn check_line(line: &str, algorithm: ChecksumAlgorithm) -> Result<String, String> {
    let hex = line
        .strip_prefix(algorithm.prefix())
        .ok_or_else(|| format!("{line:?} is not a {} digest", algorithm.prefix()))?;
    if !is_lower_hex_of_len(hex, algorithm.hex_len()) {
        return Err(format!(
            "{line:?} is not {} lowercase hex digits",
            algorithm.hex_len()
        ));
    }
    Ok(line.to_string())
}

/// Check a digest Job's termination message: exactly a `sha256:` line,
/// optionally followed by a `sha512:` line, lowercase hex of the right
/// lengths. Anything else is refused, so a confused Job cannot put
/// arbitrary text into `status`.
///
/// # Errors
/// A message saying what was wrong.
pub fn parse_digest_result(message: &str) -> Result<MeasuredDigests, String> {
    let mut lines = message.trim().lines();
    let sha256 = check_line(lines.next().unwrap_or_default(), ChecksumAlgorithm::Sha256)?;
    let sha512 = lines
        .next()
        .map(|l| check_line(l, ChecksumAlgorithm::Sha512))
        .transpose()?;
    if let Some(extra) = lines.next() {
        return Err(format!("unexpected line {extra:?}"));
    }
    Ok(MeasuredDigests { sha256, sha512 })
}

/// Where measuring a build is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DigestOutcome {
    /// The Job is running (or was just created or replaced).
    Running,
    /// The Job succeeded but its pod, and so its result, is gone: run it
    /// again.
    ResultLost,
    /// The Job reported these digests.
    Done(MeasuredDigests),
    /// This build's digest was already measured, verified and recorded.
    Carried(String),
    /// The Job failed, or reported something that is not a digest.
    Failed(String),
}

/// Map a digest Job, and its pod's termination message once read, to an
/// outcome.
#[must_use]
pub fn digest_outcome(job: &Job, termination_message: Option<&str>) -> DigestOutcome {
    let status = job.status.as_ref();
    if status.and_then(|s| s.succeeded).unwrap_or(0) > 0 {
        return match termination_message.map(parse_digest_result) {
            Some(Ok(digests)) => DigestOutcome::Done(digests),
            Some(Err(why)) => DigestOutcome::Failed(format!("digest Job reported {why}")),
            None => DigestOutcome::ResultLost,
        };
    }
    if status.and_then(|s| s.failed).unwrap_or(0) > 0 {
        return DigestOutcome::Failed(
            "digest Job failed: the built file could not be read; see its pod's log".to_string(),
        );
    }
    DigestOutcome::Running
}

/// The checksum already recorded for this build, if the previous status is
/// a `Ready`, measured (not pre-laid) result for the same `OSArtifact` uid.
/// Lets a reconcile that finds the Job succeeded but its pod gone keep the
/// result instead of flapping the image back to `Building`. Only a SHA-256
/// is recorded, so a source declaring `sha512:` measures again instead.
#[must_use]
pub fn carried_digest(
    previous: Option<&BuildArtifactStatus>,
    os_artifact_uid: &str,
) -> Option<String> {
    let previous = previous?;
    if previous.phase != BuildArtifactPhase::Ready
        || previous.prelaid_disk_size_gi_b.is_some()
        || previous.os_artifact_uid.as_deref() != Some(os_artifact_uid)
    {
        return None;
    }
    previous.checksum.clone()
}

fn fail(status: &mut BuildArtifactStatus, reason: &str, message: String) {
    status.phase = BuildArtifactPhase::Failed;
    status.reason = Some(reason.to_string());
    status.message = Some(message);
    status.file = None;
    status.checksum = None;
}

/// Whether `measured` satisfies `declared`.
fn mismatch(measured: &MeasuredDigests, declared: &DeclaredChecksum) -> Option<String> {
    let want = declared.to_string();
    let got = match declared.algorithm {
        ChecksumAlgorithm::Sha256 => Some(measured.sha256.as_str()),
        ChecksumAlgorithm::Sha512 => measured.sha512.as_deref(),
    };
    match got {
        Some(got) if got == want => None,
        Some(got) => Some(format!(
            "the built file is {got}, but the source declares {want} (SEC-004)"
        )),
        None => Some(format!(
            "the source declares {want}, but the digest Job did not measure {}",
            declared.algorithm.prefix()
        )),
    }
}

/// Fold a digest outcome into a `Ready` build's status. Only `Done` (with a
/// matching declared checksum, if any) and `Carried` leave it `Ready`, with
/// `checksum` set to the measured `sha256:<hex>`.
pub fn apply_digest(
    status: &mut BuildArtifactStatus,
    outcome: &DigestOutcome,
    declared: Option<&DeclaredChecksum>,
) {
    match outcome {
        DigestOutcome::Running | DigestOutcome::ResultLost => {
            status.phase = BuildArtifactPhase::Building;
            status.reason = Some(REASON_DIGEST_PENDING.to_string());
            status.message =
                Some("measuring the built file's sha256 before anything uses it".to_string());
            status.file = None;
            status.checksum = None;
        }
        DigestOutcome::Done(measured) => {
            if let Some(why) = declared.and_then(|d| mismatch(measured, d)) {
                fail(status, REASON_CHECKSUM_MISMATCH, why);
                return;
            }
            status.checksum = Some(measured.sha256.clone());
        }
        DigestOutcome::Carried(checksum) => {
            let carried = MeasuredDigests {
                sha256: checksum.clone(),
                sha512: None,
            };
            if let Some(why) = declared.and_then(|d| mismatch(&carried, d)) {
                fail(status, REASON_CHECKSUM_MISMATCH, why);
                return;
            }
            status.checksum = Some(checksum.clone());
        }
        DigestOutcome::Failed(why) => fail(status, REASON_DIGEST_FAILED, why.clone()),
    }
}

/// Fold an unusable declared checksum into a build's status: `Failed`,
/// before any Job runs, rather than publishing an artifact nothing could
/// verify (SEC-004 fails closed on an unsupported algorithm).
pub fn apply_checksum_refusal(status: &mut BuildArtifactStatus, declared: &str, why: &str) {
    fail(
        status,
        REASON_CHECKSUM_UNSUPPORTED,
        format!("the source's checksum {declared:?} cannot be verified: {why}"),
    );
}

#[cfg(test)]
#[path = "digest_tests.rs"]
mod digest_tests;
