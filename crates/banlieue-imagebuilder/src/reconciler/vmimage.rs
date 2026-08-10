// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VMImage` reconciler — drives the shared, provider-agnostic image build for
//! `Url`-kind sources via kairos-operator's `OSArtifact` CRD.
//!
//! For every `VMImage` with at least one `spec.sources[]` entry where
//! `kind == Url`, server-side-apply an `OSArtifact` (`build.kairos.io/v1alpha2`
//! — a CRD banlieue does not own, so it is modeled as a [`DynamicObject`]
//! rather than a typed `#[derive(CustomResource)]`; banlieue must never
//! generate or install kairos-operator's own CRD), watch its
//! `status.phase`, and mirror progress into `VMImage.status.buildArtifact`.
//! The requested artifact is typed by the `Url` source's provider class:
//! `iso` for vSphere (`auroraboot build-iso`, with a baked cloud-config from
//! `spec.cloudConfig`) or `cloudImage` (raw) for libvirt. `VMImage.status
//! .perProvider[]` is never touched here — that stays each provider's own
//! field, written by its own field manager. See ADR-0010 and ADR-0020.
//!
//! The pure helpers ([`find_url_source`], [`artifact_kind_for_class`],
//! [`desired_os_artifact`], [`map_kairos_phase`],
//! [`compute_build_artifact_status`]) take plain values so they're testable
//! without a live `kube::Api`.

use std::sync::Arc;

use banlieue_api::banlieue::{
    Architecture, BuildArtifactKind, BuildArtifactPhase, BuildArtifactStatus, ImageSource,
    ImageSourceKind, VMImage, VMImageStatus,
};
use banlieue_api::common::{CloudConfigSource, DEFAULT_CLOUD_CONFIG_KEY, LocalObjectReference};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use banlieue_provider_sdk::scheduling::BuildScheduling;
use banlieue_provider_sdk::ssa::FIELD_MANAGER_IMAGEBUILDER;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
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

/// Stable `reason` strings for `BuildArtifactStatus.reason`.
pub mod reasons {
    pub const PENDING: &str = "Pending";
    pub const BUILDING: &str = "Building";
    pub const READY: &str = "Reconciled";
    pub const FAILED: &str = "BuildFailed";
}

/// Provider class whose `Url` sources need a bootable ISO (built via
/// `auroraboot build-iso`) rather than a raw cloud image. Every other class
/// (libvirt) consumes the raw `cloudImage`. See ADR-0020.
pub const PROVIDER_CLASS_VSPHERE: &str = "vsphere";

/// Choose the [`BuildArtifactKind`] for a `Url` source based on its provider
/// class: `vsphere` needs an `iso`, everything else a raw `cloudImage`.
pub fn artifact_kind_for_class(provider_class: &str) -> BuildArtifactKind {
    if provider_class == PROVIDER_CLASS_VSPHERE {
        BuildArtifactKind::Iso
    } else {
        BuildArtifactKind::CloudImage
    }
}

/// The `spec.artifacts` boolean key kairos-operator uses to request this kind:
/// `iso` -> `iso`, `cloudImage` -> `cloudImage`.
fn artifacts_flag(kind: &BuildArtifactKind) -> &'static str {
    match kind {
        BuildArtifactKind::Iso => "iso",
        BuildArtifactKind::CloudImage => "cloudImage",
    }
}

/// The artifact file extension kairos-operator writes for this kind, without
/// the leading dot: `iso` -> `iso`, `cloudImage` -> `raw`.
fn artifact_extension(kind: &BuildArtifactKind) -> &'static str {
    match kind {
        BuildArtifactKind::Iso => "iso",
        BuildArtifactKind::CloudImage => "raw",
    }
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

/// Identity of the `VMImage` owning an `OSArtifact`, for the owner reference
/// that binds the artifact's lifecycle to the image (SEC-005).
#[derive(Debug, Clone, Copy)]
pub struct OwnerRef<'a> {
    /// `metadata.name` of the owning `VMImage`.
    pub name: &'a str,
    /// `metadata.uid` of the owning `VMImage`.
    pub uid: &'a str,
}

/// Build the desired `OSArtifact` SSA-apply body for a `Url` source.
///
/// Requests exactly one artifact — `iso` (vSphere) or `cloudImage` (libvirt),
/// per `kind` — since only that one is consumed downstream by the owning
/// provider's per-zone import. When `cloud_config` is set, its `secretRef` is
/// passed through as `spec.artifacts.cloudConfigRef` so kairos'
/// `auroraboot build-iso --cloud-config` bakes a default cloud-config into the
/// artifact (ADR-0020). The referenced Secret must live in `namespace` (the
/// imagebuild namespace), where the build pod mounts it.
///
/// The owner reference is what makes garbage collection work: a `VMImage` is
/// cluster-scoped and an `OSArtifact` namespaced, and a namespaced dependent
/// with a cluster-scoped owner is valid — deleting the image reaps the build.
/// `blockOwnerDeletion` stays off: setting it requires `update` on the owner's
/// `finalizers` subresource, RBAC this controller does not otherwise need.
// Eight parameters: each is a distinct, unrelated input to the manifest
// (identity, source, arch, artifact kind, cloud-config, owner, scheduling).
// Bundling them into a struct would only move the same fields behind one more
// layer without improving call-site clarity.
#[allow(clippy::too_many_arguments)]
pub fn desired_os_artifact(
    name: &str,
    namespace: &str,
    source: &ImageSource,
    architecture: &Architecture,
    kind: &BuildArtifactKind,
    cloud_config: Option<&CloudConfigSource>,
    owner: Option<OwnerRef<'_>>,
    scheduling: &BuildScheduling,
) -> Value {
    let owner_references = owner.map(|o| {
        json!([{
            "apiVersion": VMImage::api_version(&()).to_string(),
            "kind": VMImage::kind(&()).to_string(),
            "name": o.name,
            "uid": o.uid,
            "controller": true,
            "blockOwnerDeletion": false,
        }])
    });
    // Build the spec incrementally so optional scheduling fields are *omitted*
    // when unconfigured, not emitted as `null`. The OSArtifact CRD types
    // `nodeSelector` as an object and `tolerations` as an array and rejects a
    // literal `null` with a 422 ("must be of type object/array"), so a `json!`
    // that leaves the key present with a null value never applies. Absent →
    // kairos' default scheduling (ADR-0016 follow-up); present only when set.
    let mut spec = serde_json::Map::new();
    spec.insert("image".to_string(), json!({ "ref": source.import_from }));
    // Request exactly the one artifact kind this build serves. `cloudConfigRef`
    // is added only when a cloud-config source is set, and only its `secretRef`
    // is honoured today (ADR-0020, secretRef-first).
    let mut artifacts = serde_json::Map::new();
    artifacts.insert(artifacts_flag(kind).to_string(), json!(true));
    artifacts.insert("arch".to_string(), json!(arch_str(architecture)));
    if let Some(secret_ref) = cloud_config.and_then(|cc| cc.secret_ref.as_ref()) {
        artifacts.insert(
            "cloudConfigRef".to_string(),
            json!({
                "name": secret_ref.name,
                "key": secret_ref.key_or(DEFAULT_CLOUD_CONFIG_KEY),
            }),
        );
    }
    spec.insert("artifacts".to_string(), Value::Object(artifacts));
    if !scheduling.node_selector.is_empty() {
        spec.insert(
            "nodeSelector".to_string(),
            serde_json::to_value(&scheduling.node_selector).unwrap_or_else(|_| json!({})),
        );
    }
    if !scheduling.tolerations.is_empty() {
        spec.insert(
            "tolerations".to_string(),
            serde_json::to_value(&scheduling.tolerations).unwrap_or_else(|_| json!([])),
        );
    }

    let mut metadata = serde_json::Map::new();
    metadata.insert("name".to_string(), json!(name));
    metadata.insert("namespace".to_string(), json!(namespace));
    // Same rule for ownerReferences: omit when there is no owner rather than
    // sending metadata.ownerReferences: null.
    if let Some(refs) = owner_references {
        metadata.insert("ownerReferences".to_string(), refs);
    }

    json!({
        "apiVersion": format!("{OSARTIFACT_GROUP}/{OSARTIFACT_VERSION}"),
        "kind": OSARTIFACT_KIND,
        "metadata": Value::Object(metadata),
        "spec": Value::Object(spec),
    })
}

/// True when the live `OSArtifact` carries an owner reference to this
/// `VMImage` UID. UID, not name: a deleted-and-recreated `VMImage` reuses the
/// name but never the UID, and trusting the name is exactly the stale-`Ready`
/// hole from SEC-005.
///
/// Takes the TYPED metadata, not the DynamicObject's `data`: kube parses
/// `metadata` out of the flattened JSON, so `data["metadata"]` is always
/// null and reading it there silently reports every object as unowned.
pub fn owner_uid_matches(refs: Option<&[OwnerReference]>, uid: &str) -> bool {
    refs.is_some_and(|refs| refs.iter().any(|r| r.uid == uid))
}

/// True when the live `OSArtifact`'s spec requests exactly this build —
/// same image ref, same architecture, and same artifact kind. A changed kind
/// (e.g. the `Url` source moved from libvirt to vSphere) forces a rebuild:
/// the old `cloudImage`/`iso` output is the wrong shape for the new consumer.
pub fn spec_matches(data: &Value, import_from: &str, arch: &str, kind: &BuildArtifactKind) -> bool {
    data["spec"]["image"]["ref"].as_str() == Some(import_from)
        && data["spec"]["artifacts"]["arch"].as_str() == Some(arch)
        && data["spec"]["artifacts"][artifacts_flag(kind)].as_bool() == Some(true)
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
/// banlieue's 4-state [`BuildArtifactPhase`]. `Exporting` collapses into
/// `Building`; `Error` and any unrecognized value fail closed to `Failed` —
/// an unknown phase string must never be silently treated as progress.
pub fn map_kairos_phase(phase: &str) -> BuildArtifactPhase {
    match phase {
        "Pending" => BuildArtifactPhase::Pending,
        "Building" | "Exporting" => BuildArtifactPhase::Building,
        "Ready" => BuildArtifactPhase::Ready,
        _ => BuildArtifactPhase::Failed,
    }
}

fn reason_for_phase(phase: &BuildArtifactPhase) -> &'static str {
    match phase {
        BuildArtifactPhase::Pending => reasons::PENDING,
        BuildArtifactPhase::Building => reasons::BUILDING,
        BuildArtifactPhase::Ready => reasons::READY,
        BuildArtifactPhase::Failed => reasons::FAILED,
    }
}

/// kairos-operator's PVC-naming convention for an `OSArtifact`'s default
/// artifacts volume: `<name>-artifacts`.
fn artifacts_pvc_name(os_artifact_name: &str) -> String {
    format!("{os_artifact_name}-artifacts")
}

/// kairos-operator's file-naming convention for an artifact output:
/// `<name>.raw` for a `cloudImage`, `<name>.iso` for an `iso`.
fn artifact_file_name(os_artifact_name: &str, kind: &BuildArtifactKind) -> String {
    format!("{os_artifact_name}.{}", artifact_extension(kind))
}

/// Compute the [`BuildArtifactStatus`] to publish, given the `OSArtifact`'s
/// name, the artifact `kind`, and its current kairos status view. A missing
/// `phase` (the `OSArtifact` was just created; status not yet populated) is
/// treated as `Pending`. `checksum` is copied from the `Url` source verbatim —
/// consumers verify the artifact against it (SEC-004).
pub fn compute_build_artifact_status(
    os_artifact_name: &str,
    kind: BuildArtifactKind,
    view: &KairosArtifactStatusView,
    checksum: Option<&str>,
) -> BuildArtifactStatus {
    let phase = view
        .phase
        .as_deref()
        .map(map_kairos_phase)
        .unwrap_or(BuildArtifactPhase::Pending);

    let (pvc_ref, file) = if phase == BuildArtifactPhase::Ready {
        (
            Some(LocalObjectReference {
                name: artifacts_pvc_name(os_artifact_name),
            }),
            Some(artifact_file_name(os_artifact_name, &kind)),
        )
    } else {
        (None, None)
    };

    BuildArtifactStatus {
        kind,
        reason: Some(reason_for_phase(&phase).to_string()),
        phase,
        os_artifact_ref: os_artifact_name.to_string(),
        pvc_ref,
        file,
        message: view.message.clone(),
        checksum: checksum.map(str::to_string),
    }
}

/// Top-level reconcile entrypoint.
///
/// 1. Bail early (long requeue) if this `VMImage` has no `Url` source —
///    nothing for `banlieue-imagebuilder` to do.
/// 2. Read the live `OSArtifact`. If it exists but is **not owned by this
///    `VMImage`'s UID** or **does not request the current build**, delete it
///    and stop (SEC-005): kairos' status carries no `observedGeneration` and
///    no digest to bind a `Ready` to the spec, so object identity is the only
///    anchor — a stale or foreign artifact is rebuilt from scratch rather
///    than trusted. The next pass recreates it fresh.
/// 3. Server-side-apply the `OSArtifact` (idempotent; field manager
///    `banlieue.io/imagebuilder`), owned by this `VMImage` so garbage
///    collection reaps the build when the image is deleted.
/// 4. Mirror the `OSArtifact`'s current status into
///    `VMImage.status.buildArtifact`.
/// 5. Requeue based on phase: short while building, long once terminal
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

    let uid = image.metadata.uid.clone().unwrap_or_default();
    let os_name = os_artifact_name(&name);
    let os_api: Api<DynamicObject> = Api::namespaced_with(
        ctx.client.clone(),
        &ctx.build_namespace,
        &os_artifact_api_resource(),
    );

    let live = match os_api.get(&os_name).await {
        Ok(obj) => Some(obj),
        Err(kube::Error::Api(e)) if e.code == 404 => None,
        Err(e) => return Err(Error::Kube(e)),
    };

    let kind = artifact_kind_for_class(&source.provider_class);

    if let Some(obj) = &live {
        let owned = owner_uid_matches(obj.metadata.owner_references.as_deref(), &uid);
        let current = spec_matches(
            &obj.data,
            source.import_from.as_deref().unwrap_or_default(),
            arch_str(&image.spec.architecture),
            &kind,
        );
        if !owned || !current {
            info!(
                owned,
                current, "OSArtifact is stale or foreign; deleting for rebuild"
            );
            os_api
                .delete(&os_name, &kube::api::DeleteParams::default())
                .await?;
            let view = KairosArtifactStatusView {
                phase: None,
                message: Some(
                    "replaced a stale or foreign OSArtifact; rebuild starts next pass".to_string(),
                ),
            };
            let build_status = compute_build_artifact_status(
                &os_name,
                kind.clone(),
                &view,
                source.checksum.as_deref(),
            );
            patch_vmimage_status(&ctx, &name, build_status).await?;
            return Ok(requeue_default());
        }
    }

    let desired = desired_os_artifact(
        &os_name,
        &ctx.build_namespace,
        source,
        &image.spec.architecture,
        &kind,
        image.spec.cloud_config.as_ref(),
        Some(OwnerRef {
            name: &name,
            uid: &uid,
        }),
        &ctx.scheduling,
    );
    let params = PatchParams::apply(FIELD_MANAGER_IMAGEBUILDER).force();
    os_api
        .patch(&os_name, &params, &Patch::Apply(&desired))
        .await?;

    let view = live
        .as_ref()
        .map(|obj| extract_kairos_status(&obj.data))
        .unwrap_or_default();

    let build_status =
        compute_build_artifact_status(&os_name, kind, &view, source.checksum.as_deref());
    let phase = build_status.phase.clone();
    patch_vmimage_status(&ctx, &name, build_status).await?;

    Ok(match phase {
        BuildArtifactPhase::Ready | BuildArtifactPhase::Failed => requeue_long(),
        BuildArtifactPhase::Pending => requeue_default(),
        BuildArtifactPhase::Building => requeue_default(),
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
    build_artifact: BuildArtifactStatus,
) -> Result<()> {
    let status = VMImageStatus {
        build_artifact: Some(build_artifact),
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
