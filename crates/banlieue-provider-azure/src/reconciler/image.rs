// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VMImage` reconciler: the Compute Gallery image version a `VMImage`
//! boots from on Azure (ADR-0162 Decisions 4 and 8).
//!
//! - **`Template`**: the source's `ref` is the resource ID of a gallery image
//!   version an admin published by hand. Admin-asserted, like a Proxmox
//!   template VMID, and **regional**: one row per `Provider` with `zones`
//!   empty, which the scheduler reads as ready in every zone. No ARM call is
//!   made; a wrong ID surfaces at the VM create. banlieue never touches it
//!   (ADR-0028), and it never gets the finalizer.
//! - **`Url`**: banlieue publishes the shared `cloudImage` build into the
//!   gallery every `azure` `Provider` serving the `VMImage` declares, and
//!   the gallery follows the `VMImage` (the declared state): a missing version
//!   for the current digest is published, a version this `VMImage` published
//!   for an earlier digest is deleted once no machine references it, and
//!   every version it owns is deleted with it (finalizer
//!   [`VM_IMAGE_FINALIZER`]). The publishing and deleting is done by Jobs in
//!   the imagebuild namespace under the publisher's own identity
//!   ([`super::publish_job`]): this provider only reads versions. The
//!   decisions are pure, in [`super::gallery`].
//!
//! Other source kinds are reported unsupported rather than skipped.

use std::collections::BTreeSet;
use std::sync::Arc;

use banlieue_api::banlieue::{
    AzureConnection, AzureImageGallery, AzurePublisherIdentity, ImagePerProviderStatus,
    ImageSource, ImageSourceKind, Provider, VMImage,
};
use banlieue_api::infrastructure::AzureVirtualMachine;
use banlieue_azure::ArmApi;
use banlieue_provider_sdk::finalizer::{ensure_finalizer, remove_finalizer};
use banlieue_provider_sdk::reconciler::{
    list_providers_of_class, providers_in_scope, requeue_default, requeue_long, serves,
};
use banlieue_provider_sdk::ssa::{
    FIELD_MANAGER_PROVIDER_AZURE, apply_status, is_not_found, status_field_manager,
};
use banlieue_provider_sdk::status::{image_row, provider_rows_status};
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    ResourceExt,
    api::{Api, DeleteParams, ListParams, Patch, PatchParams},
    runtime::controller::Action,
};
use tracing::{info, warn};

use super::gallery::{
    self, JobAction, ReadyBuild, SupersededAction, VersionPath, decide_publish, decide_superseded,
};
use super::publish_job::{self, JobInputs, JobOutput, JobState, Mode};
use crate::client::azure_connection;
use crate::context::Context;
use crate::error::Result;
use crate::reconciler::provider::PROVIDER_CLASS_NAME;

/// Every ARM resource ID starts here.
const ID_PREFIX: &str = "/subscriptions/";
/// The path segment of a Compute Gallery.
const GALLERY_SEGMENT: &str = "/providers/microsoft.compute/galleries/";
/// The path segment of a gallery image definition.
const IMAGES_SEGMENT: &str = "/images/";
/// The path segment of a gallery image version.
const VERSIONS_SEGMENT: &str = "/versions/";
/// The label Kubernetes puts on a Job's pods.
const JOB_NAME_LABEL: &str = "batch.kubernetes.io/job-name";

/// Finalizer on a `VMImage` with an `azure` `Url` source: held until every
/// gallery image version it published is deleted (ADR-0162 Decision 8).
/// Distinct from the vSphere provider's `banlieue.io/vmimage`, so each
/// backend releases only its own.
pub const VM_IMAGE_FINALIZER: &str = "banlieue.io/vmimage-azure";

/// Stable `reason` strings for `ImagePerProviderStatus.reason`. A `Url`
/// source's are in [`super::gallery::reasons`].
pub mod reasons {
    /// The reference is a gallery image version ID.
    pub const RECONCILED: &str = "Reconciled";
    /// `ref` is not a gallery image version resource ID.
    pub const INVALID_REF: &str = "InvalidRef";
    /// Only `Template` and `Url` sources are supported on Azure.
    pub const UNSUPPORTED_SOURCE_KIND: &str = "UnsupportedSourceKind";
    /// The `VMImage` is being deleted; its published versions are going.
    pub const DELETING: &str = "Deleting";
}

/// The first source declared for class `azure`.
#[must_use]
pub fn find_azure_source(sources: &[ImageSource]) -> Option<&ImageSource> {
    sources
        .iter()
        .find(|s| s.provider_class == PROVIDER_CLASS_NAME)
}

/// Whether `reference` is shaped like a gallery image version resource ID:
/// `/subscriptions/<s>/resourceGroups/<rg>/providers/Microsoft.Compute/galleries/<g>/images/<i>/versions/<v>`,
/// compared case-insensitively as ARM IDs are.
#[must_use]
pub fn is_gallery_version_id(reference: &str) -> bool {
    let id = reference.trim().to_ascii_lowercase();
    let Some(gallery) = id.find(GALLERY_SEGMENT) else {
        return false;
    };
    let after = &id[gallery + GALLERY_SEGMENT.len()..];
    let Some(images) = after.find(IMAGES_SEGMENT) else {
        return false;
    };
    let after_images = &after[images + IMAGES_SEGMENT.len()..];
    let Some(versions) = after_images.find(VERSIONS_SEGMENT) else {
        return false;
    };
    id.starts_with(ID_PREFIX)
        && images > 0
        && versions > 0
        && !after_images[versions + VERSIONS_SEGMENT.len()..]
            .trim_matches('/')
            .is_empty()
}

/// The row for `provider` and a source that is not `Url`, decided from the
/// source alone.
#[must_use]
pub fn row_for(provider: &Provider, source: &ImageSource) -> ImagePerProviderStatus {
    if source.kind != ImageSourceKind::Template {
        return image_row(
            provider,
            false,
            reasons::UNSUPPORTED_SOURCE_KIND,
            Some(
                "Azure supports Template sources (a gallery image version resource ID) and Url \
                 sources (published by banlieue)"
                    .to_string(),
            ),
        );
    }
    if !is_gallery_version_id(&source.reference) {
        return image_row(
            provider,
            false,
            reasons::INVALID_REF,
            Some(format!(
                "ref {:?} is not a gallery image version resource ID",
                source.reference
            )),
        );
    }
    ImagePerProviderStatus {
        resolved_ref: Some(source.reference.trim().to_string()),
        ..image_row(provider, true, reasons::RECONCILED, None)
    }
}

/// Whether the published image definition is Trusted Launch: when the image
/// declares first-boot sealing, which needs a vTPM (ADR-0162 Decisions 3
/// and 7). A `VMImage` has no `tpmEnabled` of its own; that is the class's.
#[must_use]
pub fn trusted_launch(image: &VMImage) -> bool {
    image.spec.first_boot_sealing.is_some()
}

/// This `VMImage`'s existing row for `provider`, if any.
#[must_use]
pub fn existing_row<'a>(
    image: &'a VMImage,
    provider: &Provider,
) -> Option<&'a ImagePerProviderStatus> {
    let namespace = provider.namespace().unwrap_or_default();
    let name = provider.name_any();
    image
        .status
        .as_ref()?
        .per_provider
        .iter()
        .find(|r| r.provider_name == name && r.provider_namespace == namespace)
}

/// A not-ready row that keeps what the existing row knew about the published
/// version, so a transient gate (a rebuild in progress, a failed read) never
/// loses track of a version banlieue must later delete.
#[must_use]
pub fn holding_row(
    provider: &Provider,
    existing: Option<&ImagePerProviderStatus>,
    reason: &str,
    message: String,
) -> ImagePerProviderStatus {
    let base = image_row(provider, false, reason, Some(message));
    let Some(existing) = existing else {
        return base;
    };
    ImagePerProviderStatus {
        resolved_ref: existing
            .resolved_ref
            .clone()
            .filter(|_| existing.source_digest.is_some()),
        source_digest: existing.source_digest.clone(),
        min_os_disk_size_gib: existing.min_os_disk_size_gib,
        superseded_refs: existing.superseded_refs.clone(),
        ..base
    }
}

/// The publish settings a `Provider` must declare.
pub struct PublishTarget<'a> {
    /// The `Provider`'s `spec.connection.azure`.
    pub azure: &'a AzureConnection,
    /// Its `imageGallery`.
    pub gallery: &'a AzureImageGallery,
    /// Its `publisher`.
    pub publisher: &'a AzurePublisherIdentity,
}

/// The gallery and publisher `provider` declares.
///
/// # Errors
/// `(reason, message)` naming what is missing.
pub fn publish_target(
    provider: &Provider,
) -> std::result::Result<PublishTarget<'_>, (&'static str, String)> {
    let azure = azure_connection(&provider.spec.connection)
        .map_err(|e| (gallery::reasons::GALLERY_NOT_CONFIGURED, e.to_string()))?;
    let Some(gallery) = azure.image_gallery.as_ref() else {
        return Err((
            gallery::reasons::GALLERY_NOT_CONFIGURED,
            "Provider spec.connection.azure.imageGallery is not set; a Url source is published \
             into the admin's existing Compute Gallery named there"
                .to_string(),
        ));
    };
    let Some(publisher) = azure.publisher.as_ref() else {
        return Err((
            gallery::reasons::PUBLISHER_NOT_CONFIGURED,
            "Provider spec.connection.azure.publisher is not set; the publish Job federates to \
             that identity (ADR-0159 Decision 3)"
                .to_string(),
        ));
    };
    Ok(PublishTarget {
        azure,
        gallery,
        publisher,
    })
}

/// Reconcile one `VMImage`.
///
/// # Errors
/// [`crate::Error`] on Kubernetes API failure.
pub async fn reconcile(image: Arc<VMImage>, ctx: Arc<Context>) -> Result<Action> {
    let name = image.name_any();
    let generation = image.metadata.generation.unwrap_or(0);
    let api: Api<VMImage> = Api::all(ctx.client.clone());
    if image.metadata.deletion_timestamp.is_some() {
        return finalize(&api, &image, &ctx).await;
    }
    let Some(source) = find_azure_source(&image.spec.sources) else {
        return Ok(requeue_long());
    };

    let of_class =
        list_providers_of_class(&ctx.client, ctx.namespace.as_deref(), PROVIDER_CLASS_NAME).await?;
    let providers = providers_in_scope(of_class, ctx.provider_name.as_deref());
    if providers.is_empty() {
        info!("no azure Providers in scope; leaving status untouched");
        return Ok(requeue_long());
    }
    if source.kind == ImageSourceKind::Url {
        ensure_finalizer(&api, image.as_ref(), VM_IMAGE_FINALIZER).await?;
    }

    let mut rows: Vec<ImagePerProviderStatus> = Vec::with_capacity(providers.len());
    for provider in &providers {
        let row = match source.kind {
            ImageSourceKind::Url => url_row(&ctx, &image, provider).await,
            _ => non_url_row(&ctx, &image, provider, source).await,
        };
        rows.push(row);
    }
    let any_pending = rows
        .iter()
        .any(|r| !r.ready || !r.superseded_refs.is_empty());
    // Every Provider's row, this workload's as just decided: another
    // workload may still be deleting what it published.
    let all_rows = merge_rows(
        image
            .status
            .as_ref()
            .map(|s| s.per_provider.clone())
            .unwrap_or_default(),
        &rows,
    );
    let nothing_tracked = all_rows
        .iter()
        .all(|r| gallery::owned_refs_on_delete(r).is_empty());

    // ADR-0087 (Amendment 2026-11-01): started for one Provider, this
    // workload writes one row under that Provider's own manager.
    let namespace = providers
        .first()
        .and_then(ResourceExt::namespace)
        .unwrap_or_default();
    let field_manager = status_field_manager(
        FIELD_MANAGER_PROVIDER_AZURE,
        ctx.provider_name.as_deref(),
        &namespace,
    );
    let status = provider_rows_status(rows, generation);
    apply_status(&api, &name, &field_manager, &status, true).await?;

    // A source switched away from Url: once nothing published is left to
    // delete, the finalizer has no work to guard.
    if source.kind != ImageSourceKind::Url && nothing_tracked {
        remove_finalizer(&api, image.as_ref(), VM_IMAGE_FINALIZER).await?;
    }
    Ok(if any_pending {
        requeue_default()
    } else {
        requeue_long()
    })
}

/// A `Template` (or unsupported) source's row, still deleting what an
/// earlier `Url` source of this `VMImage` published.
async fn non_url_row(
    ctx: &Context,
    image: &VMImage,
    provider: &Provider,
    source: &ImageSource,
) -> ImagePerProviderStatus {
    let row = row_for(provider, source);
    let leftover = gallery::superseded_refs(existing_row(image, provider), None);
    if leftover.is_empty() {
        return row;
    }
    let remaining = match publish_target(provider) {
        Ok(target) => match ctx.arm.build(&provider.spec.connection).await {
            Ok(arm) => delete_unwanted(ctx, arm.as_ref(), image, provider, &target, leftover).await,
            Err(e) => {
                warn!(error = %e, "cannot reach Azure to delete superseded versions");
                leftover
            }
        },
        Err(_) => leftover,
    };
    ImagePerProviderStatus {
        superseded_refs: remaining,
        ..row
    }
}

/// A `Url` source's row for `provider`: the declared-state loop.
async fn url_row(ctx: &Context, image: &VMImage, provider: &Provider) -> ImagePerProviderStatus {
    let existing = existing_row(image, provider);
    let target = match publish_target(provider) {
        Ok(t) => t,
        Err((reason, message)) => return holding_row(provider, existing, reason, message),
    };
    let artifact = image
        .status
        .as_ref()
        .and_then(|s| s.build_artifact.as_ref());
    let build = match gallery::gate_build(artifact) {
        Ok(b) => b,
        Err((reason, message)) => return holding_row(provider, existing, reason, message),
    };
    let arm = match ctx.arm.build(&provider.spec.connection).await {
        Ok(a) => a,
        Err(e) => {
            return holding_row(
                provider,
                existing,
                gallery::reasons::LOOKUP_FAILED,
                e.to_string(),
            );
        }
    };
    let definition =
        banlieue_azure::image_definition_name(&image.name_any(), trusted_launch(image));
    let version = banlieue_azure::gallery_version_name(&build.hex);
    let read = match arm
        .get_gallery_image_version(
            &target.gallery.resource_group,
            &target.gallery.name,
            &definition,
            &version,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return holding_row(
                provider,
                existing,
                gallery::reasons::LOOKUP_FAILED,
                format!("reading gallery image version {definition}/{version}: {e}"),
            );
        }
    };
    let owner_uid = image.uid().unwrap_or_default();
    let state = gallery::classify_version(read.as_ref(), &owner_uid, &build.hex);

    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), &ctx.publish.build_namespace);
    let job_name = publish_job::job_name(Mode::Publish, provider, &definition, &version);
    let job = match jobs.get_opt(&job_name).await {
        Ok(j) => j,
        Err(e) => {
            return holding_row(
                provider,
                existing,
                gallery::reasons::PUBLISHING,
                format!("reading publish Job {job_name}: {e}"),
            );
        }
    };
    let job_state = publish_job::job_state(job.as_ref(), k8s_openapi::jiff::Timestamp::now());
    let decision = decide_publish(&state, &job_state);

    let digest = build.digest();
    let inputs = JobInputs {
        job_name: &job_name,
        namespace: &ctx.publish.build_namespace,
        image: &ctx.publish.image,
        service_account: &ctx.publish.service_account,
        mode: Mode::Publish,
        vmimage: image,
        provider,
        azure: target.azure,
        gallery: target.gallery,
        publisher: target.publisher,
        definition: &definition,
        version: &version,
        artifact: Some(build.artifact),
        checksum: Some(&digest),
        trusted_launch: trusted_launch(image),
        tolerations: &ctx.publish.tolerations,
    };
    let mut message = decision.message.clone();
    if let Err(e) = apply_job_action(&jobs, decision.action, &inputs).await {
        message = Some(format!("publish Job {job_name}: {e}"));
    }
    let output = match job_state {
        JobState::Succeeded | JobState::Failed { .. } => job_output(ctx, &job_name).await,
        JobState::Absent | JobState::Running => None,
    };
    if let (Some(JobOutput::Error { code, message: m }), false) = (&output, decision.ready) {
        message = Some(format!(
            "{}; last error {code}: {m}",
            message.unwrap_or_default()
        ));
    }

    let current = Some((definition.as_str(), version.as_str()));
    let unwanted = gallery::superseded_refs(existing, current);
    let remaining = delete_unwanted(ctx, arm.as_ref(), image, provider, &target, unwanted).await;
    published_row(&PublishedRow {
        provider,
        existing,
        build: &build,
        state: &state,
        decision: &decision,
        output: output.as_ref(),
        message,
        superseded: remaining,
    })
}

/// What [`published_row`] assembles a `Url` row from.
pub struct PublishedRow<'a> {
    /// The `Provider`.
    pub provider: &'a Provider,
    /// The row as it stood.
    pub existing: Option<&'a ImagePerProviderStatus>,
    /// The build being published.
    pub build: &'a ReadyBuild<'a>,
    /// The version read.
    pub state: &'a gallery::VersionState,
    /// The decision taken.
    pub decision: &'a gallery::PublishDecision,
    /// The publish Job's output, when it has finished.
    pub output: Option<&'a JobOutput>,
    /// The row's message.
    pub message: Option<String>,
    /// The superseded versions still tracked.
    pub superseded: Vec<String>,
}

/// Assemble the `Url` row. A ready row carries the version ID, the digest,
/// and the minimum OS disk size from the publish Job's output, or from the
/// existing row for the same digest once the Job has been cleaned up. Pure.
#[must_use]
pub fn published_row(parts: &PublishedRow<'_>) -> ImagePerProviderStatus {
    let row = image_row(
        parts.provider,
        parts.decision.ready,
        parts.decision.reason,
        parts.message.clone(),
    );
    let gallery::VersionState::Published { id } = parts.state else {
        return ImagePerProviderStatus {
            superseded_refs: parts.superseded.clone(),
            ..row
        };
    };
    let digest = parts.build.digest();
    let from_job = match parts.output {
        Some(JobOutput::Published {
            version_id,
            min_os_disk_size_gib,
            ..
        }) if gallery::same_id(version_id, id) => *min_os_disk_size_gib,
        _ => None,
    };
    let from_row = parts
        .existing
        .filter(|r| r.source_digest.as_deref() == Some(digest.as_str()))
        .and_then(|r| r.min_os_disk_size_gib);
    ImagePerProviderStatus {
        resolved_ref: Some(id.clone()),
        source_digest: Some(digest),
        min_os_disk_size_gib: from_job.or(from_row),
        superseded_refs: parts.superseded.clone(),
        ..row
    }
}

/// Carry out a Job action. Server-side apply creates (or adopts) the Job; a
/// delete is in the background and a 404 is success.
async fn apply_job_action(
    jobs: &Api<Job>,
    action: JobAction,
    inputs: &JobInputs<'_>,
) -> Result<()> {
    match action {
        JobAction::None => Ok(()),
        JobAction::Create => {
            let manifest = publish_job::build_job(inputs);
            jobs.patch(
                inputs.job_name,
                &PatchParams::apply(FIELD_MANAGER_PROVIDER_AZURE).force(),
                &Patch::Apply(&manifest),
            )
            .await?;
            info!(
                job = inputs.job_name,
                mode = inputs.mode.as_str(),
                "image Job applied"
            );
            Ok(())
        }
        JobAction::Delete => {
            match jobs
                .delete(inputs.job_name, &DeleteParams::background())
                .await
            {
                Ok(_) => {}
                Err(e) if is_not_found(&e) => {}
                Err(e) => return Err(e.into()),
            }
            info!(job = inputs.job_name, "image Job deleted, to be recreated");
            Ok(())
        }
    }
}

/// The termination message of a finished Job's pod, parsed. `None` when the
/// pod is gone or its message is not one of the Job's JSON lines.
async fn job_output(ctx: &Context, job_name: &str) -> Option<JobOutput> {
    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), &ctx.publish.build_namespace);
    let listed = pods
        .list(&ListParams::default().labels(&format!("{JOB_NAME_LABEL}={job_name}")))
        .await
        .map_err(|e| warn!(job = job_name, error = %e, "cannot read the Job's pods"))
        .ok()?;
    listed.items.iter().rev().find_map(|pod| {
        pod.status
            .as_ref()?
            .container_statuses
            .as_ref()?
            .iter()
            .find_map(|c| {
                let message = c.state.as_ref()?.terminated.as_ref()?.message.as_deref()?;
                publish_job::parse_output(message)
            })
    })
}

/// The image version IDs every `AzureVirtualMachine` this provider can list
/// boots from, lowercased. `None` when the list fails: the caller then
/// deletes nothing.
async fn referenced_versions(ctx: &Context, provider: &Provider) -> Option<BTreeSet<String>> {
    let home = provider.namespace().unwrap_or_default();
    let api: Api<AzureVirtualMachine> = match ctx.scope() {
        Some(scope) => scope.api(&ctx.client),
        None => Api::namespaced(ctx.client.clone(), &home),
    };
    let listed = api
        .list(&ListParams::default())
        .await
        .map_err(|e| warn!(error = %e, "cannot list AzureVirtualMachines; deleting nothing"))
        .ok()?;
    Some(
        listed
            .items
            .iter()
            .map(|m| m.spec.image_version_id.trim().to_ascii_lowercase())
            .collect(),
    )
}

/// Run the delete half of the loop over `ids` (versions this `VMImage`
/// published and no longer wants) and return the ones still to track.
/// Never fails: anything it cannot decide is kept for the next pass.
async fn delete_unwanted(
    ctx: &Context,
    arm: &dyn ArmApi,
    image: &VMImage,
    provider: &Provider,
    target: &PublishTarget<'_>,
    ids: Vec<String>,
) -> Vec<String> {
    if ids.is_empty() {
        return ids;
    }
    let Some(referenced) = referenced_versions(ctx, provider).await else {
        return ids;
    };
    let owner_uid = image.uid().unwrap_or_default();
    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), &ctx.publish.build_namespace);
    let mut remaining = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(path) = VersionPath::parse(&id) else {
            warn!(id, "not a gallery image version ID; no longer tracked");
            continue;
        };
        let read = match arm
            .get_gallery_image_version(
                &path.resource_group,
                &path.gallery,
                &path.image,
                &path.version,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(id, error = %e, "cannot read a superseded version; kept");
                remaining.push(id);
                continue;
            }
        };
        let job_name = publish_job::job_name(Mode::Delete, provider, &path.image, &path.version);
        let job = match jobs.get_opt(&job_name).await {
            Ok(j) => j,
            Err(e) => {
                warn!(job = job_name, error = %e, "cannot read a delete Job; kept");
                remaining.push(id);
                continue;
            }
        };
        let state = publish_job::job_state(job.as_ref(), k8s_openapi::jiff::Timestamp::now());
        let is_referenced = referenced.contains(&id.trim().to_ascii_lowercase());
        let job_action = match decide_superseded(read.as_ref(), &owner_uid, is_referenced, &state) {
            SupersededAction::Drop => {
                info!(id, "superseded version gone or not ours; no longer tracked");
                continue;
            }
            SupersededAction::Keep => JobAction::None,
            SupersededAction::CreateDeleteJob => JobAction::Create,
            SupersededAction::RetryDeleteJob => JobAction::Delete,
        };
        let gallery = AzureImageGallery {
            resource_group: path.resource_group.clone(),
            name: path.gallery.clone(),
        };
        let inputs = JobInputs {
            job_name: &job_name,
            namespace: &ctx.publish.build_namespace,
            image: &ctx.publish.image,
            service_account: &ctx.publish.service_account,
            mode: Mode::Delete,
            vmimage: image,
            provider,
            azure: target.azure,
            gallery: &gallery,
            publisher: target.publisher,
            definition: &path.image,
            version: &path.version,
            artifact: None,
            checksum: None,
            trusted_launch: false,
            tolerations: &ctx.publish.tolerations,
        };
        if let Err(e) = apply_job_action(&jobs, job_action, &inputs).await {
            warn!(job = job_name, error = %e, "delete Job action failed; retried next pass");
        }
        remaining.push(id);
    }
    remaining
}

/// `rows` with each of `replaced`'s rows swapped in for the row of the same
/// `Provider`. Pure.
#[must_use]
pub fn merge_rows(
    rows: Vec<ImagePerProviderStatus>,
    replaced: &[ImagePerProviderStatus],
) -> Vec<ImagePerProviderStatus> {
    rows.into_iter()
        .map(|r| {
            replaced
                .iter()
                .find(|m| {
                    m.provider_name == r.provider_name
                        && m.provider_namespace == r.provider_namespace
                })
                .cloned()
                .unwrap_or(r)
        })
        .collect()
}

/// Deletion path (ADR-0162 Decisions 5 and 8): delete every version this
/// `VMImage` published into this workload's `Provider`s once no machine
/// references it, then drop the finalizer when no `Provider` that still
/// exists has one left.
///
/// The rule for not blocking forever, mirroring the vSphere provider's
/// `finalizer_can_go` (ADR-0028): a row whose `Provider` is gone does not
/// hold the delete, and neither does a `Provider` that no longer declares a
/// gallery and publisher (nothing could delete its versions); both are
/// logged, and the versions stay in the gallery with their ownership tags. A
/// version a machine still boots from does hold it, until the machine goes.
async fn finalize(api: &Api<VMImage>, image: &VMImage, ctx: &Context) -> Result<Action> {
    if !image.finalizers().iter().any(|f| f == VM_IMAGE_FINALIZER) {
        return Ok(requeue_long());
    }
    info!("finalizing VMImage: deleting its published gallery image versions");
    let scope = ctx.provider_name.as_deref();
    let rows = image
        .status
        .as_ref()
        .map(|s| s.per_provider.clone())
        .unwrap_or_default();
    let mut mine = Vec::new();
    for row in rows.iter().filter(|r| serves(scope, &r.provider_name)) {
        let ids = gallery::owned_refs_on_delete(row);
        if ids.is_empty() {
            continue;
        }
        let providers: Api<Provider> = Api::namespaced(ctx.client.clone(), &row.provider_namespace);
        let Some(provider) = providers.get_opt(&row.provider_name).await? else {
            warn!(provider = %row.provider_name, "Provider gone; its published versions stay in the gallery");
            continue;
        };
        let remaining = match publish_target(&provider) {
            Ok(target) => {
                let arm = ctx.arm.build(&provider.spec.connection).await?;
                delete_unwanted(ctx, arm.as_ref(), image, &provider, &target, ids).await
            }
            Err((_, why)) => {
                warn!(provider = %row.provider_name, why, "cannot delete published versions; leaving them in the gallery");
                Vec::new()
            }
        };
        mine.push(ImagePerProviderStatus {
            superseded_refs: remaining,
            ..image_row(
                &provider,
                false,
                reasons::DELETING,
                Some("deleting the gallery image versions this VMImage published".to_string()),
            )
        });
    }
    if let Some(first) = mine.first() {
        let field_manager = status_field_manager(
            FIELD_MANAGER_PROVIDER_AZURE,
            scope,
            &first.provider_namespace,
        );
        let generation = image.metadata.generation.unwrap_or(0);
        let status = provider_rows_status(mine.clone(), generation);
        apply_status(api, &image.name_any(), &field_manager, &status, true).await?;
    }
    let after = merge_rows(rows, &mine);
    let live: BTreeSet<(String, String)> =
        list_providers_of_class(&ctx.client, ctx.namespace.as_deref(), PROVIDER_CLASS_NAME)
            .await?
            .iter()
            .map(|p| (p.namespace().unwrap_or_default(), p.name_any()))
            .collect();
    if gallery::finalizer_can_go(&after, &live) {
        remove_finalizer(api, image, VM_IMAGE_FINALIZER).await?;
        return Ok(requeue_long());
    }
    info!("published versions remain; finalizer kept");
    Ok(requeue_default())
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod image_tests;
