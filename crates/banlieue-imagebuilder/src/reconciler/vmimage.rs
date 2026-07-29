// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VMImage` reconciler — drives the shared, provider-agnostic raw-disk
//! build for `Url`-kind sources via kairos-operator's `OSArtifact` CRD.
//!
//! For every `VMImage` with at least one `spec.sources[]` entry where
//! `kind == Url`, server-side-apply an `OSArtifact` (`build.kairos.io/v1alpha2`
//! — a CRD banlieue does not own, so it is modeled as a [`DynamicObject`]
//! rather than a typed `#[derive(CustomResource)]`; banlieue must never
//! generate or install kairos-operator's own CRD), watch its
//! `status.phase`, and mirror progress into `VMImage.status.rawDiskArtifact`.
//! `VMImage.status.perProvider[]` is never touched here — that stays each
//! provider's own field, written by its own field manager. See ADR-0010.
//!
//! The pure helpers ([`find_url_source`], [`desired_os_artifact`],
//! [`map_kairos_phase`], [`compute_raw_disk_artifact_status`]) take plain
//! values so they're testable without a live `kube::Api`.

use std::sync::Arc;

use banlieue_api::banlieue::{
    Architecture, ImageSource, ImageSourceKind, RawDiskArtifactPhase, RawDiskArtifactStatus,
    VMImage, VMImageStatus,
};
use banlieue_api::common::LocalObjectReference;
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_IMAGEBUILDER;
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::{Resource, ResourceExt};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::{Error, Result};

/// `OSArtifact`'s API group (kairos-operator, not banlieue's own).
pub const OSARTIFACT_GROUP: &str = "build.kairos.io";
/// `OSArtifact`'s API version.
pub const OSARTIFACT_VERSION: &str = "v1alpha2";
/// `OSArtifact`'s kind.
pub const OSARTIFACT_KIND: &str = "OSArtifact";
/// `OSArtifact`'s plural, given explicitly (not guessed) — ADD non-negotiable
/// #4, explicit over implicit.
pub const OSARTIFACT_PLURAL: &str = "osartifacts";

const OSARTIFACT_NAME_SUFFIX: &str = "-build";

/// Stable `reason` strings for `RawDiskArtifactStatus.reason`.
pub mod reasons {
    pub const PENDING: &str = "Pending";
    pub const BUILDING: &str = "Building";
    pub const READY: &str = "Reconciled";
    pub const FAILED: &str = "BuildFailed";
}

/// [`ApiResource`] describing kairos-operator's `OSArtifact` CRD. banlieue
/// never generates or installs this CRD — the platform operator installs
/// kairos-operator independently (`kubectl apply -k`, per its own docs). See
/// ADR-0010.
pub fn os_artifact_api_resource() -> ApiResource {
    let gvk = GroupVersionKind::gvk(OSARTIFACT_GROUP, OSARTIFACT_VERSION, OSARTIFACT_KIND);
    ApiResource::from_gvk_with_plural(&gvk, OSARTIFACT_PLURAL)
}

/// Deterministic `OSArtifact` name for a `VMImage`.
pub fn os_artifact_name(vmimage_name: &str) -> String {
    format!("{vmimage_name}{OSARTIFACT_NAME_SUFFIX}")
}

/// Find the first `Url`-kind source with `importFrom` set — the one and only
/// source `banlieue-imagebuilder` acts on. `Template` / `BackingFile`
/// sources, and any `Url` source missing `importFrom`, are ignored (nothing
/// to build).
pub fn find_url_source(sources: &[ImageSource]) -> Option<&ImageSource> {
    sources
        .iter()
        .find(|s| s.kind == ImageSourceKind::Url && s.import_from.is_some())
}

fn arch_str(architecture: &Architecture) -> &'static str {
    match architecture {
        Architecture::Amd64 => "amd64",
        Architecture::Arm64 => "arm64",
    }
}

/// Build the desired `OSArtifact` SSA-apply body for a `Url` source.
///
/// Requests a `cloudImage` (raw disk) build only — ISO / Azure / GCE
/// artifacts are never requested, since only the raw disk is consumed
/// downstream by a provider's per-zone import.
pub fn desired_os_artifact(
    name: &str,
    namespace: &str,
    source: &ImageSource,
    architecture: &Architecture,
) -> Value {
    json!({
        "apiVersion": format!("{OSARTIFACT_GROUP}/{OSARTIFACT_VERSION}"),
        "kind": OSARTIFACT_KIND,
        "metadata": {
            "name": name,
            "namespace": namespace,
        },
        "spec": {
            "image": {
                "ref": source.import_from,
            },
            "artifacts": {
                "cloudImage": true,
                "arch": arch_str(architecture),
            },
        },
    })
}

/// Minimal view of an `OSArtifact.status` this reconciler needs — extracted
/// once from the live object so the mapping logic below is pure and
/// testable without a live `kube::Api`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KairosArtifactStatusView {
    pub phase: Option<String>,
    pub message: Option<String>,
}

/// Extract a [`KairosArtifactStatusView`] from a live `OSArtifact`
/// [`DynamicObject`]'s underlying JSON (`.data`).
pub fn extract_kairos_status(data: &Value) -> KairosArtifactStatusView {
    KairosArtifactStatusView {
        phase: data["status"]["phase"].as_str().map(str::to_string),
        message: data["status"]["message"].as_str().map(str::to_string),
    }
}

/// Map kairos-operator's own `OSArtifact.status.phase` values onto
/// banlieue's 4-state [`RawDiskArtifactPhase`]. `Exporting` collapses into
/// `Building`; `Error` and any unrecognized value fail closed to `Failed` —
/// an unknown phase string must never be silently treated as progress.
pub fn map_kairos_phase(phase: &str) -> RawDiskArtifactPhase {
    match phase {
        "Pending" => RawDiskArtifactPhase::Pending,
        "Building" | "Exporting" => RawDiskArtifactPhase::Building,
        "Ready" => RawDiskArtifactPhase::Ready,
        _ => RawDiskArtifactPhase::Failed,
    }
}

fn reason_for_phase(phase: &RawDiskArtifactPhase) -> &'static str {
    match phase {
        RawDiskArtifactPhase::Pending => reasons::PENDING,
        RawDiskArtifactPhase::Building => reasons::BUILDING,
        RawDiskArtifactPhase::Ready => reasons::READY,
        RawDiskArtifactPhase::Failed => reasons::FAILED,
    }
}

/// kairos-operator's PVC-naming convention for an `OSArtifact`'s default
/// artifacts volume: `<name>-artifacts`.
fn artifacts_pvc_name(os_artifact_name: &str) -> String {
    format!("{os_artifact_name}-artifacts")
}

/// kairos-operator's file-naming convention for a `cloudImage` (raw disk)
/// output: `<name>.raw`.
fn raw_disk_file_name(os_artifact_name: &str) -> String {
    format!("{os_artifact_name}.raw")
}

/// Compute the [`RawDiskArtifactStatus`] to publish, given the `OSArtifact`'s
/// name and its current kairos status view. A missing `phase` (the
/// `OSArtifact` was just created; status not yet populated) is treated as
/// `Pending`.
pub fn compute_raw_disk_artifact_status(
    os_artifact_name: &str,
    view: &KairosArtifactStatusView,
) -> RawDiskArtifactStatus {
    let phase = view
        .phase
        .as_deref()
        .map(map_kairos_phase)
        .unwrap_or(RawDiskArtifactPhase::Pending);

    let (pvc_ref, disk_file) = if phase == RawDiskArtifactPhase::Ready {
        (
            Some(LocalObjectReference {
                name: artifacts_pvc_name(os_artifact_name),
            }),
            Some(raw_disk_file_name(os_artifact_name)),
        )
    } else {
        (None, None)
    };

    RawDiskArtifactStatus {
        reason: Some(reason_for_phase(&phase).to_string()),
        phase,
        os_artifact_ref: os_artifact_name.to_string(),
        pvc_ref,
        disk_file,
        message: view.message.clone(),
    }
}

/// Top-level reconcile entrypoint.
///
/// 1. Bail early (long requeue) if this `VMImage` has no `Url` source —
///    nothing for `banlieue-imagebuilder` to do.
/// 2. Server-side-apply the `OSArtifact` (idempotent; field manager
///    `banlieue.io/imagebuilder`).
/// 3. Read the `OSArtifact`'s current status and mirror it into
///    `VMImage.status.rawDiskArtifact`.
/// 4. Requeue based on phase: short while building, long once terminal
///    (`Ready` / `Failed`) so the owning provider's own watch — not a poll
///    loop here — drives the next step.
pub async fn reconcile(image: Arc<VMImage>, ctx: Arc<Context>) -> Result<Action> {
    let name = image.name_any();
    let generation = image.metadata.generation.unwrap_or(0);

    let span = tracing::info_span!("reconcile", kind = "VMImage", name = %name, generation);
    let _enter = span.enter();

    let Some(source) = find_url_source(&image.spec.sources) else {
        return Ok(requeue_long());
    };

    info!(build_namespace = %ctx.build_namespace, "reconciling VMImage build");

    let os_name = os_artifact_name(&name);
    let os_api: Api<DynamicObject> = Api::namespaced_with(
        ctx.client.clone(),
        &ctx.build_namespace,
        &os_artifact_api_resource(),
    );

    let desired = desired_os_artifact(
        &os_name,
        &ctx.build_namespace,
        source,
        &image.spec.architecture,
    );
    let params = PatchParams::apply(FIELD_MANAGER_IMAGEBUILDER).force();
    os_api
        .patch(&os_name, &params, &Patch::Apply(&desired))
        .await?;

    let view = match os_api.get(&os_name).await {
        Ok(obj) => extract_kairos_status(&obj.data),
        Err(kube::Error::Api(e)) if e.code == 404 => KairosArtifactStatusView::default(),
        Err(e) => return Err(Error::Kube(e)),
    };

    let raw_status = compute_raw_disk_artifact_status(&os_name, &view);
    let phase = raw_status.phase.clone();
    patch_vmimage_status(&ctx, &name, raw_status).await?;

    Ok(match phase {
        RawDiskArtifactPhase::Ready | RawDiskArtifactPhase::Failed => requeue_long(),
        RawDiskArtifactPhase::Pending => requeue_default(),
        RawDiskArtifactPhase::Building => requeue_default(),
    })
}

/// `error_policy` invoked on `reconcile` failure.
pub fn error_policy(_image: Arc<VMImage>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "imagebuilder vmimage reconcile error policy fired");
    requeue_on_error()
}

async fn patch_vmimage_status(
    ctx: &Context,
    name: &str,
    raw_disk_artifact: RawDiskArtifactStatus,
) -> Result<()> {
    let status = VMImageStatus {
        raw_disk_artifact: Some(raw_disk_artifact),
        ..VMImageStatus::default()
    };

    let patch = json!({
        "apiVersion": VMImage::api_version(&()).to_string(),
        "kind": VMImage::kind(&()).to_string(),
        "metadata": { "name": name },
        "status": status,
    });

    let api: Api<VMImage> = Api::all(ctx.client.clone());
    let params = PatchParams::apply(FIELD_MANAGER_IMAGEBUILDER).force();
    api.patch_status(name, &params, &Patch::Apply(&patch))
        .await?;
    Ok(())
}

#[cfg(test)]
#[path = "vmimage_tests.rs"]
mod vmimage_tests;
