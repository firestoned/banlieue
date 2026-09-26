// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Pushing a finished build artifact to an OCI registry (ADR-0064 Decision 2).
//!
//! A host-resident provider (Cloud Hypervisor) cannot mount the artifacts
//! PVC, so once the build is `Ready` and a `cloud-hypervisor` `Url` source
//! exists, the reconciler creates a Job beside the PVC that runs
//! `banlieue imagebuilder push`. The Job pushes the file as a single-layer
//! artifact and writes the digest-pinned reference as its termination
//! message; that message is its only output. The reconciler checks it names
//! the configured repository by digest before recording it on
//! `VMImage.status.buildArtifact.ociArtifact`.
//!
//! Everything here is pure; the reconciler does the API calls.

use std::collections::BTreeMap;

use banlieue_api::banlieue::{
    BuildArtifactKind, BuildArtifactStatus, ImageSource, ImageSourceKind, OciArtifactPhase,
    OciArtifactStatus,
};
use banlieue_oci::Reference;
pub use banlieue_oci::manifest::{ARTIFACT_TYPE_ISO, ARTIFACT_TYPE_RAW};
use banlieue_provider_sdk::naming::truncate_with_hash;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Toleration;
use serde_json::{Value, json};

/// Provider class whose `Url` sources are delivered through the registry.
pub const PROVIDER_CLASS_CLOUD_HYPERVISOR: &str = "cloud-hypervisor";

/// Suffix of the push Job's name, after the `OSArtifact`'s.
const PUSH_JOB_SUFFIX: &str = "-push";

/// Manifest annotation naming the `VMImage`, so registry retention can be
/// traced back to the image that pushed it.
pub const ANNOTATION_VMIMAGE: &str = "io.banlieue.vmimage";
/// Manifest annotation carrying the `VMImage`'s generation.
pub const ANNOTATION_GENERATION: &str = "io.banlieue.vmimage.generation";

/// Where the artifacts PVC is mounted in the push Job.
const ARTIFACTS_MOUNT_PATH: &str = "/artifacts";
/// Where the push credentials Secret is mounted.
pub const CREDENTIALS_MOUNT_PATH: &str = "/var/run/banlieue/registry";
/// Writable scratch for the compressed copy; the root filesystem is
/// read-only and so is the PVC.
const SCRATCH_MOUNT_PATH: &str = "/scratch";

const VOLUME_ARTIFACTS: &str = "artifacts";
const VOLUME_CREDENTIALS: &str = "registry-credentials";
const VOLUME_SCRATCH: &str = "scratch";

/// The distroless nonroot uid the banlieue image runs as.
const NONROOT_UID: i64 = 65532;

/// Registry configuration, parsed once at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryConfig {
    /// `registry/repository`, no tag and no digest.
    pub repository: Reference,
    /// `kubernetes.io/basic-auth` Secret in the build namespace, if the
    /// registry needs credentials to push.
    pub credentials_secret: Option<String>,
    /// Speak `http://`: a registry on a private network or a test.
    pub plain_http: bool,
    /// Image the push Job runs: the banlieue image itself.
    pub image: String,
}

impl RegistryConfig {
    /// Validate and build.
    ///
    /// # Errors
    /// A message naming the problem when `repository` is not a
    /// registry-qualified repository, or carries a tag or digest.
    pub fn new(
        repository: &str,
        credentials_secret: Option<String>,
        plain_http: bool,
        image: String,
    ) -> Result<Self, String> {
        let repository = Reference::parse(repository).map_err(|e| e.to_string())?;
        if repository.tag.is_some() || repository.digest.is_some() {
            return Err(format!(
                "{repository}: give the repository only; each build is tagged by its own id"
            ));
        }
        Ok(Self {
            repository,
            credentials_secret,
            plain_http,
            image,
        })
    }
}

/// Whether any source is a `cloud-hypervisor` `Url`: the only consumer that
/// cannot mount the PVC.
#[must_use]
pub fn needs_push(sources: &[ImageSource]) -> bool {
    sources.iter().any(|s| {
        s.kind == ImageSourceKind::Url && s.provider_class == PROVIDER_CLASS_CLOUD_HYPERVISOR
    })
}

/// Deterministic push Job name for an `OSArtifact`.
#[must_use]
pub fn push_job_name(os_artifact_name: &str) -> String {
    truncate_with_hash(&format!("{os_artifact_name}{PUSH_JOB_SUFFIX}"))
}

/// Where to push this build: the repository, tagged by the `OSArtifact`'s
/// uid. One tag per build, so a rebuild never finds the previous build's
/// tag and mistakes it for its own.
#[must_use]
pub fn push_target(registry: &RegistryConfig, os_artifact_uid: &str) -> Reference {
    Reference {
        tag: Some(os_artifact_uid.to_string()),
        digest: None,
        ..registry.repository.clone()
    }
}

/// OCI `artifactType` for a build artifact kind.
#[must_use]
pub fn artifact_type(kind: &BuildArtifactKind) -> &'static str {
    match kind {
        BuildArtifactKind::CloudImage => ARTIFACT_TYPE_RAW,
        BuildArtifactKind::Iso => ARTIFACT_TYPE_ISO,
    }
}

/// Everything one push Job needs.
#[derive(Debug)]
pub struct PushJobInputs<'a> {
    /// From [`push_job_name`].
    pub job_name: &'a str,
    /// The build namespace, where the PVC is.
    pub namespace: &'a str,
    /// `VMImage` name, for the manifest annotation and labels.
    pub vmimage: &'a str,
    /// `VMImage` generation, for the manifest annotation.
    pub generation: i64,
    /// Where to push.
    pub registry: &'a RegistryConfig,
    /// The `Ready` build artifact.
    pub artifact: &'a BuildArtifactStatus,
    /// The live `OSArtifact`'s uid: the tag, and the Job's owner.
    pub os_artifact_uid: &'a str,
    /// Taints the Job may tolerate: the PVC may live on a tainted build
    /// node. Placement itself follows the PVC.
    pub tolerations: &'a [Toleration],
}

/// Build the push Job.
///
/// No ServiceAccount token (the Job never talks to the API server), a
/// read-only PVC, a read-only root filesystem with an `emptyDir` for the
/// compressed copy, and no privilege. Owned by the `OSArtifact` so a
/// rebuild garbage-collects it (ADR-0027). No `ttlSecondsAfterFinished`:
/// the termination message is the Job's only output, and it must outlive
/// the reconcile that reads it.
#[must_use]
pub fn build_push_job(inputs: &PushJobInputs<'_>) -> Value {
    let PushJobInputs {
        job_name,
        namespace,
        vmimage,
        generation,
        registry,
        artifact,
        os_artifact_uid,
        tolerations,
    } = *inputs;

    let pvc = artifact
        .pvc_ref
        .as_ref()
        .map(|r| r.name.clone())
        .unwrap_or_default();
    let file = artifact.file.clone().unwrap_or_default();
    let target = push_target(registry, os_artifact_uid);

    let mut args = vec![
        "imagebuilder".to_string(),
        "push".to_string(),
        "--source".to_string(),
        format!("{ARTIFACTS_MOUNT_PATH}/{file}"),
        "--target".to_string(),
        target.to_string(),
        "--artifact-type".to_string(),
        artifact_type(&artifact.kind).to_string(),
        "--scratch-dir".to_string(),
        SCRATCH_MOUNT_PATH.to_string(),
        "--annotation".to_string(),
        format!("{ANNOTATION_VMIMAGE}={vmimage}"),
        "--annotation".to_string(),
        format!("{ANNOTATION_GENERATION}={generation}"),
    ];
    if registry.credentials_secret.is_some() {
        args.push("--credentials-dir".to_string());
        args.push(CREDENTIALS_MOUNT_PATH.to_string());
    }
    if registry.plain_http {
        args.push("--plain-http".to_string());
    }

    let mut mounts = vec![
        json!({ "name": VOLUME_ARTIFACTS, "mountPath": ARTIFACTS_MOUNT_PATH, "readOnly": true }),
        json!({ "name": VOLUME_SCRATCH, "mountPath": SCRATCH_MOUNT_PATH }),
    ];
    let mut volumes = vec![
        json!({ "name": VOLUME_ARTIFACTS,
                "persistentVolumeClaim": { "claimName": pvc, "readOnly": true } }),
        json!({ "name": VOLUME_SCRATCH, "emptyDir": {} }),
    ];
    if let Some(secret) = &registry.credentials_secret {
        mounts.push(
            json!({ "name": VOLUME_CREDENTIALS, "mountPath": CREDENTIALS_MOUNT_PATH, "readOnly": true }),
        );
        volumes.push(json!({ "name": VOLUME_CREDENTIALS, "secret": { "secretName": secret } }));
    }

    let labels = BTreeMap::from([
        ("app.kubernetes.io/name".to_string(), "banlieue".to_string()),
        (
            "app.kubernetes.io/component".to_string(),
            "registry-push".to_string(),
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
            // A retry re-uploads only what the registry does not have.
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
                        "name": "push",
                        "image": registry.image,
                        "args": args,
                        "volumeMounts": mounts,
                        "securityContext": {
                            "allowPrivilegeEscalation": false,
                            "readOnlyRootFilesystem": true,
                            "capabilities": { "drop": ["ALL"] }
                        }
                    }],
                    "volumes": volumes,
                }
            }
        }
    })
}

/// Check a push Job's termination message: it must be the configured
/// repository, addressed by digest. Anything else is refused, so a
/// compromised or confused Job cannot point hosts at another registry.
///
/// # Errors
/// A message saying what was wrong.
pub fn pushed_reference(message: &str, registry: &RegistryConfig) -> Result<String, String> {
    let r = Reference::parse(message.trim()).map_err(|e| e.to_string())?;
    if r.digest.is_none() || r.tag.is_some() {
        return Err(format!("{r}: not a digest-only reference"));
    }
    if r.registry != registry.repository.registry || r.repository != registry.repository.repository
    {
        return Err(format!(
            "{r}: not in the configured repository {}",
            registry.repository
        ));
    }
    Ok(r.to_string())
}

/// Map a push Job, and its pod's termination message once it has one, to
/// the status row.
#[must_use]
pub fn oci_status(
    registry: &RegistryConfig,
    job: &Job,
    termination_message: Option<&str>,
) -> OciArtifactStatus {
    let status = job.status.as_ref();
    let succeeded = status.and_then(|s| s.succeeded).unwrap_or(0);
    let failed = status.and_then(|s| s.failed).unwrap_or(0);

    if succeeded > 0 {
        return match termination_message.map(|m| pushed_reference(m, registry)) {
            Some(Ok(reference)) => OciArtifactStatus {
                phase: OciArtifactPhase::Ready,
                reference: Some(reference),
                message: None,
            },
            Some(Err(why)) => failed_status(&format!("push Job reported {why}")),
            None => OciArtifactStatus {
                phase: OciArtifactPhase::Pushing,
                reference: None,
                message: Some("push Job succeeded; reading its result".to_string()),
            },
        };
    }
    if failed > 0 {
        return failed_status("push Job failed; see its pod's log");
    }
    OciArtifactStatus {
        phase: OciArtifactPhase::Pushing,
        reference: None,
        message: None,
    }
}

/// A `Failed` row with `message`.
#[must_use]
pub fn failed_status(message: &str) -> OciArtifactStatus {
    OciArtifactStatus {
        phase: OciArtifactPhase::Failed,
        reference: None,
        message: Some(message.to_string()),
    }
}

#[cfg(test)]
#[path = "push_tests.rs"]
mod push_tests;
