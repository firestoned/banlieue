// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VMImage`: this host's row in `status.perProvider[]`.
//!
//! Two source kinds, one cache layout, `<storage class dir>/images/<name>`
//! (ADR-0064 Decision 4):
//!
//! - **`BackingFile`**: an image an admin has already placed in a storage
//!   class's cache. Ready when at least one class holds it.
//! - **`Url`**: the imagebuilder's build, pushed to a registry
//!   (`status.buildArtifact.ociArtifact`). The host pulls it **by digest**,
//!   and only from the repository its own config names, in a transient
//!   unit (`banlieue-ch-import@<vmimage uid>.service`) that runs
//!   [`crate::import`]; the reconciler never touches the bytes. The cache
//!   file is named by digest, so identical images share it. Ready once every
//!   storage class holds it, since a machine may pick any of them.
//!
//! Either way `resolvedRef` is the file name the controller copies into
//! `CloudHypervisorMachine.spec.bootSource.image`.
//!
//! The row never carries a host path (ADR-0062 Decision 4): it names
//! storage classes, which are already published on the failure domain.
//! `perProvider` is merge-keyed (ADR-0015), so applying our one row leaves
//! every other provider's untouched.

use crate::error::{Error, Result};
use crate::host_config::HostConfig;
use crate::plan::{IMAGES_DIR, require_image_name, require_uuid};
use crate::reconciler::{Context, is_ours};
use crate::systemd::{EnvFile, UnitStart, UnitState};
use banlieue_api::banlieue::{
    BuildArtifactStatus, ImagePerProviderStatus, ImageSource, ImageSourceKind, OciArtifactPhase,
    VMImage,
};
use banlieue_api::infrastructure::CloudHypervisorMachine;
use banlieue_oci::Reference;
use banlieue_oci::reference::{SHA256_PREFIX, require_digest};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::{Resource, ResourceExt};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use tracing::{info, warn};

/// `ImageSource.providerClass` this provider serves.
pub const PROVIDER_CLASS: &str = "cloud-hypervisor";

/// Stable `reason` strings on this provider's row.
pub mod reasons {
    /// Held by at least one storage class on this host.
    pub const RECONCILED: &str = "Reconciled";
    /// No storage class on this host holds the file.
    pub const IMAGE_NOT_FOUND: &str = "ImageNotFound";
    /// The reference is not a plain file name.
    pub const INVALID_REFERENCE: &str = "InvalidReference";
    /// Neither `BackingFile` nor `Url`.
    pub const UNSUPPORTED_SOURCE_KIND: &str = "UnsupportedSourceKind";
    /// A `Url` source, and this host has no `[registry]`.
    pub const REGISTRY_NOT_CONFIGURED: &str = "RegistryNotConfigured";
    /// The build has not been pushed yet.
    pub const AWAITING_ARTIFACT: &str = "AwaitingArtifact";
    /// The pushed reference is not a digest in this host's repository.
    pub const FOREIGN_REFERENCE: &str = "ForeignReference";
    /// The import unit is pulling or copying.
    pub const IMPORTING: &str = "Importing";
    /// The import unit failed.
    pub const IMPORT_FAILED: &str = "ImportFailed";
    /// The `VMImage` is being deleted and this host has let go of it: its
    /// cache file is removed, or kept only because something else here
    /// still uses it. `banlieue-controller` waits for this row before
    /// releasing the image's finalizer (ADR-0064 Decision 4).
    pub const RELEASED: &str = "Released";
}

/// Prefix and suffix of a pulled cache file: `sha256-<hex>.raw`.
const PULLED_PREFIX: &str = "sha256-";
/// Hex digits in a sha256 digest.
const SHA256_HEX_LEN: usize = 64;

/// Prefix of an import unit's name; the VMImage uid follows.
const IMPORT_TEMPLATE: &str = "banlieue-ch-import";
/// The import template's `EnvironmentFile=` is
/// `<state_root>/units/import-%i.env`.
const IMPORT_ENV_PREFIX: &str = "import-";
/// Extension of a pulled cache file.
const RAW_EXTENSION: &str = ".raw";

/// The first source for this provider class, of any kind.
#[must_use]
pub fn find_source(sources: &[ImageSource]) -> Option<&ImageSource> {
    sources.iter().find(|s| s.provider_class == PROVIDER_CLASS)
}

/// The file name a `BackingFile` reference names: its last path component,
/// so `/srv/x/images/kairos.raw` and `kairos.raw` both mean `kairos.raw`.
#[must_use]
pub fn image_file_name(reference: &str) -> &str {
    reference.rsplit('/').next().unwrap_or(reference)
}

/// Storage classes on this host whose image cache holds `file`, sorted.
#[must_use]
pub fn classes_holding(config: &HostConfig, file: &str) -> Vec<String> {
    config
        .storage_classes
        .iter()
        .filter(|(_, dir)| dir.join(IMAGES_DIR).join(file).is_file())
        .map(|(name, _)| name.clone())
        .collect()
}

/// This host's row for `source`, given the storage classes holding it.
#[must_use]
pub fn compute_row(
    config: &HostConfig,
    source: &ImageSource,
    holding: &[String],
) -> ImagePerProviderStatus {
    let row = |ready: bool, reason: &str, message: String, resolved: Option<String>| {
        status_row(config, ready, reason, message, resolved)
    };
    if source.kind != ImageSourceKind::BackingFile {
        return row(
            false,
            reasons::UNSUPPORTED_SOURCE_KIND,
            format!("{:?} sources are not served by this provider", source.kind),
            None,
        );
    }
    let file = image_file_name(&source.reference);
    if require_image_name(file).is_err() {
        return row(
            false,
            reasons::INVALID_REFERENCE,
            format!("{file:?} is not a plain file name"),
            None,
        );
    }
    if holding.is_empty() {
        return row(
            false,
            reasons::IMAGE_NOT_FOUND,
            format!("{file} is in no storage class's image cache on this host"),
            None,
        );
    }
    row(
        true,
        reasons::RECONCILED,
        format!("held by storage classes: {}", holding.join(", ")),
        Some(file.to_string()),
    )
}

/// This host's row, before or after an attempt, as one closure's worth of
/// fields.
fn status_row(
    config: &HostConfig,
    ready: bool,
    reason: &str,
    message: String,
    resolved_ref: Option<String>,
) -> ImagePerProviderStatus {
    ImagePerProviderStatus {
        provider_name: config.provider.name.clone(),
        provider_namespace: config.provider.namespace.clone(),
        ready,
        resolved_ref,
        reason: Some(reason.to_string()),
        message: Some(message),
        zones: Vec::new(),
    }
}

/// The cache file for a digest: `sha256-<hex>.raw`. No `:` in a file name.
///
/// # Errors
/// The digest is not `sha256:<64 hex>`.
pub fn cached_image_name(digest: &str) -> std::result::Result<String, String> {
    require_digest(digest).map_err(|e| e.to_string())?;
    let hex = digest.trim_start_matches(SHA256_PREFIX);
    Ok(format!("sha256-{hex}{RAW_EXTENSION}"))
}

/// What to pull for a `Url` source, and the file it lands in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullTarget {
    /// `registry/repository@sha256:<hex>`.
    pub reference: String,
    /// The cache file name, from [`cached_image_name`].
    pub file: String,
}

/// Decide what to pull, or why nothing can be yet: `(reason, message)`.
///
/// The reference must be a digest in the repository this host's config
/// names. Anything else is refused: `VMImage` status is written in the
/// cluster, and the host owner, not the cluster, chooses where its images
/// come from.
///
/// # Errors
/// `(reason, message)` for the row when there is nothing to pull.
pub fn pull_target(
    config: &HostConfig,
    artifact: Option<&BuildArtifactStatus>,
) -> std::result::Result<PullTarget, (&'static str, String)> {
    if config.registry.is_none() {
        return Err((
            reasons::REGISTRY_NOT_CONFIGURED,
            "Url sources need a [registry] section in this host's config (ADR-0064)".into(),
        ));
    }
    let pushed = artifact
        .and_then(|a| a.oci_artifact.as_ref())
        .filter(|o| o.phase == OciArtifactPhase::Ready)
        .and_then(|o| o.reference.as_deref());
    let Some(pushed) = pushed else {
        return Err((
            reasons::AWAITING_ARTIFACT,
            "waiting for the build to be pushed to the registry".into(),
        ));
    };
    check_reference(config, pushed)
}

/// Check `pushed` is a digest in this host's registry repository, and name
/// its cache file. Also run by the import itself, which trusts nothing on
/// its command line that the host config does not allow.
///
/// # Errors
/// `(reason, message)` saying why the reference is refused.
pub fn check_reference(
    config: &HostConfig,
    pushed: &str,
) -> std::result::Result<PullTarget, (&'static str, String)> {
    let Some(registry) = config.registry.as_ref() else {
        return Err((
            reasons::REGISTRY_NOT_CONFIGURED,
            "Url sources need a [registry] section in this host's config (ADR-0064)".into(),
        ));
    };
    let foreign = |why: &str| (reasons::FOREIGN_REFERENCE, format!("{pushed}: {why}"));
    let r = Reference::parse(pushed).map_err(|e| foreign(&e.to_string()))?;
    let Some(digest) = r.digest.as_deref() else {
        return Err(foreign("not addressed by digest"));
    };
    if r.tag.is_some() {
        return Err(foreign("not addressed by digest alone"));
    }
    if format!("{}/{}", r.registry, r.repository) != registry.repository {
        return Err(foreign("not in this host's registry repository"));
    }
    let file = cached_image_name(digest).map_err(|e| foreign(&e))?;
    Ok(PullTarget {
        reference: r.to_string(),
        file,
    })
}

/// The import unit, as far as the reconciler can see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportUnit {
    /// Not loaded: never started, or finished and collected.
    Absent,
    /// Pulling or copying.
    Running,
    /// Failed, with systemd's account of why.
    Failed(String),
}

/// This host's row for a `Url` source with a pull target.
#[must_use]
pub fn import_row(
    config: &HostConfig,
    target: &PullTarget,
    holding: &[String],
    unit: &ImportUnit,
) -> ImagePerProviderStatus {
    if holding.len() == config.storage_classes.len() {
        return status_row(
            config,
            true,
            reasons::RECONCILED,
            format!("held by storage classes: {}", holding.join(", ")),
            Some(target.file.clone()),
        );
    }
    match unit {
        ImportUnit::Failed(why) => status_row(
            config,
            false,
            reasons::IMPORT_FAILED,
            format!("importing {}: {why}", target.reference),
            None,
        ),
        ImportUnit::Absent | ImportUnit::Running => status_row(
            config,
            false,
            reasons::IMPORTING,
            format!("importing {}", target.reference),
            None,
        ),
    }
}

/// `banlieue-ch-import@<vmimage uid>.service`.
///
/// # Errors
/// The uid is not a lowercase UUID, which the polkit rule would refuse.
pub fn import_unit_name(vmimage_uid: &str) -> std::result::Result<String, String> {
    require_uuid(vmimage_uid).map_err(|e| e.to_string())?;
    Ok(crate::plan::instance_unit(IMPORT_TEMPLATE, vmimage_uid))
}

/// The import unit: an instance of `banlieue-ch-import@.service`, named by
/// the `VMImage`'s UID, which runs this binary's `import` subcommand as the
/// provider's own user, writing only the image caches. The reference and
/// cache file go in its environment file; the import re-checks both against
/// the host config.
///
/// # Errors
/// An invalid `VMImage` uid.
pub fn import_unit(
    config: &HostConfig,
    vmimage_uid: &str,
    target: &PullTarget,
) -> std::result::Result<UnitStart, String> {
    let name = import_unit_name(vmimage_uid)?;
    Ok(UnitStart {
        name,
        memory_max: None,
        environment: Some(EnvFile {
            path: import_env_file(config, vmimage_uid),
            vars: vec![
                ("REFERENCE".into(), target.reference.clone()),
                ("FILE".into(), target.file.clone()),
            ],
        }),
    })
}

/// The import template's environment file for `vmimage_uid`. Removed once
/// the image is in every cache, or released.
#[must_use]
pub fn import_env_file(config: &HostConfig, vmimage_uid: &str) -> PathBuf {
    config
        .paths
        .state_root
        .join(crate::plan::UNITS_ENV_DIR)
        .join(format!("{IMPORT_ENV_PREFIX}{vmimage_uid}.env"))
}

/// Remove `path`; absent is success.
fn remove_env_file(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Every storage class's image cache, in class order.
#[must_use]
pub fn image_dirs(config: &HostConfig) -> Vec<PathBuf> {
    config
        .storage_classes
        .values()
        .map(|dir| dir.join(IMAGES_DIR))
        .collect()
}

/// Whether `name` is a file the import put in the cache (`sha256-<hex>.raw`),
/// as opposed to one an admin placed for a `BackingFile` source. Only these
/// are ever evicted.
#[must_use]
pub fn is_pulled_cache_name(name: &str) -> bool {
    name.strip_prefix(PULLED_PREFIX)
        .and_then(|rest| rest.strip_suffix(RAW_EXTENSION))
        .is_some_and(|hex| {
            hex.len() == SHA256_HEX_LEN
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// Cache files something on this host still needs: this host's
/// `resolvedRef` on every `VMImage` not being deleted, and every machine's
/// boot image. Other hosts' rows do not count.
#[must_use]
pub fn referenced_files<'a>(
    config: &HostConfig,
    images: &[VMImage],
    machine_images: impl IntoIterator<Item = &'a str>,
) -> BTreeSet<String> {
    let mut referenced: BTreeSet<String> = machine_images.into_iter().map(str::to_string).collect();
    for image in images
        .iter()
        .filter(|i| i.meta().deletion_timestamp.is_none())
    {
        let ours = image
            .status
            .as_ref()
            .into_iter()
            .flat_map(|s| &s.per_provider)
            .filter(|r| {
                r.provider_name == config.provider.name
                    && r.provider_namespace == config.provider.namespace
            });
        referenced.extend(ours.filter_map(|r| r.resolved_ref.clone()));
    }
    referenced
}

/// Pulled cache files to delete: unreferenced ones beyond the newest
/// `keep`, oldest last. Admin-placed files and referenced ones are never
/// candidates.
#[must_use]
pub fn eviction_candidates(
    entries: &[(String, SystemTime)],
    referenced: &BTreeSet<String>,
    keep: usize,
) -> Vec<String> {
    let mut unreferenced: Vec<&(String, SystemTime)> = entries
        .iter()
        .filter(|(name, _)| is_pulled_cache_name(name) && !referenced.contains(name))
        .collect();
    unreferenced.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    unreferenced
        .into_iter()
        .skip(keep)
        .map(|(name, _)| name.clone())
        .collect()
}

/// This host's row once it has let go of a `VMImage` being deleted.
#[must_use]
pub fn release_row(config: &HostConfig, message: String) -> ImagePerProviderStatus {
    status_row(config, false, reasons::RELEASED, message, None)
}

/// Every pulled file in any storage class's cache, with its newest mtime.
fn cache_entries(config: &HostConfig) -> Vec<(String, SystemTime)> {
    let mut newest: BTreeMap<String, SystemTime> = BTreeMap::new();
    for dir in image_dirs(config) {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_pulled_cache_name(&name) {
                continue;
            }
            let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            let slot = newest.entry(name).or_insert(modified);
            *slot = (*slot).max(modified);
        }
    }
    newest.into_iter().collect()
}

/// Remove `file` from every storage class's cache. A reflinked or copied
/// OS disk is its own file, so running guests are unaffected.
fn remove_from_caches(config: &HostConfig, file: &str) -> Result<()> {
    for dir in image_dirs(config) {
        match std::fs::remove_file(dir.join(file)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(())
}

/// What this host's machines and the cluster's `VMImage`s still reference.
async fn live_references(ctx: &Context) -> Result<BTreeSet<String>> {
    let images = Api::<VMImage>::all(ctx.client.clone())
        .list(&ListParams::default())
        .await?
        .items;
    let machines = Api::<CloudHypervisorMachine>::namespaced(
        ctx.client.clone(),
        &ctx.config.provider.namespace,
    )
    .list(&ListParams::default())
    .await?
    .items;
    let boot_images = machines
        .iter()
        .filter(|m| is_ours(m, &ctx.config))
        .map(|m| m.spec.boot_source.image.as_str());
    Ok(referenced_files(&ctx.config, &images, boot_images))
}

/// Delete unreferenced pulled images beyond the configured number to keep.
async fn evict_unreferenced(ctx: &Context) -> Result<()> {
    let Some(registry) = ctx.config.registry.as_ref() else {
        return Ok(());
    };
    let referenced = live_references(ctx).await?;
    let keep = usize::try_from(registry.keep_unreferenced).unwrap_or(usize::MAX);
    for file in eviction_candidates(&cache_entries(&ctx.config), &referenced, keep) {
        remove_from_caches(&ctx.config, &file)?;
        info!(%file, "evicted an unreferenced image from the cache");
    }
    Ok(())
}

/// A `Url` image is being deleted: stop any import, remove its cache file
/// unless something else here still uses it, and say so.
async fn release(image: &VMImage, ctx: &Context) -> Result<ImagePerProviderStatus> {
    let config = &ctx.config;
    let uid = image.uid().unwrap_or_default();
    if let Ok(unit) = import_unit_name(&uid)
        && ctx.host.unit_state(&unit).await?.is_some()
    {
        ctx.host.stop_unit(&unit).await?;
    }
    if import_unit_name(&uid).is_ok() {
        remove_env_file(&import_env_file(config, &uid))?;
    }
    let ours = image
        .status
        .as_ref()
        .into_iter()
        .flat_map(|s| &s.per_provider)
        .find(|r| {
            r.provider_name == config.provider.name
                && r.provider_namespace == config.provider.namespace
        })
        .and_then(|r| r.resolved_ref.clone());
    let artifact = image
        .status
        .as_ref()
        .and_then(|s| s.build_artifact.as_ref());
    let file = ours.or_else(|| pull_target(config, artifact).ok().map(|t| t.file));
    let Some(file) = file.filter(|f| is_pulled_cache_name(f)) else {
        return Ok(release_row(config, "nothing cached on this host".into()));
    };
    if live_references(ctx).await?.contains(&file) {
        return Ok(release_row(
            config,
            format!("{file} kept: another image or a machine on this host uses it"),
        ));
    }
    remove_from_caches(config, &file)?;
    info!(vmimage = %image.name_any(), %file, "released and evicted");
    Ok(release_row(
        config,
        format!("{file} removed from this host"),
    ))
}

/// Reconcile one `VMImage`: publish this host's row if it has a source for
/// this provider class. Requeued while not ready, since an admin copying
/// the file in raises no Kubernetes event.
///
/// # Errors
/// Kubernetes API errors from the status patch.
pub async fn reconcile(image: Arc<VMImage>, ctx: Arc<Context>) -> Result<Action> {
    let Some(source) = find_source(&image.spec.sources) else {
        return Ok(Action::await_change());
    };
    let name = image.name_any();
    if image.meta().deletion_timestamp.is_some() {
        // Admin-placed BackingFile images are the admin's to remove.
        if source.kind != ImageSourceKind::Url {
            return Ok(Action::await_change());
        }
        let row = release(&image, &ctx).await?;
        patch_row(&ctx, &name, &row).await?;
        return Ok(Action::await_change());
    }
    let row = if source.kind == ImageSourceKind::Url {
        url_row(&image, &ctx).await?
    } else {
        let holding = classes_holding(&ctx.config, image_file_name(&source.reference));
        compute_row(&ctx.config, source, &holding)
    };
    let ready = row.ready;
    patch_row(&ctx, &name, &row).await?;
    // A newly ready pull may have superseded an older build's file.
    if ready && source.kind == ImageSourceKind::Url {
        evict_unreferenced(&ctx).await?;
    }
    info!(vmimage = %name, ready, reason = ?row.reason, "VMImage row published");
    Ok(if ready {
        requeue_long()
    } else {
        requeue_default()
    })
}

/// A `Url` source's row: decide what to pull, then drive the import unit.
async fn url_row(image: &VMImage, ctx: &Context) -> Result<ImagePerProviderStatus> {
    let config = &ctx.config;
    let artifact = image
        .status
        .as_ref()
        .and_then(|s| s.build_artifact.as_ref());
    let target = match pull_target(config, artifact) {
        Ok(t) => t,
        Err((reason, message)) => return Ok(status_row(config, false, reason, message, None)),
    };
    let holding = classes_holding(config, &target.file);
    let uid = image.uid().unwrap_or_default();
    let unit_name = import_unit_name(&uid).map_err(Error::Import)?;

    let unit = match ctx.host.unit_state(&unit_name).await? {
        None => ImportUnit::Absent,
        Some(UnitState::Failed) => {
            let why = ctx
                .host
                .unit_failure(&unit_name)
                .await?
                .unwrap_or_else(|| "failed".to_string());
            // Report it, and free the name: the next reconcile retries.
            ctx.host.stop_unit(&unit_name).await?;
            ImportUnit::Failed(why)
        }
        Some(_) => ImportUnit::Running,
    };
    let done = holding.len() == config.storage_classes.len();
    if done && unit == ImportUnit::Absent {
        remove_env_file(&import_env_file(config, &uid))?;
    }
    if unit == ImportUnit::Absent && !done {
        let spec = import_unit(config, &uid, &target).map_err(Error::Import)?;
        ctx.host.start_unit(&spec).await?;
        info!(vmimage = %image.name_any(), unit = %unit_name, reference = %target.reference, "import started");
    }
    Ok(import_row(config, &target, &holding, &unit))
}

async fn patch_row(ctx: &Context, name: &str, row: &ImagePerProviderStatus) -> Result<()> {
    // Only our row: other fields of status belong to the imagebuilder and
    // the controller (ADR-0010, ADR-0015).
    let patch = json!({
        "apiVersion": VMImage::api_version(&()).to_string(),
        "kind": VMImage::kind(&()).to_string(),
        "metadata": { "name": name },
        "status": { "perProvider": [row] },
    });
    let api: Api<VMImage> = Api::all(ctx.client.clone());
    // Scoped to this host's Provider, and deliberately NOT forced
    // (ADR-0087). `perProvider` is merge-keyed, so this host owns only its own
    // row; a conflict here would mean another writer is claiming our row, which
    // is a defect that must surface rather than be steamrolled. Forcing under a
    // class-wide manager is what erased every other host's row and produced a
    // permanent write loop.
    api.patch_status(
        name,
        &PatchParams::apply(&ctx.config.field_manager()),
        &Patch::Apply(&patch),
    )
    .await
    .map_err(Error::from)?;
    Ok(())
}

/// Requeue after a reconcile error.
pub fn error_policy(_i: Arc<VMImage>, e: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %e, "VMImage reconcile error");
    requeue_on_error()
}

#[cfg(test)]
#[path = "vmimage_tests.rs"]
mod vmimage_tests;
