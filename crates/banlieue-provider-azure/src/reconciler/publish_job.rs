// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The image publish Job's manifest, name, state and output (ADR-0162
//! Decision 1).
//!
//! A `VMImage` is published into a Compute Gallery by a Job in the
//! imagebuild namespace (ADR-0016), never by the runtime provider: the Job
//! runs `banlieue provider azure image-publish` as its own ServiceAccount
//! (`banlieue-azure-publisher`), whose projected token federates to the
//! publisher identity the `Provider` declares (ADR-0159 Decision 3). The
//! runtime provider can read a published version; only the Job can create
//! or delete one. The same command in `delete` mode removes a version banlieue
//! published, for a superseded digest or a deleted `VMImage` (ADR-0162
//! Decision 8).
//!
//! Everything here is pure: the reconciler applies the manifest and feeds
//! back the Job and its pod's termination message.

use std::collections::BTreeMap;

use banlieue_api::banlieue::{
    AzureConnection, AzureImageGallery, AzurePublisherIdentity, BuildArtifactStatus, Provider,
    VMImage,
};
use banlieue_provider_sdk::k8s_name::collision_safe_name;
use banlieue_provider_sdk::status::condition_status;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Toleration;
use kube::{Resource, ResourceExt};
use serde_json::{Value, json};

/// Label every publish and delete Job carries naming its `VMImage`, so the
/// Job watch maps a Job's change back to the image (cluster-scoped, so the
/// name alone is the reference).
pub const LABEL_VMIMAGE: &str = "banlieue.io/vmimage";
/// `app.kubernetes.io/component` of the Job's pod; the NetworkPolicy and the
/// provider's Job watch select on it.
pub const COMPONENT: &str = "azure-publish";
/// Label key of the component.
pub const LABEL_COMPONENT: &str = "app.kubernetes.io/component";
/// Label key naming the application.
const LABEL_NAME: &str = "app.kubernetes.io/name";
/// Value of [`LABEL_NAME`].
const APP_NAME: &str = "banlieue";
/// Label naming the Job's mode, `publish` or `delete`.
pub const LABEL_MODE: &str = "banlieue.io/publish-mode";
/// The label selector the provider's Job watch uses.
#[must_use]
pub fn job_watch_selector() -> String {
    format!("{LABEL_COMPONENT}={COMPONENT}")
}

/// The ServiceAccount the Job runs as, in the imagebuild namespace. Its
/// subject, `system:serviceaccount:<namespace>:banlieue-azure-publisher`, is
/// what the publisher identity's federated credential trusts.
pub const DEFAULT_PUBLISHER_SERVICE_ACCOUNT: &str = "banlieue-azure-publisher";

/// Where the artifacts PVC is mounted, read-only.
pub const ARTIFACTS_MOUNT_PATH: &str = "/artifacts";
/// Directory of the projected ServiceAccount token.
pub const TOKEN_MOUNT_PATH: &str = "/var/run/secrets/azure/tokens";
/// The projected token's file name, and its volume's name.
pub const TOKEN_FILE: &str = "azure-identity-token";
/// The audience Entra ID requires on a federated token (ADR-0159).
pub const TOKEN_AUDIENCE: &str = "api://AzureADTokenExchange";
/// Lifetime of the projected token; the kubelet refreshes it, and the Job
/// reads the file again on every exchange.
pub const TOKEN_EXPIRATION_SECS: i64 = 3600;
/// Scratch space, so the root filesystem can be read-only.
const TMP_MOUNT_PATH: &str = "/tmp";

/// Hard stop for one Job. A version create took 7 to 22 minutes in the
/// roadmap 23 spike (ADR-0162 finding 4) and the upload is bound by the
/// uplink, so this is generous; a Job that hits it fails, and the next one
/// adopts whatever is still in flight (ADR-0162 Decision 3). Four hours.
pub const ACTIVE_DEADLINE_SECS: i64 = 14_400;
/// Retries inside one Job. The provider's own retry
/// ([`PUBLISH_RETRY_AFTER_SECS`]) covers the rest.
pub const BACKOFF_LIMIT: i32 = 1;
/// How long a finished Job is kept: long enough for its termination message
/// and logs to be read. One day.
pub const TTL_SECONDS_AFTER_FINISHED: i64 = 86_400;
/// How long a failed Job is left in place before it is deleted and
/// recreated: long enough to read its logs and to bound the cost of a
/// failure that keeps recurring (mirrors the vSphere import, ADR-0020).
pub const PUBLISH_RETRY_AFTER_SECS: i64 = 600;
/// The distroless image's non-root user.
const NONROOT_UID: i64 = 65532;
/// Kubernetes `Job` condition type of a Job that has given up.
const JOB_CONDITION_FAILED: &str = "Failed";

/// What the Job does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Upload the build and register it as a gallery image version.
    Publish,
    /// Delete a gallery image version banlieue published.
    Delete,
}

impl Mode {
    /// The `--mode` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::Delete => "delete",
        }
    }
}

/// Deterministic Job name for one `(mode, Provider, image definition,
/// version)`. Deterministic so a later reconcile adopts the Job instead of
/// starting a second upload; collision-safe when truncated, since a
/// definition name is long and two Providers may share a name in different
/// namespaces.
#[must_use]
pub fn job_name(mode: Mode, provider: &Provider, definition: &str, version: &str) -> String {
    let namespace = provider.namespace().unwrap_or_default();
    let name = provider.name_any();
    collision_safe_name(&[
        "azure",
        mode.as_str(),
        &namespace,
        &name,
        definition,
        version,
    ])
}

/// `<namespace>.<name>` of a `Provider`, the value of its ownership tag
/// (ADR-0158 Decision 4).
#[must_use]
pub fn provider_tag(provider: &Provider) -> String {
    format!(
        "{}.{}",
        provider.namespace().unwrap_or_default(),
        provider.name_any()
    )
}

/// Everything one publish or delete Job needs.
#[derive(Debug)]
pub struct JobInputs<'a> {
    /// From [`job_name`].
    pub job_name: &'a str,
    /// The imagebuild namespace.
    pub namespace: &'a str,
    /// The container image: banlieue's own.
    pub image: &'a str,
    /// The publisher ServiceAccount.
    pub service_account: &'a str,
    /// Publish or delete.
    pub mode: Mode,
    /// The `VMImage` the version belongs to.
    pub vmimage: &'a VMImage,
    /// The `Provider` it is published into.
    pub provider: &'a Provider,
    /// The `Provider`'s `spec.connection.azure`.
    pub azure: &'a AzureConnection,
    /// Where the version lives. For a delete, read from the version's ID.
    pub gallery: &'a AzureImageGallery,
    /// The identity the Job federates to.
    pub publisher: &'a AzurePublisherIdentity,
    /// The image definition.
    pub definition: &'a str,
    /// The version name.
    pub version: &'a str,
    /// Publish only: the build to upload.
    pub artifact: Option<&'a BuildArtifactStatus>,
    /// Publish only: `sha256:<hex>` of the build.
    pub checksum: Option<&'a str>,
    /// Publish only: a Trusted Launch image definition (ADR-0162 Decision 3).
    pub trusted_launch: bool,
    /// Taints the Job may tolerate (placement follows the artifacts PVC).
    pub tolerations: &'a [Toleration],
}

/// The Job's command line: `banlieue provider azure image-publish ...`.
#[must_use]
pub fn job_args(inputs: &JobInputs<'_>) -> Vec<String> {
    let mut args: Vec<String> = ["provider", "azure", "image-publish", "--mode"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    args.push(inputs.mode.as_str().to_string());
    let mut flag = |name: &str, value: &str| {
        args.push(format!("--{name}"));
        args.push(value.to_string());
    };
    flag("endpoint", &inputs.provider.spec.connection.endpoint);
    if let Some(authority) = inputs.azure.authority_host.as_deref() {
        flag("authority-host", authority);
    }
    flag("tenant-id", &inputs.azure.tenant_id);
    flag("client-id", &inputs.publisher.client_id);
    flag("subscription-id", &inputs.azure.subscription_id);
    flag("location", &inputs.azure.location);
    flag("gallery-resource-group", &inputs.gallery.resource_group);
    flag("gallery-name", &inputs.gallery.name);
    flag("image-definition", inputs.definition);
    flag("version", inputs.version);
    if inputs.mode == Mode::Publish {
        let file = inputs
            .artifact
            .and_then(|a| a.file.as_deref())
            .unwrap_or_default();
        flag("source", &format!("{ARTIFACTS_MOUNT_PATH}/{file}"));
        if let Some(checksum) = inputs.checksum {
            flag("checksum", checksum);
        }
        // ADR-0172: the Provider's extra tags and disk encryption set go on
        // everything the publish creates. A delete creates nothing.
        if !inputs.azure.tags.is_empty() {
            let tags = serde_json::to_string(&inputs.azure.tags).unwrap_or_default();
            flag("tags", &tags);
        }
        if let Some(des) = inputs.azure.disk_encryption_set_id.as_deref() {
            flag("disk-encryption-set-id", des);
        }
    }
    flag("owner-uid", &inputs.vmimage.uid().unwrap_or_default());
    flag("provider", &provider_tag(inputs.provider));
    flag(
        "federated-token-file",
        &format!("{TOKEN_MOUNT_PATH}/{TOKEN_FILE}"),
    );
    if inputs.mode == Mode::Publish && inputs.trusted_launch {
        args.push("--trusted-launch".to_string());
    }
    args
}

/// Labels on the Job and its pod.
fn labels(inputs: &JobInputs<'_>) -> BTreeMap<String, String> {
    BTreeMap::from([
        (LABEL_NAME.to_string(), APP_NAME.to_string()),
        (LABEL_COMPONENT.to_string(), COMPONENT.to_string()),
        (LABEL_VMIMAGE.to_string(), inputs.vmimage.name_any()),
        (LABEL_MODE.to_string(), inputs.mode.as_str().to_string()),
    ])
}

/// The Job's owner. A publish Job is owned by the `OSArtifact` whose PVC it
/// mounts, so a rebuild garbage-collects it (ADR-0027); a delete Job mounts
/// nothing and is owned by its `VMImage`, which the finalizer keeps until the
/// Job has finished.
fn owner_references(inputs: &JobInputs<'_>) -> Option<Value> {
    if inputs.mode == Mode::Publish {
        let artifact = inputs.artifact?;
        return banlieue_provider_sdk::osartifact::owner_references(
            &artifact.os_artifact_ref,
            artifact.os_artifact_uid.as_deref(),
        );
    }
    let uid = inputs.vmimage.uid()?;
    Some(json!([{
        "apiVersion": VMImage::api_version(&()),
        "kind": VMImage::kind(&()),
        "name": inputs.vmimage.name_any(),
        "uid": uid,
    }]))
}

/// The projected ServiceAccount token, audience [`TOKEN_AUDIENCE`]: the
/// Job's only credential. No Secret is mounted.
fn token_volume() -> Value {
    json!({
        "name": TOKEN_FILE,
        "projected": { "sources": [{ "serviceAccountToken": {
            "audience": TOKEN_AUDIENCE,
            "expirationSeconds": TOKEN_EXPIRATION_SECS,
            "path": TOKEN_FILE,
        }}]},
    })
}

/// Build the Job manifest. The pod runs non-root with no privilege, a
/// read-only root filesystem, no API server token (it never calls the API
/// server) and no Secret; a publish Job mounts the artifacts PVC read-only.
#[must_use]
pub fn build_job(inputs: &JobInputs<'_>) -> Value {
    let labels = labels(inputs);
    let mut mounts = vec![
        json!({ "name": TOKEN_FILE, "mountPath": TOKEN_MOUNT_PATH, "readOnly": true }),
        json!({ "name": "tmp", "mountPath": TMP_MOUNT_PATH }),
    ];
    let mut volumes = vec![token_volume(), json!({ "name": "tmp", "emptyDir": {} })];
    if inputs.mode == Mode::Publish {
        let pvc = inputs
            .artifact
            .and_then(|a| a.pvc_ref.as_ref())
            .map(|r| r.name.clone())
            .unwrap_or_default();
        mounts.push(
            json!({ "name": "artifacts", "mountPath": ARTIFACTS_MOUNT_PATH, "readOnly": true }),
        );
        volumes.push(json!({
            "name": "artifacts",
            "persistentVolumeClaim": { "claimName": pvc, "readOnly": true },
        }));
    }
    let mut pod_spec = json!({
        "restartPolicy": "Never",
        "serviceAccountName": inputs.service_account,
        "automountServiceAccountToken": false,
        "securityContext": {
            "runAsNonRoot": true,
            "runAsUser": NONROOT_UID,
            "seccompProfile": { "type": "RuntimeDefault" },
        },
        "containers": [{
            "name": "publish",
            "image": inputs.image,
            "args": job_args(inputs),
            "volumeMounts": mounts,
            "terminationMessagePolicy": "FallbackToLogsOnError",
            "securityContext": {
                "allowPrivilegeEscalation": false,
                "readOnlyRootFilesystem": true,
                "capabilities": { "drop": ["ALL"] },
            },
        }],
        "volumes": volumes,
    });
    if !inputs.tolerations.is_empty() {
        pod_spec["tolerations"] =
            serde_json::to_value(inputs.tolerations).unwrap_or_else(|_| json!([]));
    }
    json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": inputs.job_name,
            "namespace": inputs.namespace,
            "labels": labels,
            "ownerReferences": owner_references(inputs),
        },
        "spec": {
            "backoffLimit": BACKOFF_LIMIT,
            "activeDeadlineSeconds": ACTIVE_DEADLINE_SECS,
            "ttlSecondsAfterFinished": TTL_SECONDS_AFTER_FINISHED,
            "template": {
                "metadata": { "labels": labels },
                "spec": pod_spec,
            },
        },
    })
}

/// Where a Job stands, as the decisions need it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    /// No Job by that name.
    Absent,
    /// Created and not finished.
    Running,
    /// Finished successfully.
    Succeeded,
    /// Gave up. `retry_due` once its `Failed` condition is at least
    /// [`PUBLISH_RETRY_AFTER_SECS`] old.
    Failed {
        /// Whether to delete and recreate it now.
        retry_due: bool,
    },
}

/// Classify `job` at `now`. Pure.
#[must_use]
pub fn job_state(job: Option<&Job>, now: k8s_openapi::jiff::Timestamp) -> JobState {
    let Some(job) = job else {
        return JobState::Absent;
    };
    let Some(status) = job.status.as_ref() else {
        return JobState::Running;
    };
    if status.succeeded.unwrap_or(0) > 0 {
        return JobState::Succeeded;
    }
    let failed_at = status
        .conditions
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|c| c.type_ == JOB_CONDITION_FAILED && c.status == condition_status::TRUE)
        .find_map(|c| c.last_transition_time.as_ref());
    match failed_at {
        Some(at) => JobState::Failed {
            retry_due: now.as_second() - at.0.as_second() >= PUBLISH_RETRY_AFTER_SECS,
        },
        None => JobState::Running,
    }
}

/// What the Job reported on its last line (its pod's termination message).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobOutput {
    /// A version was published.
    Published {
        /// The version's resource ID.
        version_id: String,
        /// `sha256:<hex>` of what was uploaded.
        source_digest: String,
        /// The smallest OS disk a VM booting it may have, when reported.
        min_os_disk_size_gib: Option<u32>,
    },
    /// The version is gone.
    Deleted,
    /// The Job failed, with Azure's error code when it had one.
    Error {
        /// The error code.
        code: String,
        /// The message.
        message: String,
    },
}

/// Parse a termination message. `None` for anything that is not one of the
/// three JSON shapes (a log tail, from `FallbackToLogsOnError`). Pure.
#[must_use]
pub fn parse_output(message: &str) -> Option<JobOutput> {
    let value: Value = serde_json::from_str(message.trim()).ok()?;
    if let Some(code) = value.get("error") {
        return Some(JobOutput::Error {
            code: code.as_str().unwrap_or_default().to_string(),
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }
    if value.get("deleted").and_then(Value::as_bool) == Some(true) {
        return Some(JobOutput::Deleted);
    }
    let version_id = value.get("versionId")?.as_str()?.to_string();
    let source_digest = value.get("sourceDigest")?.as_str()?.to_string();
    let min_os_disk_size_gib = value
        .get("minOsDiskSizeGiB")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok());
    Some(JobOutput::Published {
        version_id,
        source_digest,
        min_os_disk_size_gib,
    })
}

#[cfg(test)]
#[path = "publish_job_tests.rs"]
mod publish_job_tests;
