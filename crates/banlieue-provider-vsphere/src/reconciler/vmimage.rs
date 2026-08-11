// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VMImage` reconciler — template availability and `Url`-source import
//! progress on vSphere.
//!
//! For every `Provider` of class `vsphere` in scope, resolve the vsphere
//! `ImageSource` named in `VMImage.spec.sources[]` (where `provider_class ==
//! "vsphere"`):
//!
//! - `Template` sources: look up the template by name; flip the matching
//!   `VMImage.status.perProvider[]` entry to `ready=true` (with
//!   `resolved_ref` populated) when found in every datacenter the Provider
//!   has a failure domain in.
//! - `Url` sources (ADR-0010 / ADR-0020): readiness depends on
//!   `VMImage.status.buildArtifact` (an `iso`, built by `banlieue-imagebuilder`)
//!   — this reconciler never writes that field, only reads it. Once it reports
//!   `Ready`, one per-zone import Job is ensured per
//!   `Provider.status.failureDomains[]` (running the `image-import` subcommand,
//!   [`crate::import`]); each Job's success/failure is translated into a
//!   [`ZoneImageStatus`]. `useContentLibrary` is a documented follow-up.
//! - `BackingFile` sources are not a vsphere concept and are rejected.
//!
//! `ready=false` in every non-terminal case carries a stable [`reasons`] tag.
//!
//! The pure helpers ([`compute_template_status`], [`gate_on_build_artifact`],
//! [`build_import_job`], [`import_job_name`], [`zone_from_job`]) take plain
//! values / `&dyn VSphereClient` so the reconciler tests drive them with
//! `FakeClient` and never touch `kube::Api`; the Job-creating step
//! (`ensure_import_jobs`) is the only part that needs a cluster.

use std::sync::Arc;

use std::collections::BTreeMap;

use banlieue_api::banlieue::{
    BuildArtifactKind, BuildArtifactPhase, BuildArtifactStatus, FailureDomain,
    ImagePerProviderStatus, ImageSource, ImageSourceKind, NicAdapter, Provider, VMImage,
    VMImageStatus, VMImageTemplateDisk, ZoneImageStatus,
};
use banlieue_api::common::Firmware;
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_PROVIDER_VSPHERE;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{Secret, Toleration};
use kube::{
    Resource, ResourceExt,
    api::{Api, ListParams, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::provider::PROVIDER_CLASS_NAME;
use crate::client::{Credentials, Datacenter, VSphereClient};
use crate::context::Context;
use crate::error::{Error, Result};

const SECRET_KEY_USERNAME: &str = "username";
const SECRET_KEY_PASSWORD: &str = "password";

/// Annotation on a `VMImage` requesting a forced re-import: the per-zone import
/// Jobs are deleted and recreated, so a completed import re-runs (ADR-0020).
/// Value is truthy (`"true"`, case-insensitive). Orthogonal to
/// `spec.forceUpload` / `spec.forceCreate`, which control what the (re)run does.
pub const ANNOTATION_FORCE_REIMPORT: &str = "banlieue.io/force-reimport";

/// True when the `VMImage` carries a truthy `banlieue.io/force-reimport`
/// annotation. Pure, so the trigger rule is unit-testable.
pub fn force_reimport_requested(image: &VMImage) -> bool {
    image
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(ANNOTATION_FORCE_REIMPORT))
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

/// The three force knobs that drive the per-zone import, bundled so they thread
/// through the reconcile chain as one value.
///
/// - `reimport` (annotation `banlieue.io/force-reimport`): delete + recreate the
///   import Job so a completed import re-runs.
/// - `upload` (`spec.forceUpload`) / `create` (`spec.forceCreate`): baked into
///   the Job's `--force-upload` / `--force-create` flags — what the run *does*
///   (replace the ISO / recreate the template).
#[derive(Debug, Clone, Default)]
pub struct ImportForce {
    pub reimport: bool,
    pub upload: bool,
    pub create: bool,
    /// Port group from `spec.template.network`; `None` → zone-derived.
    pub network: Option<String>,
    /// NIC adapter type from `spec.template.networkAdapter`; `None` → Job default.
    pub network_adapter: Option<NicAdapter>,
    /// NIC PCI slot from `spec.template.nicPciSlot`; `None` → Job default (192).
    pub nic_pci_slot: Option<i32>,
    /// Install disk from `spec.template.disk`; `None` → the Job's own default.
    pub disk: Option<VMImageTemplateDisk>,
    /// CPU count from `spec.template.cpus`; `None` → Job default (2).
    pub cpus: Option<i32>,
    /// Memory (MiB) from `spec.template.memoryMib`; `None` → Job default (4096).
    pub memory_mib: Option<i64>,
    /// Firmware from `spec.template.firmware`; `None` → Job default (efi).
    pub firmware: Option<Firmware>,
    /// vCenter `guestId` override from `spec.template.guestId`; `None` → derived
    /// from the VMImage OS by the Job.
    pub guest_id: Option<String>,
    /// Target vCenter folder from `spec.template.folder`; `None` → datacenter
    /// VM-folder root. Bundled here so it threads through with the force knobs.
    pub folder: Option<String>,
}

impl ImportForce {
    /// Read the force knobs + template settings off a `VMImage` (the
    /// `banlieue.io/force-reimport` annotation + `spec.template`).
    pub fn from_image(image: &VMImage) -> Self {
        let t = image.spec.template.as_ref();
        Self {
            reimport: force_reimport_requested(image),
            upload: t.is_some_and(|t| t.force_upload),
            create: t.is_some_and(|t| t.force_create),
            network: t.and_then(|t| t.network.clone()),
            network_adapter: t.and_then(|t| t.network_adapter),
            nic_pci_slot: t.and_then(|t| t.nic_pci_slot),
            disk: t.and_then(|t| t.disk.clone()),
            cpus: t.and_then(|t| t.cpus),
            memory_mib: t.and_then(|t| t.memory_mib),
            firmware: t.and_then(|t| t.firmware.clone()),
            guest_id: t.and_then(|t| t.guest_id.clone()),
            folder: t.and_then(|t| t.folder.clone()),
        }
    }
}

/// Stable `reason` strings for `ImagePerProviderStatus.reason` and the
/// aggregate `Ready` condition. Operators match against these.
pub mod reasons {
    /// All resolved providers have the template available.
    pub const RECONCILED: &str = "Reconciled";
    /// At least one vSphere Provider does not have the template in any
    /// reachable datacenter.
    pub const TEMPLATE_NOT_FOUND: &str = "TemplateNotFound";
    /// The Provider's credentials Secret is missing or malformed.
    pub const SECRET_UNAVAILABLE: &str = "SecretUnavailable";
    /// We could not connect to the Provider's vCenter.
    pub const CONNECT_FAILED: &str = "ConnectFailed";
    /// vCenter rejected the inventory walk during template lookup.
    pub const LOOKUP_FAILED: &str = "LookupFailed";
    /// No vSphere ImageSource on this VMImage — nothing to do for this provider class.
    pub const NO_VSPHERE_SOURCE: &str = "NoVSphereSource";
    /// `Url` source: `VMImage.status.buildArtifact` isn't `Ready` yet
    /// (missing, `Pending`, or `Building`) — waiting on `banlieue-imagebuilder`.
    pub const BUILD_PENDING: &str = "BuildPending";
    /// `Url` source: `VMImage.status.buildArtifact.phase == Failed`.
    pub const BUILD_FAILED: &str = "BuildFailed";
    /// `Url` source, ISO `Ready`: the Provider has no
    /// `status.failureDomains[]` published yet, so there is nowhere to import
    /// into.
    pub const NO_FAILURE_DOMAINS: &str = "NoFailureDomains";
    /// A per-zone import Job is running (uploading the ISO / creating the
    /// template) for this failure domain.
    pub const IMPORTING: &str = "Importing";
    /// The per-zone import Job for this failure domain failed.
    pub const IMPORT_FAILED: &str = "ImportFailed";
    /// `Url` source, artifact `Ready` but its `kind` is not `iso` — the vSphere
    /// provider only imports ISO artifacts (ADR-0020). Defensive:
    /// `banlieue-imagebuilder` requests `iso` for vSphere sources, so this
    /// should not occur unless the build pipeline is misconfigured.
    pub const WRONG_ARTIFACT_KIND: &str = "WrongArtifactKind";
    /// `Url` source with `Provider.spec.useContentLibrary: true` — the Content
    /// Library import path is not implemented yet (ADR-0020 follow-up); the
    /// default datastore-upload + `MarkAsTemplate` path is the supported one.
    pub const CONTENT_LIBRARY_NOT_IMPLEMENTED: &str = "ContentLibraryNotImplemented";
    /// This `ImageSource.kind` is not supported by the vsphere provider
    /// (`BackingFile` is a libvirt-shaped concept). Defensive — unreachable
    /// via [`find_vsphere_source`]'s own filter, kept in case that contract
    /// ever changes.
    pub const UNSUPPORTED_SOURCE_KIND: &str = "UnsupportedSourceKind";
}

/// Top-level reconcile entrypoint.
///
/// 1. Read the `VMImage` spec and bail early if no vsphere `ImageSource` is
///    declared (other providers handle their own classes).
/// 2. List every `Provider` (cluster-wide or scoped) of class `vsphere`.
/// 3. For each Provider, connect and look up the template name in every
///    failure-domain datacenter.
/// 4. SSA-patch `VMImage.status.perProvider[]` with the per-provider rows
///    and set the aggregate `Ready` condition.
pub async fn reconcile(image: Arc<VMImage>, ctx: Arc<Context>) -> Result<Action> {
    let name = image.name_any();
    let generation = image.metadata.generation.unwrap_or(0);

    let span = tracing::info_span!(
        "reconcile",
        kind = "VMImage",
        name = %name,
        generation,
    );
    let _enter = span.enter();
    info!("reconciling VMImage");

    let Some(vsphere_source) = find_vsphere_source(&image.spec.sources) else {
        // Not our concern — every other provider handles its own ImageSources.
        return Ok(requeue_long());
    };

    let providers = list_vsphere_providers(&ctx).await?;
    if providers.is_empty() {
        info!("no vsphere Providers in scope — leaving status untouched");
        return Ok(requeue_long());
    }

    let build_artifact = image
        .status
        .as_ref()
        .and_then(|s| s.build_artifact.as_ref());

    let force = ImportForce::from_image(&image);

    let mut rows: Vec<ImagePerProviderStatus> = Vec::with_capacity(providers.len());
    let mut any_pending = false;
    for provider in &providers {
        let row = reconcile_for_provider(
            &ctx,
            provider,
            vsphere_source,
            build_artifact,
            &name,
            &force,
        )
        .await;
        any_pending |= !row.ready;
        rows.push(row);
    }

    patch_vmimage_status(&ctx, &name, generation, rows).await?;

    // Poll while a per-zone import is still running; back off once every
    // provider row is ready (or terminally stuck on a reason the operator must
    // act on, in which case a spec/status change re-triggers us anyway).
    Ok(if any_pending {
        requeue_default()
    } else {
        requeue_long()
    })
}

/// `error_policy` invoked on `reconcile` failure.
pub fn error_policy(_image: Arc<VMImage>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "vmimage reconcile error policy fired");
    requeue_on_error()
}

/// Resolve the per-provider status row for one `(Provider, vsphere
/// ImageSource)` pair. `Template` sources connect to the Provider's vCenter,
/// walk its failure-domain datacenters, and confirm the template exists in
/// each — errors become `ready=false` rows with a stable `reason`; never
/// returns `Err`. `Url` sources never touch vCenter — see
/// [`compute_url_source_status`].
pub async fn reconcile_for_provider(
    ctx: &Context,
    provider: &Provider,
    source: &ImageSource,
    build_artifact: Option<&BuildArtifactStatus>,
    image_name: &str,
    force: &ImportForce,
) -> ImagePerProviderStatus {
    match source.kind {
        ImageSourceKind::Url => {
            return reconcile_url_source(ctx, provider, build_artifact, image_name, force).await;
        }
        ImageSourceKind::BackingFile => {
            return per_provider_failure(
                provider,
                reasons::UNSUPPORTED_SOURCE_KIND,
                "BackingFile sources are not supported by the vsphere provider".to_string(),
            );
        }
        ImageSourceKind::Template => {}
    }

    let namespace = provider.namespace().unwrap_or_default();

    let creds = match read_credentials(ctx, &namespace, provider).await {
        Ok(c) => c,
        Err(e) => {
            return per_provider_failure(provider, reasons::SECRET_UNAVAILABLE, e.to_string());
        }
    };
    let ca_bundle_pem = match crate::reconciler::ca_bundle::resolve_ca_bundle(
        ctx,
        &namespace,
        &provider.spec.connection.ca_bundle,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => return per_provider_failure(provider, reasons::CONNECT_FAILED, e.to_string()),
    };
    let client = match ctx
        .vsphere
        .build(&provider.spec.connection, &creds, ca_bundle_pem.as_deref())
        .await
    {
        Ok(c) => c,
        Err(e) => return per_provider_failure(provider, reasons::CONNECT_FAILED, e.to_string()),
    };

    let datacenters = match dcs_from_provider_status(provider, client.as_ref()).await {
        Ok(v) => v,
        Err(e) => {
            return per_provider_failure(provider, reasons::LOOKUP_FAILED, e.to_string());
        }
    };

    compute_template_status(client.as_ref(), &datacenters, &source.reference, provider).await
}

/// Pure helper for the per-provider template check: given a connected client
/// and a list of candidate datacenters, return a populated
/// [`ImagePerProviderStatus`] row.
pub async fn compute_template_status(
    client: &dyn VSphereClient,
    datacenters: &[Datacenter],
    template_name: &str,
    provider: &Provider,
) -> ImagePerProviderStatus {
    if datacenters.is_empty() {
        return per_provider_failure(
            provider,
            reasons::TEMPLATE_NOT_FOUND,
            "no datacenters discovered for this Provider".to_string(),
        );
    }

    let mut hits = Vec::new();
    for dc in datacenters {
        match client.find_template(dc, template_name).await {
            Ok(Some(t)) => hits.push((dc.name.clone(), t)),
            Ok(None) => {}
            Err(e) => {
                return per_provider_failure(provider, reasons::LOOKUP_FAILED, e.to_string());
            }
        }
    }

    if hits.is_empty() {
        return per_provider_failure(
            provider,
            reasons::TEMPLATE_NOT_FOUND,
            format!("template {template_name:?} not present in any datacenter"),
        );
    }

    let resolved = render_resolved_ref(&hits, template_name);
    ImagePerProviderStatus {
        provider_name: provider.name_any(),
        provider_namespace: provider.namespace().unwrap_or_default(),
        ready: true,
        resolved_ref: Some(resolved),
        reason: Some(reasons::RECONCILED.to_string()),
        message: None,
        zones: vec![],
    }
}

/// Pick the first vsphere `ImageSource` of kind `Template` or `Url`
/// (ADR-0010). `BackingFile` is a libvirt-shaped concept vsphere never
/// declares, and is never matched here.
pub fn find_vsphere_source(sources: &[ImageSource]) -> Option<&ImageSource> {
    sources.iter().find(|s| {
        s.provider_class == PROVIDER_CLASS_NAME
            && matches!(s.kind, ImageSourceKind::Template | ImageSourceKind::Url)
    })
}

/// Resolve the per-provider status row for a `Url`-kind vsphere source
/// (ADR-0010 / ADR-0020). Readiness depends on `VMImage.status.buildArtifact`
/// (written exclusively by `banlieue-imagebuilder`); once it reports an `iso`
/// artifact `Ready`, one per-zone import Job is ensured per
/// `Provider.status.failureDomains[]` and its state translated into a
/// [`ZoneImageStatus`]. Never writes `buildArtifact` — only reads it.
async fn reconcile_url_source(
    ctx: &Context,
    provider: &Provider,
    build_artifact: Option<&BuildArtifactStatus>,
    image_name: &str,
    force: &ImportForce,
) -> ImagePerProviderStatus {
    let artifact = match gate_on_build_artifact(build_artifact) {
        Err((reason, message)) => return per_provider_failure(provider, reason, message),
        Ok(a) => a,
    };

    // ADR-0020: the Content Library import path is a documented follow-up; the
    // supported path is datastore-upload + MarkAsTemplate (default, CL off).
    if provider.spec.use_content_library {
        return per_provider_failure(
            provider,
            reasons::CONTENT_LIBRARY_NOT_IMPLEMENTED,
            "Provider.spec.useContentLibrary=true, but the Content Library import path is not implemented yet (ADR-0020 follow-up)".to_string(),
        );
    }

    let failure_domains: &[FailureDomain] = provider
        .status
        .as_ref()
        .map(|s| s.failure_domains.as_slice())
        .unwrap_or_default();
    if failure_domains.is_empty() {
        return per_provider_failure(
            provider,
            reasons::NO_FAILURE_DOMAINS,
            "Provider has no status.failureDomains[] published yet".to_string(),
        );
    }

    let zones =
        ensure_import_jobs(ctx, provider, image_name, artifact, failure_domains, force).await;
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
    ImagePerProviderStatus {
        provider_name: provider.name_any(),
        provider_namespace: provider.namespace().unwrap_or_default(),
        ready,
        resolved_ref: None,
        reason: Some(reason.to_string()),
        message: None,
        zones,
    }
}

/// Decide whether the shared build artifact is usable by the vSphere importer
/// yet. Pure, so the gating rules are unit-testable without kube or vCenter.
///
/// # Errors
/// A `(reason, message)` pair when the artifact is missing, not `Ready`, or the
/// wrong kind (vSphere consumes only `iso`, ADR-0020).
pub fn gate_on_build_artifact(
    build_artifact: Option<&BuildArtifactStatus>,
) -> std::result::Result<&BuildArtifactStatus, (&'static str, String)> {
    let Some(a) = build_artifact else {
        return Err((
            reasons::BUILD_PENDING,
            "waiting for banlieue-imagebuilder (VMImage.status.buildArtifact not set yet)"
                .to_string(),
        ));
    };
    match a.phase {
        BuildArtifactPhase::Pending | BuildArtifactPhase::Building => Err((
            reasons::BUILD_PENDING,
            format!("ISO build in progress ({:?})", a.phase),
        )),
        BuildArtifactPhase::Failed => Err((
            reasons::BUILD_FAILED,
            a.message
                .clone()
                .unwrap_or_else(|| "ISO build failed".to_string()),
        )),
        BuildArtifactPhase::Ready if a.kind != BuildArtifactKind::Iso => Err((
            reasons::WRONG_ARTIFACT_KIND,
            format!(
                "buildArtifact.kind is {:?}, but the vSphere provider imports only iso artifacts",
                a.kind
            ),
        )),
        BuildArtifactPhase::Ready => Ok(a),
    }
}

/// Deterministic Job name for one (image, provider, failure-domain) import.
///
/// Deterministic so a re-reconcile adopts the existing Job instead of starting
/// a second copy of a multi-gigabyte ISO transfer.
pub fn import_job_name(image: &str, provider: &str, failure_domain: &str) -> String {
    let raw = format!("import-{image}-{provider}-{failure_domain}");
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .to_lowercase();
    // Kubernetes names cap at 63 characters.
    cleaned.chars().take(63).collect::<String>()
}

/// Create the import Job for each failure domain that lacks one, and translate
/// existing Job state into [`ZoneImageStatus`].
///
/// When `force.reimport` (VMImage `banlieue.io/force-reimport` annotation), any
/// existing Job is deleted first so a completed import re-runs. The (re)created
/// Job carries `--force-upload` / `--force-create` per `force.upload` /
/// `force.create` (from `spec`).
async fn ensure_import_jobs(
    ctx: &Context,
    provider: &Provider,
    image_name: &str,
    artifact: &BuildArtifactStatus,
    failure_domains: &[FailureDomain],
    force: &ImportForce,
) -> Vec<ZoneImageStatus> {
    let api: Api<Job> = Api::namespaced(ctx.client.clone(), &ctx.build_namespace);
    let mut zones = Vec::with_capacity(failure_domains.len());

    for fd in failure_domains {
        let job_name = import_job_name(image_name, &provider.name_any(), &fd.name);

        // Forced re-import: delete any existing Job so it is recreated and
        // re-runs. Deletion is not instant — if it is still terminating when we
        // (re)create below, the apply reports an error and we retry next pass.
        // A 404 (nothing to delete) is fine.
        if force.reimport {
            match api
                .delete(&job_name, &kube::api::DeleteParams::background())
                .await
            {
                Ok(_) => {}
                Err(kube::Error::Api(e)) if e.code == 404 => {}
                Err(e) => {
                    zones.push(zone_row(
                        &fd.name,
                        false,
                        reasons::IMPORT_FAILED,
                        &e.to_string(),
                    ));
                    continue;
                }
            }
        }

        // On reimport always (re)create; otherwise adopt an existing Job.
        let zone = if force.reimport {
            create_import_job(
                &api, ctx, provider, image_name, &fd.name, &job_name, artifact, force,
            )
            .await
        } else {
            match api.get(&job_name).await {
                Ok(job) => zone_from_job(&fd.name, &job_name, &job),
                Err(kube::Error::Api(e)) if e.code == 404 => {
                    create_import_job(
                        &api, ctx, provider, image_name, &fd.name, &job_name, artifact, force,
                    )
                    .await
                }
                Err(e) => zone_row(&fd.name, false, reasons::IMPORT_FAILED, &e.to_string()),
            }
        };
        zones.push(zone);
    }
    zones
}

/// Server-side-apply one per-zone import Job and translate the outcome into a
/// zone row. An apply that races a still-terminating prior Job reports
/// `Importing` (retried next pass), not `ImportFailed`.
#[allow(clippy::too_many_arguments)]
async fn create_import_job(
    api: &Api<Job>,
    ctx: &Context,
    provider: &Provider,
    image_name: &str,
    failure_domain: &str,
    job_name: &str,
    artifact: &BuildArtifactStatus,
    force: &ImportForce,
) -> ZoneImageStatus {
    let spec = build_import_job(&ImportJobInputs {
        job_name,
        namespace: &ctx.build_namespace,
        image: &ctx.import_image,
        service_account: Some(ctx.import_service_account.as_str()),
        vmimage: image_name,
        provider,
        failure_domain,
        artifact,
        tolerations: &ctx.import_tolerations,
        force_upload: force.upload,
        force_create: force.create,
        network: force.network.as_deref(),
        network_adapter: force.network_adapter,
        nic_pci_slot: force.nic_pci_slot,
        disk: force.disk.as_ref(),
        cpus: force.cpus,
        memory_mib: force.memory_mib,
        firmware: force.firmware.as_ref(),
        guest_id: force.guest_id.as_deref(),
        folder: force.folder.as_deref(),
    });
    match api
        .patch(
            job_name,
            &PatchParams::apply(FIELD_MANAGER_PROVIDER_VSPHERE).force(),
            &Patch::Apply(&spec),
        )
        .await
    {
        Ok(_) => zone_row(
            failure_domain,
            false,
            reasons::IMPORTING,
            "import Job created",
        ),
        Err(e) => zone_row(failure_domain, false, reasons::IMPORTING, &e.to_string()),
    }
}

/// Map a Job's status onto a zone row.
pub fn zone_from_job(failure_domain: &str, job_name: &str, job: &Job) -> ZoneImageStatus {
    let status = job.status.as_ref();
    let succeeded = status.and_then(|s| s.succeeded).unwrap_or(0);
    let failed = status.and_then(|s| s.failed).unwrap_or(0);

    if succeeded > 0 {
        return ZoneImageStatus {
            name: failure_domain.to_string(),
            ready: true,
            resolved_ref: Some(format!("{failure_domain}/{job_name}")),
            reason: Some(reasons::RECONCILED.to_string()),
            message: None,
        };
    }
    if failed > 0 {
        return zone_row(
            failure_domain,
            false,
            reasons::IMPORT_FAILED,
            &format!("import Job {job_name} failed"),
        );
    }
    zone_row(
        failure_domain,
        false,
        reasons::IMPORTING,
        "import Job running",
    )
}

fn zone_row(failure_domain: &str, ready: bool, reason: &str, message: &str) -> ZoneImageStatus {
    ZoneImageStatus {
        name: failure_domain.to_string(),
        ready,
        resolved_ref: None,
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
    }
}

/// Everything one per-zone import Job needs to know.
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
    /// `VMImage` being imported (also the target template name).
    pub vmimage: &'a str,
    /// `Provider` whose vCenter receives the template.
    pub provider: &'a Provider,
    /// Target failure domain (zone) — one vSphere compute cluster / datastore.
    pub failure_domain: &'a str,
    /// The ISO to upload, as published by banlieue-imagebuilder.
    pub artifact: &'a BuildArtifactStatus,
    /// Taints the Job may tolerate (placement follows the artifacts PVC).
    pub tolerations: &'a [Toleration],
    /// Pass `--force-upload` so the import re-uploads the ISO (spec.forceUpload).
    pub force_upload: bool,
    /// Pass `--force-create` so the import recreates the template (spec.forceCreate).
    pub force_create: bool,
    /// Port group override (`spec.template.network`); `None` → zone-derived.
    pub network: Option<&'a str>,
    /// NIC adapter type (`spec.template.networkAdapter`); `None` → Job default.
    pub network_adapter: Option<NicAdapter>,
    /// NIC PCI slot (`spec.template.nicPciSlot`); `None` → Job default (192).
    pub nic_pci_slot: Option<i32>,
    /// Install disk (`spec.template.disk`); `None` → the Job's own default.
    pub disk: Option<&'a VMImageTemplateDisk>,
    /// CPU count (`spec.template.cpus`); `None` → Job default (2).
    pub cpus: Option<i32>,
    /// Memory in MiB (`spec.template.memoryMib`); `None` → Job default (4096).
    pub memory_mib: Option<i64>,
    /// Firmware (`spec.template.firmware`); `None` → Job default (efi).
    pub firmware: Option<&'a Firmware>,
    /// `guestId` override (`spec.template.guestId`); `None` → derived by the Job.
    pub guest_id: Option<&'a str>,
    /// Target vCenter folder path; `None` → datacenter VM-folder root.
    pub folder: Option<&'a str>,
}

/// Build the per-zone import Job manifest.
///
/// Pure, so the manifest is unit-testable without a cluster. The Job runs the
/// `banlieue` binary's own `provider vsphere image-import` subcommand with the
/// artifacts PVC mounted read-only at `/artifacts`.
pub fn build_import_job(inputs: &ImportJobInputs<'_>) -> Value {
    let ImportJobInputs {
        job_name,
        namespace,
        image,
        service_account,
        vmimage,
        provider,
        failure_domain,
        artifact,
        tolerations,
        force_upload,
        force_create,
        network,
        network_adapter,
        nic_pci_slot,
        disk,
        cpus,
        memory_mib,
        firmware,
        guest_id,
        folder,
    } = *inputs;

    let pvc = artifact
        .pvc_ref
        .as_ref()
        .map(|r| r.name.clone())
        .unwrap_or_default();
    let iso_file = artifact.file.clone().unwrap_or_default();

    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/name".to_string(), "banlieue".to_string());
    labels.insert(
        "app.kubernetes.io/component".to_string(),
        "vsphere-import".to_string(),
    );
    labels.insert("banlieue.io/vmimage".to_string(), vmimage.to_string());
    labels.insert(
        "banlieue.io/failure-domain".to_string(),
        failure_domain.to_string(),
    );

    let mut args = vec![
        "provider".to_string(),
        "vsphere".to_string(),
        "image-import".to_string(),
        "--vmimage".to_string(),
        vmimage.to_string(),
        "--provider".to_string(),
        provider.name_any(),
        // The Job runs in the build namespace; the Provider generally does not.
        "--provider-namespace".to_string(),
        provider.namespace().unwrap_or_default(),
        "--failure-domain".to_string(),
        failure_domain.to_string(),
        "--source".to_string(),
        format!("/artifacts/{iso_file}"),
    ];
    // SEC-004: verify the ISO against the published checksum before it reaches
    // any datastore.
    if let Some(checksum) = artifact.checksum.as_deref() {
        args.push("--checksum".to_string());
        args.push(checksum.to_string());
    }
    // Re-upload the ISO / recreate the template rather than no-op when present.
    if force_upload {
        args.push("--force-upload".to_string());
    }
    if force_create {
        args.push("--force-create".to_string());
    }
    if let Some(network) = network {
        args.push("--network".to_string());
        args.push(network.to_string());
    }
    if let Some(adapter) = network_adapter {
        args.push("--network-adapter".to_string());
        args.push(adapter.as_str().to_string());
    }
    if let Some(slot) = nic_pci_slot {
        args.push("--nic-pci-slot".to_string());
        args.push(slot.to_string());
    }
    if let Some(cpus) = cpus {
        args.push("--cpus".to_string());
        args.push(cpus.to_string());
    }
    if let Some(memory_mib) = memory_mib {
        args.push("--memory-mib".to_string());
        args.push(memory_mib.to_string());
    }
    if let Some(firmware) = firmware {
        args.push("--firmware".to_string());
        args.push(firmware.as_str().to_string());
    }
    if let Some(guest_id) = guest_id {
        args.push("--guest-id".to_string());
        args.push(guest_id.to_string());
    }
    if let Some(disk) = disk {
        if let Some(size) = disk.size {
            args.push("--disk-gb".to_string());
            args.push(size.to_string());
        }
        args.push("--disk-type".to_string());
        args.push(disk.provisioning.as_str().to_string());
        args.push("--disk-controller".to_string());
        args.push(disk.controller.as_str().to_string());
    }
    if let Some(folder) = folder {
        args.push("--folder".to_string());
        args.push(folder.to_string());
    }

    let mut pod_spec = json!({
        "restartPolicy": "Never",
        "containers": [{
            "name": "import",
            "image": image,
            "args": args,
            "volumeMounts": [{
                "name": "artifacts",
                "mountPath": "/artifacts",
                "readOnly": true,
            }],
        }],
        "volumes": [{
            "name": "artifacts",
            "persistentVolumeClaim": { "claimName": pvc, "readOnly": true },
        }],
    });
    if let Some(sa) = service_account {
        pod_spec["serviceAccountName"] = json!(sa);
    }
    if !tolerations.is_empty() {
        pod_spec["tolerations"] = serde_json::to_value(tolerations).unwrap_or_else(|_| json!([]));
    }

    json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": { "name": job_name, "namespace": namespace, "labels": labels },
        "spec": {
            // A half-finished upload is resumable only by starting over, and
            // retrying forever would hammer vCenter; two attempts then stop.
            "backoffLimit": 1,
            "ttlSecondsAfterFinished": 86400,
            "template": {
                "metadata": { "labels": labels },
                "spec": pod_spec,
            },
        },
    })
}

fn per_provider_failure(
    provider: &Provider,
    reason: &str,
    message: String,
) -> ImagePerProviderStatus {
    ImagePerProviderStatus {
        provider_name: provider.name_any(),
        provider_namespace: provider.namespace().unwrap_or_default(),
        ready: false,
        resolved_ref: None,
        reason: Some(reason.to_string()),
        message: Some(message),
        zones: vec![],
    }
}

fn render_resolved_ref(hits: &[(String, crate::client::Template)], template_name: &str) -> String {
    // vSphere convention: "[datacenter,...] template-name". With one DC hit
    // we render the simpler "[dc] name"; with multiple we list all the DCs.
    let dcs: Vec<&str> = hits.iter().map(|(dc, _)| dc.as_str()).collect();
    format!("[{}] {}", dcs.join(","), template_name)
}

/// Read the Provider's credentials Secret. Mirrors `provider.rs::read_credentials`
/// — kept local so the two reconcilers can evolve independently.
async fn read_credentials(
    ctx: &Context,
    namespace: &str,
    provider: &Provider,
) -> Result<Credentials> {
    let secret_name = &provider.spec.connection.credentials_ref.name;
    let api: Api<Secret> = Api::namespaced(ctx.client.clone(), namespace);
    let secret = api.get(secret_name).await.map_err(|e| {
        if let kube::Error::Api(api_err) = &e
            && api_err.code == 404
        {
            return Error::Missing("Provider.spec.connection.credentialsRef");
        }
        Error::Kube(e)
    })?;
    let data = secret.data.unwrap_or_default();
    let username = data
        .get(SECRET_KEY_USERNAME)
        .ok_or(Error::Missing("secret.data.username"))?;
    let password = data
        .get(SECRET_KEY_PASSWORD)
        .ok_or(Error::Missing("secret.data.password"))?;
    Ok(Credentials {
        username: String::from_utf8(username.0.clone())
            .map_err(|_| Error::Missing("secret.data.username (not utf-8)"))?,
        password: String::from_utf8(password.0.clone())
            .map_err(|_| Error::Missing("secret.data.password (not utf-8)"))?,
    })
}

/// Resolve the candidate datacenters for a Provider. Prefers
/// `Provider.status.failureDomains[*].attributes.raw["datacenter"]` populated
/// by the [`super::provider`] reconciler; falls back to a live `list_datacenters`
/// when the status is empty (first-touch race).
async fn dcs_from_provider_status(
    provider: &Provider,
    client: &dyn VSphereClient,
) -> Result<Vec<Datacenter>> {
    let mut from_status: Vec<String> = Vec::new();
    if let Some(status) = provider.status.as_ref() {
        for fd in &status.failure_domains {
            if let Some(dc) = fd.attributes.raw.get("datacenter")
                && !from_status.contains(dc)
            {
                from_status.push(dc.clone());
            }
        }
    }
    if from_status.is_empty() {
        // Provider hasn't been reconciled yet; do a live walk.
        return client.list_datacenters().await;
    }
    let live = client.list_datacenters().await?;
    // Cross-reference: keep only DCs that vCenter currently reports AND that
    // appear in Provider.status. Drops stale Provider.status entries.
    Ok(live
        .into_iter()
        .filter(|dc| from_status.contains(&dc.name))
        .collect())
}

/// List vsphere-class Providers in scope.
async fn list_vsphere_providers(ctx: &Context) -> Result<Vec<Provider>> {
    let api: Api<Provider> = match ctx.namespace.as_deref() {
        Some(ns) => Api::namespaced(ctx.client.clone(), ns),
        None => Api::all(ctx.client.clone()),
    };
    let list = api.list(&ListParams::default()).await?;
    Ok(list
        .into_iter()
        .filter(|p| p.spec.provider_class_ref.name == PROVIDER_CLASS_NAME)
        .collect())
}

async fn patch_vmimage_status(
    ctx: &Context,
    name: &str,
    generation: i64,
    per_provider: Vec<ImagePerProviderStatus>,
) -> Result<()> {
    let status = VMImageStatus {
        per_provider,
        // Never set by this provider — banlieue-imagebuilder is the sole
        // writer of buildArtifact (ADR-0010); omitting it here (rather than
        // writing None explicitly into the SSA-applied JSON) would be
        // equally correct since it's skip_serializing_if, but staying
        // explicit documents the field-manager split at the call site.
        build_artifact: None,
        // Likewise the aggregate Ready, which belongs to banlieue-controller
        // (ADR-0015): this provider only ever sees its own rows, so any value
        // it computed would be an answer to a different question.
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
    let params = PatchParams::apply(FIELD_MANAGER_PROVIDER_VSPHERE).force();
    api.patch_status(name, &params, &Patch::Apply(&patch))
        .await?;
    Ok(())
}

#[cfg(test)]
#[path = "vmimage_tests.rs"]
mod vmimage_tests;
