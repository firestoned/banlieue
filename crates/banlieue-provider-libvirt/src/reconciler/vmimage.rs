// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VMImage` reconciler — the libvirt half of ADR-0010's pipeline.
//!
//! For sources where `providerClass == "libvirt"`:
//!
//! - **`BackingFile`** — a volume expected to already exist on the host. Ready
//!   when it is actually present in the target pool.
//! - **`Url`** — gated on `VMImage.status.buildArtifact.phase == Ready`,
//!   which only `banlieue-imagebuilder` ever writes (ADR-0010). Once the raw
//!   disk exists, one import Job per target pool streams it in.
//!
//! **Why a Job and not this process.** Importing a guest image moves
//! gigabytes. A reconcile loop blocked for minutes on I/O stops reconciling
//! everything else, holds memory proportional to the image, and leaves a
//! half-written volume behind if the pod restarts. The Job runs the `banlieue`
//! binary itself, so the data path stays inside banlieue's own supply chain —
//! no third-party tools image to pin or patch (ADR-0011).
//!
//! This reconciler writes only `status.perProvider[]`. `status.buildArtifact`
//! belongs to `banlieue-imagebuilder`'s field manager and is never touched
//! here; the SSA split from ADR-0010 is what keeps the two controllers from
//! contending.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use banlieue_api::banlieue::{
    BuildArtifactPhase, BuildArtifactStatus, ImagePerProviderStatus, ImageSource, ImageSourceKind,
    Provider, VMImage, VMImageStatus, ZoneImageStatus,
};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_PROVIDER_LIBVIRT;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Toleration;
use kube::{
    Resource, ResourceExt,
    api::{Api, ListParams, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{info, warn};

use super::provider::PROVIDER_CLASS_NAME;
use crate::context::Context;
use crate::error::{Error, Result};

/// Stable `reason` strings for `ImagePerProviderStatus` / `ZoneImageStatus`.
pub mod reasons {
    /// Everything this provider is responsible for is present.
    pub const RECONCILED: &str = "Reconciled";
    /// No libvirt source on this VMImage — nothing for us to do.
    pub const NO_LIBVIRT_SOURCE: &str = "NoLibvirtSource";
    /// `status.buildArtifact` is absent, Pending, or Building.
    pub const BUILD_PENDING: &str = "BuildPending";
    /// `status.buildArtifact.phase == Failed`.
    pub const BUILD_FAILED: &str = "BuildFailed";
    /// The Provider has published no failure domains, so there is nowhere to
    /// import into.
    pub const NO_FAILURE_DOMAINS: &str = "NoFailureDomains";
    /// An import Job is running for this pool.
    pub const IMPORTING: &str = "Importing";
    /// The import Job for this pool failed.
    pub const IMPORT_FAILED: &str = "ImportFailed";
    /// A `BackingFile` source names a volume the pool does not contain.
    pub const VOLUME_NOT_FOUND: &str = "VolumeNotFound";
    /// This source kind is not supported by the libvirt provider.
    pub const UNSUPPORTED_SOURCE_KIND: &str = "UnsupportedSourceKind";
}

/// Top-level reconcile entrypoint.
pub async fn reconcile(image: Arc<VMImage>, ctx: Arc<Context>) -> Result<Action> {
    let name = image.name_any();
    let generation = image.metadata.generation.unwrap_or(0);

    let span = tracing::info_span!("reconcile", kind = "VMImage", name = %name, generation);
    let _enter = span.enter();

    let Some(source) = find_libvirt_source(&image.spec.sources) else {
        // Every other provider handles its own classes.
        return Ok(requeue_long());
    };

    let providers = list_libvirt_providers(&ctx).await?;
    if providers.is_empty() {
        info!("no libvirt Providers in scope — leaving status untouched");
        return Ok(requeue_long());
    }

    let raw_disk = image
        .status
        .as_ref()
        .and_then(|s| s.build_artifact.as_ref());

    let mut rows = Vec::with_capacity(providers.len());
    let mut any_pending = false;
    for provider in &providers {
        let row = reconcile_for_provider(&ctx, provider, source, raw_disk, &name).await;
        any_pending |= !row.ready;
        rows.push(row);
    }

    patch_status(&ctx, &name, generation, rows).await?;

    // Poll while work is outstanding; back off once everything is settled.
    Ok(if any_pending {
        requeue_default()
    } else {
        requeue_long()
    })
}

/// `error_policy` invoked on `reconcile` failure.
pub fn error_policy(_image: Arc<VMImage>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "libvirt vmimage reconcile error policy fired");
    requeue_on_error()
}

/// Pick the first `libvirt` source of a kind this provider supports.
///
/// `Template` is a vSphere concept and is never matched here.
pub fn find_libvirt_source(sources: &[ImageSource]) -> Option<&ImageSource> {
    sources.iter().find(|s| {
        s.provider_class == PROVIDER_CLASS_NAME
            && matches!(s.kind, ImageSourceKind::BackingFile | ImageSourceKind::Url)
    })
}

/// Resolve the per-provider row, creating or observing import Jobs as needed.
async fn reconcile_for_provider(
    ctx: &Context,
    provider: &Provider,
    source: &ImageSource,
    raw_disk: Option<&BuildArtifactStatus>,
    image_name: &str,
) -> ImagePerProviderStatus {
    match source.kind {
        ImageSourceKind::BackingFile => {
            // Declared to exist already; nothing to import. Treated as ready
            // without contacting the host: verifying it would need a
            // connection per reconcile for a source the admin asserted is
            // static. The provider's own probe already proves reachability.
            row(provider, true, reasons::RECONCILED, None, Vec::new())
        }
        ImageSourceKind::Url => match gate_on_raw_disk(raw_disk) {
            Err((reason, message)) => row(provider, false, reason, Some(message), Vec::new()),
            Ok(artifact) => {
                let pools = target_pools(provider);
                if pools.is_empty() {
                    return row(
                        provider,
                        false,
                        reasons::NO_FAILURE_DOMAINS,
                        Some("Provider has published no failure domains yet".to_string()),
                        Vec::new(),
                    );
                }
                let zones = ensure_import_jobs(ctx, provider, image_name, artifact, &pools).await;
                let ready = zones.iter().all(|z| z.ready);
                let reason = if ready {
                    reasons::RECONCILED
                } else if zones
                    .iter()
                    .any(|z| z.reason.as_deref() == Some(reasons::IMPORT_FAILED))
                {
                    reasons::IMPORT_FAILED
                } else {
                    reasons::IMPORTING
                };
                row(provider, ready, reason, None, zones)
            }
        },
        ImageSourceKind::Template => row(
            provider,
            false,
            reasons::UNSUPPORTED_SOURCE_KIND,
            Some("Template sources are a vSphere concept".to_string()),
            Vec::new(),
        ),
    }
}

/// Decide whether the shared raw disk is usable yet.
///
/// Pure, so the gating rules are unit-testable without kube.
pub fn gate_on_raw_disk(
    raw_disk: Option<&BuildArtifactStatus>,
) -> std::result::Result<&BuildArtifactStatus, (&'static str, String)> {
    let Some(a) = raw_disk else {
        return Err((
            reasons::BUILD_PENDING,
            "waiting for banlieue-imagebuilder to build the raw disk".to_string(),
        ));
    };
    match a.phase {
        BuildArtifactPhase::Ready => Ok(a),
        BuildArtifactPhase::Failed => Err((
            reasons::BUILD_FAILED,
            a.message
                .clone()
                .unwrap_or_else(|| "raw disk build failed".to_string()),
        )),
        _ => Err((
            reasons::BUILD_PENDING,
            format!("raw disk build in progress ({:?})", a.phase),
        )),
    }
}

/// Storage pool names this Provider should import into.
///
/// The **declared** storage classes, narrowed to those the Provider reconciler
/// actually verified on the host — never the raw discovered pool list.
///
/// `attributes.raw["pools"]` is discovery output: every pool libvirtd reports,
/// including ones the admin never asked banlieue to use. Importing into those
/// writes gigabytes into storage nobody declared, and contradicts the rule that
/// capabilities are declared while discovery is a status-time concern.
/// `availableStorageClasses` is the intersection that survived probing, so it
/// is the honest answer to "where may this image go".
///
/// Deduplicated: two classes may legitimately map to one pool, and importing
/// twice would run the same multi-gigabyte transfer into the same place —
/// with the second Job racing the first.
pub fn target_pools(provider: &Provider) -> Vec<String> {
    let verified: BTreeSet<&str> = provider
        .status
        .as_ref()
        .map(|s| {
            s.failure_domains
                .iter()
                .flat_map(|fd| {
                    fd.attributes
                        .available_storage_classes
                        .iter()
                        .map(String::as_str)
                })
                .collect()
        })
        .unwrap_or_default();

    provider
        .spec
        .capabilities
        .storage_classes
        .iter()
        .filter(|class| verified.contains(class.name.as_str()))
        .filter_map(|class| class.target.as_ref()?.get("pool").cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Deterministic Job name for one (image, provider, pool) import.
///
/// Deterministic so a re-reconcile adopts the existing Job instead of starting
/// a second copy of a multi-gigabyte transfer.
pub fn import_job_name(image: &str, provider: &str, pool: &str) -> String {
    // Kubernetes names cap at 63 characters; keep room for the suffix.
    let raw = format!("import-{image}-{provider}-{pool}");
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .to_lowercase();
    cleaned.chars().take(63).collect::<String>()
}

/// Create the import Job for each pool that lacks one, and translate existing
/// Job state into [`ZoneImageStatus`].
async fn ensure_import_jobs(
    ctx: &Context,
    provider: &Provider,
    image_name: &str,
    artifact: &BuildArtifactStatus,
    pools: &[String],
) -> Vec<ZoneImageStatus> {
    let api: Api<Job> = Api::namespaced(ctx.client.clone(), &ctx.build_namespace);
    let mut zones = Vec::with_capacity(pools.len());

    for pool in pools {
        let job_name = import_job_name(image_name, &provider.name_any(), pool);
        let observed = api.get(&job_name).await;

        let zone = match observed {
            Ok(job) => zone_from_job(pool, &job_name, &job),
            Err(kube::Error::Api(e)) if e.code == 404 => {
                // No Job yet — create one.
                let spec = build_import_job(&ImportJobInputs {
                    job_name: &job_name,
                    namespace: &ctx.build_namespace,
                    image: &ctx.import_image,
                    service_account: Some(ctx.import_service_account.as_str()),
                    vmimage: image_name,
                    provider,
                    pool,
                    artifact,
                    tolerations: &ctx.import_tolerations,
                });
                match api
                    .patch(
                        &job_name,
                        &PatchParams::apply(FIELD_MANAGER_PROVIDER_LIBVIRT).force(),
                        &Patch::Apply(&spec),
                    )
                    .await
                {
                    Ok(_) => zone(pool, false, reasons::IMPORTING, "import Job created"),
                    Err(e) => zone(pool, false, reasons::IMPORT_FAILED, &e.to_string()),
                }
            }
            Err(e) => zone(pool, false, reasons::IMPORT_FAILED, &e.to_string()),
        };
        zones.push(zone);
    }
    zones
}

/// Map a Job's status onto a zone row.
pub fn zone_from_job(pool: &str, job_name: &str, job: &Job) -> ZoneImageStatus {
    let status = job.status.as_ref();
    let succeeded = status.and_then(|s| s.succeeded).unwrap_or(0);
    let failed = status.and_then(|s| s.failed).unwrap_or(0);

    if succeeded > 0 {
        return ZoneImageStatus {
            name: pool.to_string(),
            ready: true,
            resolved_ref: Some(format!("{pool}/{job_name}")),
            template_folder: None,
            reason: Some(reasons::RECONCILED.to_string()),
            message: None,
        };
    }
    if failed > 0 {
        return zone(
            pool,
            false,
            reasons::IMPORT_FAILED,
            &format!("import Job {job_name} failed"),
        );
    }
    zone(pool, false, reasons::IMPORTING, "import Job running")
}

fn zone(pool: &str, ready: bool, reason: &str, message: &str) -> ZoneImageStatus {
    ZoneImageStatus {
        name: pool.to_string(),
        ready,
        resolved_ref: None,
        template_folder: None,
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
    }
}

fn row(
    provider: &Provider,
    ready: bool,
    reason: &str,
    message: Option<String>,
    zones: Vec<ZoneImageStatus>,
) -> ImagePerProviderStatus {
    ImagePerProviderStatus {
        provider_name: provider.name_any(),
        provider_namespace: provider.namespace().unwrap_or_default(),
        ready,
        resolved_ref: None,
        reason: Some(reason.to_string()),
        message,
        zones,
    }
}

/// Everything one import Job needs to know.
///
/// A struct rather than eight positional parameters: the three `&str` fields
/// in a row are trivially swappable at a call site, and a Job that mounts the
/// wrong Secret fails minutes later inside a container.
#[derive(Debug)]
pub struct ImportJobInputs<'a> {
    /// Deterministic Job name from [`import_job_name`].
    pub job_name: &'a str,
    /// Namespace the Job and the artifacts PVC live in.
    pub namespace: &'a str,
    /// Container image to run — the banlieue image itself.
    pub image: &'a str,
    /// ServiceAccount to run as. `None` falls back to the namespace default.
    pub service_account: Option<&'a str>,
    /// `VMImage` being imported.
    pub vmimage: &'a str,
    /// `Provider` whose host receives the image.
    pub provider: &'a Provider,
    /// Target storage pool.
    pub pool: &'a str,
    /// The raw disk to upload, as published by banlieue-imagebuilder.
    pub artifact: &'a BuildArtifactStatus,
    /// Taints the Job may tolerate. **Not** a placement decision — placement
    /// follows the artifacts PVC, which the scheduler resolves on its own.
    /// These only grant permission to land on a node that happens to be
    /// tainted, which is the case when the volume lives on a dedicated build
    /// node.
    pub tolerations: &'a [Toleration],
}

/// Build the import Job manifest.
///
/// Pure so the manifest is unit-testable without a cluster. The Job runs the
/// `banlieue` binary's own import subcommand with the artifacts PVC mounted
/// read-only — the image never travels through the controller.
pub fn build_import_job(inputs: &ImportJobInputs<'_>) -> serde_json::Value {
    let ImportJobInputs {
        job_name,
        namespace,
        image,
        service_account,
        vmimage,
        provider,
        pool,
        artifact,
        tolerations,
    } = *inputs;

    let pvc = artifact
        .pvc_ref
        .as_ref()
        .map(|r| r.name.clone())
        .unwrap_or_default();
    let disk_file = artifact.file.clone().unwrap_or_default();

    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/name".to_string(), "banlieue".to_string());
    labels.insert(
        "app.kubernetes.io/component".to_string(),
        "libvirt-import".to_string(),
    );
    labels.insert("banlieue.io/vmimage".to_string(), vmimage.to_string());
    labels.insert("banlieue.io/pool".to_string(), pool.to_string());

    // SEC-004: when the build published a checksum, the Job verifies the
    // artifact against it before any byte reaches the host.
    let mut args = vec![
        "provider".to_string(),
        "libvirt".to_string(),
        "import".to_string(),
        "--vmimage".to_string(),
        vmimage.to_string(),
        "--provider".to_string(),
        provider.name_any(),
        // The Job runs in the build namespace; the
        // Provider generally does not live there.
        "--provider-namespace".to_string(),
        provider.namespace().unwrap_or_default(),
        "--pool".to_string(),
        pool.to_string(),
        "--source".to_string(),
        format!("/artifacts/{disk_file}"),
    ];
    if let Some(checksum) = artifact.checksum.as_deref() {
        args.push("--checksum".to_string());
        args.push(checksum.to_string());
    }

    // ADR-0027: own this Job by the OSArtifact whose PVC it mounts, so a
    // rebuild's OSArtifact deletion garbage-collects the Job immediately
    // instead of it outliving the artifact for up to its own
    // ttlSecondsAfterFinished below.
    let owner_references = banlieue_provider_sdk::osartifact::owner_references(
        &artifact.os_artifact_ref,
        artifact.os_artifact_uid.as_deref(),
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
            // A half-finished upload is resumable only by starting over, and
            // retrying forever would hammer the host; two attempts then stop.
            "backoffLimit": 1,
            "ttlSecondsAfterFinished": 86400,
            "template": {
                "metadata": { "labels": labels },
                "spec": {
                    "restartPolicy": "Never",
                    // No nodeSelector, deliberately. Where this Job runs is
                    // decided by the PVC it mounts, not by us: the scheduler
                    // already honours the bound PV's own constraints. On
                    // node-local storage the PV carries nodeAffinity and the
                    // scheduler confines the Job accordingly; on
                    // network-attached storage there is nothing to confine and
                    // pinning would only make the Job unschedulable when that
                    // node is full, cordoned, or gone.
                    //
                    // Tolerations are different: they are not a placement
                    // choice but permission to land somewhere the scheduler
                    // has already chosen. If the node holding the volume is
                    // tainted, the Job needs them to run there at all.
                    "tolerations": (!tolerations.is_empty())
                        .then(|| serde_json::to_value(tolerations).unwrap_or(serde_json::Value::Null)),
                    // Same identity as the controller: the operator already
                    // scoped it to this Provider and its Secret, so the Job
                    // gains no authority the controller lacked (ADR-0012).
                    "serviceAccountName": service_account,
                    "securityContext": {
                        "runAsNonRoot": true,
                        "runAsUser": 65532,
                        "seccompProfile": { "type": "RuntimeDefault" }
                    },
                    "containers": [{
                        "name": "import",
                        "image": image,
                        "args": args,
                        "volumeMounts": [
                            { "name": "artifacts", "mountPath": "/artifacts", "readOnly": true }
                        ],
                        "securityContext": {
                            "allowPrivilegeEscalation": false,
                            "readOnlyRootFilesystem": true,
                            "capabilities": { "drop": ["ALL"] }
                        }
                    }],
                    "volumes": [
                        // Read-only: the Job consumes the artifact, never edits it.
                        { "name": "artifacts",
                          "persistentVolumeClaim": { "claimName": pvc, "readOnly": true } }
                    ]
                }
            }
        }
    })
}

async fn list_libvirt_providers(ctx: &Context) -> Result<Vec<Provider>> {
    let api: Api<Provider> = match ctx.namespace.as_deref() {
        Some(ns) => Api::namespaced(ctx.client.clone(), ns),
        None => Api::all(ctx.client.clone()),
    };
    Ok(api
        .list(&ListParams::default())
        .await?
        .into_iter()
        .filter(|p| p.spec.provider_class_ref.name == PROVIDER_CLASS_NAME)
        .collect())
}

async fn patch_status(
    ctx: &Context,
    name: &str,
    generation: i64,
    per_provider: Vec<ImagePerProviderStatus>,
) -> Result<()> {
    let status = VMImageStatus {
        per_provider,
        // Written solely by banlieue-imagebuilder (ADR-0010).
        build_artifact: None,
        // Written solely by banlieue-controller (ADR-0015). A provider cannot
        // compute "ready everywhere" from rows it does not own, so it reports
        // its own perProvider entry and says nothing about the aggregate.
        conditions: Vec::new(),
        observed_generation: Some(generation),
    };
    let patch = json!({
        "apiVersion": VMImage::api_version(&()).to_string(),
        "kind": VMImage::kind(&()).to_string(),
        "metadata": { "name": name },
        "status": status,
    });
    let api: Api<VMImage> = Api::all(ctx.client.clone());
    api.patch_status(
        name,
        &PatchParams::apply(FIELD_MANAGER_PROVIDER_LIBVIRT).force(),
        &Patch::Apply(&patch),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "vmimage_tests.rs"]
mod vmimage_tests;
