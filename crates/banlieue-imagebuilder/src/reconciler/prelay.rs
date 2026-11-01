// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Pre-laying a finished raw build (ADR-0113, Amendment 2026-11-01).
//!
//! When a `VMImage` asks for `spec.prelay` and its `OSArtifact` is `Ready`,
//! the reconciler creates a Job beside the artifacts PVC that runs
//! `scripts/prelay-kairos-raw.sh` from the operator's `--prelay-image`: it
//! verifies the built raw file against the source's checksum, boots it once
//! through the Kairos recovery reset under QEMU (no TPM, no network), writes
//! the result to a new file on the same PVC and reports that file's
//! `sha256:<hex>` as its termination message. Until then the artifact stays
//! `Building` (`Prelaying`), so no import Job or registry push starts on the
//! raw file; afterwards it is `Ready` with the pre-laid file and the digest.
//!
//! Everything here is pure; the reconciler does the API calls.

use std::collections::BTreeMap;

use banlieue_api::banlieue::{BuildArtifactKind, BuildArtifactPhase, BuildArtifactStatus};
use banlieue_provider_sdk::naming::truncate_with_hash;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Toleration;
use serde_json::{Value, json};

const PRELAY_JOB_SUFFIX: &str = "-prelay";
const PRELAID_FILE_SUFFIX: &str = "-prelaid.raw";
const ARTIFACTS_MOUNT_PATH: &str = "/artifacts";
const VOLUME_ARTIFACTS: &str = "artifacts";
/// The script inside `--prelay-image` (`Dockerfile.prelay`).
const PRELAY_COMMAND: &str = "/usr/local/bin/prelay-kairos-raw.sh";
/// Where the script writes the result's digest (`PRELAY_RESULT_FILE`).
const TERMINATION_LOG: &str = "/dev/termination-log";
const DIGEST_PREFIX: &str = "sha256:";
const SHA256_HEX_LEN: usize = 64;

/// `buildArtifact.reason` while the pre-lay Job runs.
pub const REASON_PRELAYING: &str = "Prelaying";
/// `buildArtifact.reason` when the pre-lay Job failed.
pub const REASON_PRELAY_FAILED: &str = "PrelayFailed";

/// Deterministic pre-lay Job name for an `OSArtifact`.
#[must_use]
pub fn prelay_job_name(os_artifact_name: &str) -> String {
    truncate_with_hash(&format!("{os_artifact_name}{PRELAY_JOB_SUFFIX}"))
}

/// The pre-laid file the Job writes beside the built one. The built file is
/// never modified.
#[must_use]
pub fn prelaid_file_name(os_artifact_name: &str) -> String {
    format!("{os_artifact_name}{PRELAID_FILE_SUFFIX}")
}

/// Why a `spec.prelay` request cannot be honoured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrelayRefusal {
    /// The build is an `iso`: only a raw `cloudImage` has a recovery reset to
    /// do once.
    NeedsCloudImage,
    /// `--prelay-image` is not set: refused rather than shipping the raw file
    /// as if it were pre-laid.
    ImageUnset,
}

impl PrelayRefusal {
    /// The `buildArtifact.reason` for this refusal.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::NeedsCloudImage => "PrelayNeedsCloudImage",
            Self::ImageUnset => "PrelayImageUnset",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::NeedsCloudImage => {
                "spec.prelay needs a raw cloudImage build; this source builds an iso (ADR-0113)"
            }
            Self::ImageUnset => {
                "spec.prelay needs banlieue-imagebuilder --prelay-image, an image carrying \
                 prelay-kairos-raw.sh and QEMU (Dockerfile.prelay, ADR-0113)"
            }
        }
    }
}

/// Whether a `spec.prelay` request for a build of `kind` can run, given the
/// configured `--prelay-image`.
#[must_use]
pub fn prelay_refusal(
    kind: &BuildArtifactKind,
    prelay_image: Option<&str>,
) -> Option<PrelayRefusal> {
    if *kind != BuildArtifactKind::CloudImage {
        return Some(PrelayRefusal::NeedsCloudImage);
    }
    if prelay_image.is_none_or(str::is_empty) {
        return Some(PrelayRefusal::ImageUnset);
    }
    None
}

/// Everything one pre-lay Job needs.
#[derive(Debug)]
pub struct PrelayJobInputs<'a> {
    /// From [`prelay_job_name`].
    pub job_name: &'a str,
    /// The build namespace, where the PVC is.
    pub namespace: &'a str,
    /// `VMImage` name, for labels.
    pub vmimage: &'a str,
    /// `--prelay-image`.
    pub image: &'a str,
    /// The `OSArtifact`'s finished build (`Ready`, with its PVC and file).
    pub artifact: &'a BuildArtifactStatus,
    /// The live `OSArtifact`'s uid: the Job's owner.
    pub os_artifact_uid: &'a str,
    /// `spec.prelay.diskSizeGiB`.
    pub disk_size_gib: u32,
    /// Build tolerations (`--build-toleration`).
    pub tolerations: &'a [Toleration],
    /// Build node selector (`--build-node-selector`): where KVM is.
    pub node_selector: &'a BTreeMap<String, String>,
}

/// Build the pre-lay Job.
///
/// Privileged (loop devices, mounts and `/dev/kvm`, as the build namespace's
/// kairos-operator builds already are, ADR-0016), with no ServiceAccount
/// token: it never talks to the API server. It mounts the artifacts PVC
/// read-write to write the pre-laid file beside the built one. Owned by the
/// `OSArtifact`, so a rebuild garbage-collects it (ADR-0027). No retry and no
/// `ttlSecondsAfterFinished`: a failed boot is reported, not repeated, and
/// the termination message must outlive the reconcile that reads it.
#[must_use]
pub fn build_prelay_job(inputs: &PrelayJobInputs<'_>) -> Value {
    let PrelayJobInputs {
        job_name,
        namespace,
        vmimage,
        image,
        artifact,
        os_artifact_uid,
        disk_size_gib,
        tolerations,
        node_selector,
    } = *inputs;

    let pvc = artifact
        .pvc_ref
        .as_ref()
        .map(|r| r.name.clone())
        .unwrap_or_default();
    let file = artifact.file.clone().unwrap_or_default();

    let mut env = vec![
        json!({ "name": "PRELAY_INPUT", "value": format!("{ARTIFACTS_MOUNT_PATH}/{file}") }),
        json!({ "name": "PRELAY_OUTPUT",
                "value": format!("{ARTIFACTS_MOUNT_PATH}/{}", prelaid_file_name(&artifact.os_artifact_ref)) }),
        json!({ "name": "PRELAY_DISK_SIZE_GIB", "value": disk_size_gib.to_string() }),
        json!({ "name": "PRELAY_RESULT_FILE", "value": TERMINATION_LOG }),
    ];
    if let Some(checksum) = &artifact.checksum {
        env.push(json!({ "name": "PRELAY_VERIFY_CHECKSUM", "value": checksum }));
    }

    let labels = BTreeMap::from([
        ("app.kubernetes.io/name".to_string(), "banlieue".to_string()),
        (
            "app.kubernetes.io/component".to_string(),
            "prelay".to_string(),
        ),
        ("banlieue.io/vmimage".to_string(), vmimage.to_string()),
    ]);
    let owner_references = banlieue_provider_sdk::osartifact::owner_references(
        &artifact.os_artifact_ref,
        Some(os_artifact_uid),
    );

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
            "backoffLimit": 0,
            "template": {
                "metadata": { "labels": labels },
                "spec": {
                    "restartPolicy": "Never",
                    "automountServiceAccountToken": false,
                    "nodeSelector": (!node_selector.is_empty()).then_some(node_selector),
                    "tolerations": (!tolerations.is_empty())
                        .then(|| serde_json::to_value(tolerations).unwrap_or(Value::Null)),
                    "containers": [{
                        "name": "prelay",
                        "image": image,
                        "command": [PRELAY_COMMAND],
                        "env": env,
                        "terminationMessagePolicy": "File",
                        "volumeMounts": [
                            { "name": VOLUME_ARTIFACTS, "mountPath": ARTIFACTS_MOUNT_PATH }
                        ],
                        "securityContext": { "privileged": true }
                    }],
                    "volumes": [
                        { "name": VOLUME_ARTIFACTS, "persistentVolumeClaim": { "claimName": pvc } }
                    ],
                }
            }
        }
    })
}

/// Check a pre-lay Job's termination message: exactly `sha256:` and 64
/// lowercase hex digits, the digest the published artifact is verified
/// against.
///
/// # Errors
/// A message saying what was wrong.
pub fn parse_prelay_result(message: &str) -> Result<String, String> {
    let digest = message.trim();
    let hex = digest
        .strip_prefix(DIGEST_PREFIX)
        .ok_or_else(|| format!("{digest:?} is not a {DIGEST_PREFIX} digest"))?;
    if hex.len() != SHA256_HEX_LEN
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(format!(
            "{digest:?} is not {SHA256_HEX_LEN} lowercase hex digits"
        ));
    }
    Ok(digest.to_string())
}

/// Where a pre-lay Job is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrelayOutcome {
    /// Running, or succeeded with its result not read yet.
    Running,
    /// Succeeded; the pre-laid file's digest.
    Done(String),
    /// Failed, or reported something that is not a digest.
    Failed(String),
}

/// Map a pre-lay Job, and its pod's termination message once read, to an
/// outcome.
#[must_use]
pub fn prelay_outcome(job: &Job, termination_message: Option<&str>) -> PrelayOutcome {
    let status = job.status.as_ref();
    if status.and_then(|s| s.succeeded).unwrap_or(0) > 0 {
        return match termination_message.map(parse_prelay_result) {
            Some(Ok(digest)) => PrelayOutcome::Done(digest),
            Some(Err(why)) => PrelayOutcome::Failed(format!("pre-lay Job reported {why}")),
            None => PrelayOutcome::Running,
        };
    }
    if status.and_then(|s| s.failed).unwrap_or(0) > 0 {
        return PrelayOutcome::Failed(
            "pre-lay Job failed; its pod's log and the console it kept say why".to_string(),
        );
    }
    PrelayOutcome::Running
}

/// Fold a pre-lay outcome into a `Ready` build's status. Only `Done` leaves
/// it `Ready`, pointing at the pre-laid file with the builder's digest.
pub fn apply_prelay(status: &mut BuildArtifactStatus, outcome: &PrelayOutcome, disk_size_gib: u32) {
    match outcome {
        PrelayOutcome::Running => {
            status.phase = BuildArtifactPhase::Building;
            status.reason = Some(REASON_PRELAYING.to_string());
            status.file = None;
        }
        PrelayOutcome::Done(digest) => {
            status.phase = BuildArtifactPhase::Ready;
            status.file = Some(prelaid_file_name(&status.os_artifact_ref));
            status.checksum = Some(digest.clone());
            status.prelaid_disk_size_gi_b = Some(disk_size_gib);
        }
        PrelayOutcome::Failed(why) => {
            status.phase = BuildArtifactPhase::Failed;
            status.reason = Some(REASON_PRELAY_FAILED.to_string());
            status.message = Some(why.clone());
            status.file = None;
        }
    }
}

/// Fold a refusal into a build's status: `Failed`, with its reason.
pub fn apply_refusal(status: &mut BuildArtifactStatus, refusal: PrelayRefusal) {
    status.phase = BuildArtifactPhase::Failed;
    status.reason = Some(refusal.reason().to_string());
    status.message = Some(refusal.message().to_string());
    status.file = None;
}

#[cfg(test)]
#[path = "prelay_tests.rs"]
mod prelay_tests;
