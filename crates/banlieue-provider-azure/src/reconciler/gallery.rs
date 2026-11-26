// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The decisions behind publishing a `VMImage`'s `Url` source into a
//! Compute Gallery (ADR-0162 Decisions 4 and 8), as pure functions.
//!
//! banlieue's `VMImage`s are the declared state and the gallery is
//! reconciled to it. Per `Provider`, the reconciler reads the version for the
//! current build's digest by name, looks at its publish Job, and asks
//! [`decide_publish`] what to do; for every version this `VMImage` published
//! for an earlier digest it asks [`decide_superseded`]. Nothing here talks to
//! Kubernetes or Azure.
//!
//! | Version read | Publish Job | Action | Row |
//! | --- | --- | --- | --- |
//! | `Succeeded`, our owner tag, our digest tag | any | none | ready, `Reconciled` |
//! | exists, not ours or another digest | any | none | `VersionConflict` |
//! | `Creating` / `Updating` | absent | create (adopts the create in flight) | `Publishing` |
//! | `Creating` / `Updating` | running | none | `Publishing` |
//! | `Creating` / `Updating` | succeeded | delete Job, recreated next pass | `Publishing` |
//! | missing or `Failed` | absent | create | `Publishing` |
//! | missing or `Failed` | running | none | `Publishing` |
//! | missing or `Failed` | succeeded | delete Job (deleted in Azure by hand: republish) | `Publishing` |
//! | any not ready | failed, retry due | delete Job, recreated next pass | `Publishing` |
//! | any not ready | failed, not due | none | `PublishFailed` |

use banlieue_api::banlieue::{
    BuildArtifactKind, BuildArtifactPhase, BuildArtifactStatus, ImagePerProviderStatus,
};
use banlieue_azure::GalleryImageVersion;
use banlieue_azure::gallery::{
    PROVISIONING_DELETING as STATE_DELETING, PROVISIONING_FAILED as STATE_FAILED,
    PROVISIONING_SUCCEEDED as STATE_SUCCEEDED,
};
use banlieue_azure::tags::TAG_OWNER_UID;

use super::publish_job::{JobState, PUBLISH_RETRY_AFTER_SECS};

/// The algorithm prefix of a digest.
pub const SHA256_PREFIX: &str = "sha256:";
/// Hex characters in a SHA-256 digest.
const SHA256_HEX_LEN: usize = 64;

/// Stable `reason` strings for a published row.
pub mod reasons {
    /// The version for the current digest exists and is ours.
    pub const RECONCILED: &str = "Reconciled";
    /// The `Provider` declares no `imageGallery`.
    pub const GALLERY_NOT_CONFIGURED: &str = "GalleryNotConfigured";
    /// The `Provider` declares no `publisher` identity.
    pub const PUBLISHER_NOT_CONFIGURED: &str = "PublisherNotConfigured";
    /// `status.buildArtifact` is missing, `Pending` or `Building`.
    pub const BUILD_PENDING: &str = "BuildPending";
    /// `status.buildArtifact.phase` is `Failed`.
    pub const BUILD_FAILED: &str = "BuildFailed";
    /// The shared build is an ISO, which a cloud cannot boot.
    pub const BUILD_KIND_UNSUPPORTED: &str = "BuildKindUnsupported";
    /// The build carries no `sha256:` checksum to name and verify it by.
    pub const CHECKSUM_MISSING: &str = "ChecksumMissing";
    /// A publish Job is running, or about to.
    pub const PUBLISHING: &str = "Publishing";
    /// The publish Job failed; it is retried after a delay.
    pub const PUBLISH_FAILED: &str = "PublishFailed";
    /// The version could not be read from Azure.
    pub const LOOKUP_FAILED: &str = "LookupFailed";
    /// A version by our name exists but banlieue did not publish it for this
    /// digest; it is never touched.
    pub const VERSION_CONFLICT: &str = "VersionConflict";
}

/// A `cloudImage` build ready to publish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadyBuild<'a> {
    /// The artifact.
    pub artifact: &'a BuildArtifactStatus,
    /// Its digest, lowercase hex, no prefix.
    pub hex: String,
}

impl ReadyBuild<'_> {
    /// `sha256:<hex>`.
    #[must_use]
    pub fn digest(&self) -> String {
        format!("{SHA256_PREFIX}{}", self.hex)
    }
}

/// The lowercase hex of a `sha256:<64 hex>` checksum, or `None` for any
/// other shape. Pure.
#[must_use]
pub fn sha256_hex(checksum: Option<&str>) -> Option<String> {
    let checksum = checksum?.trim();
    let prefix = checksum.get(..SHA256_PREFIX.len())?;
    if !prefix.eq_ignore_ascii_case(SHA256_PREFIX) {
        return None;
    }
    let hex = &checksum[SHA256_PREFIX.len()..];
    (hex.len() == SHA256_HEX_LEN && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

/// Whether the shared build can be published, and its digest. Pure.
///
/// # Errors
/// `(reason, message)` when it is not built yet, failed, is an ISO, or has
/// no `sha256:` checksum.
pub fn gate_build(
    artifact: Option<&BuildArtifactStatus>,
) -> Result<ReadyBuild<'_>, (&'static str, String)> {
    let Some(artifact) = artifact else {
        return Err((
            reasons::BUILD_PENDING,
            "waiting for banlieue-imagebuilder (status.buildArtifact not set yet)".to_string(),
        ));
    };
    match artifact.phase {
        BuildArtifactPhase::Pending | BuildArtifactPhase::Building => {
            return Err((
                reasons::BUILD_PENDING,
                format!("build in progress ({:?})", artifact.phase),
            ));
        }
        BuildArtifactPhase::Failed => {
            return Err((
                reasons::BUILD_FAILED,
                artifact
                    .message
                    .clone()
                    .unwrap_or_else(|| "the build failed".to_string()),
            ));
        }
        BuildArtifactPhase::Ready => {}
    }
    if artifact.kind != BuildArtifactKind::CloudImage {
        return Err((
            reasons::BUILD_KIND_UNSUPPORTED,
            "the shared build is an iso, which a cloud cannot boot: a vsphere Url source \
             listed before the azure one makes the build an iso; list the azure source first \
             or use a separate VMImage (ADR-0162 Decision 4)"
                .to_string(),
        ));
    }
    let Some(hex) = sha256_hex(artifact.checksum.as_deref()) else {
        return Err((
            reasons::CHECKSUM_MISSING,
            format!(
                "status.buildArtifact.checksum is {:?}; publishing needs sha256:<hex> to name \
                 and verify the image (ADR-0162 Decision 6)",
                artifact.checksum
            ),
        ));
    };
    Ok(ReadyBuild { artifact, hex })
}

/// What one read of a version by name says, for the current digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionState {
    /// No version by that name.
    Missing,
    /// `Succeeded`, carrying this `VMImage`'s owner tag and this digest.
    Published {
        /// The version's resource ID.
        id: String,
    },
    /// Being created or updated.
    InProgress,
    /// ARM reports it `Failed`.
    Failed,
    /// It exists, but banlieue did not publish it for this `VMImage` and
    /// digest; never touched.
    Foreign {
        /// Why it is not ours.
        detail: String,
    },
}

/// Whether `version` carries `owner_uid` as its owner tag.
#[must_use]
pub fn owned_by(version: &GalleryImageVersion, owner_uid: &str) -> bool {
    !owner_uid.is_empty()
        && version
            .tags
            .get(TAG_OWNER_UID)
            .is_some_and(|v| v == owner_uid)
}

/// Classify a read of the current digest's version. Pure.
#[must_use]
pub fn classify_version(
    version: Option<&GalleryImageVersion>,
    owner_uid: &str,
    hex: &str,
) -> VersionState {
    let Some(version) = version else {
        return VersionState::Missing;
    };
    if !owned_by(version, owner_uid) {
        return VersionState::Foreign {
            detail: format!(
                "gallery image version {} exists but is not tagged {TAG_OWNER_UID}={owner_uid}",
                version.id
            ),
        };
    }
    let state = version.provisioning_state.as_str();
    if state.eq_ignore_ascii_case(STATE_FAILED) {
        return VersionState::Failed;
    }
    if !state.eq_ignore_ascii_case(STATE_SUCCEEDED) {
        return VersionState::InProgress;
    }
    let recorded = version.tags.get(banlieue_azure::TAG_SOURCE_SHA256);
    if !recorded.is_some_and(|r| r.eq_ignore_ascii_case(hex)) {
        return VersionState::Foreign {
            detail: format!(
                "gallery image version {} records {}={recorded:?}, not {hex}",
                version.id,
                banlieue_azure::TAG_SOURCE_SHA256
            ),
        };
    }
    VersionState::Published {
        id: version.id.clone(),
    }
}

/// What to do with a Job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobAction {
    /// Leave it.
    None,
    /// Create (server-side apply) it.
    Create,
    /// Delete it; the next pass recreates it.
    Delete,
}

/// The outcome of [`decide_publish`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishDecision {
    /// What to do with the publish Job.
    pub action: JobAction,
    /// Whether the row is ready.
    pub ready: bool,
    /// The row's reason.
    pub reason: &'static str,
    /// The row's message.
    pub message: Option<String>,
}

impl PublishDecision {
    fn new(action: JobAction, ready: bool, reason: &'static str, message: Option<String>) -> Self {
        Self {
            action,
            ready,
            reason,
            message,
        }
    }
}

/// The declared-state loop for the current digest (ADR-0162 Decision 8):
/// the table in this module's documentation. Pure.
#[must_use]
pub fn decide_publish(version: &VersionState, job: &JobState) -> PublishDecision {
    let publishing = |action: JobAction, message: &str| {
        PublishDecision::new(
            action,
            false,
            reasons::PUBLISHING,
            Some(message.to_string()),
        )
    };
    let missing = match version {
        VersionState::Published { .. } => {
            return PublishDecision::new(JobAction::None, true, reasons::RECONCILED, None);
        }
        VersionState::Foreign { detail } => {
            return PublishDecision::new(
                JobAction::None,
                false,
                reasons::VERSION_CONFLICT,
                Some(detail.clone()),
            );
        }
        VersionState::InProgress => false,
        VersionState::Missing | VersionState::Failed => true,
    };
    match job {
        JobState::Absent => publish_absent(missing),
        JobState::Running => publishing(JobAction::None, "publish Job running"),
        JobState::Succeeded if missing => publishing(
            JobAction::Delete,
            "the published version is missing from the gallery (deleted by hand?); republishing",
        ),
        JobState::Succeeded => publishing(
            JobAction::Delete,
            "publish Job finished while the version is still being created; resuming",
        ),
        JobState::Failed { retry_due: true } => publishing(
            JobAction::Delete,
            &format!("publish Job failed more than {PUBLISH_RETRY_AFTER_SECS}s ago; retrying"),
        ),
        JobState::Failed { retry_due: false } => PublishDecision::new(
            JobAction::None,
            false,
            reasons::PUBLISH_FAILED,
            Some(format!(
                "publish Job failed; retried {PUBLISH_RETRY_AFTER_SECS}s after the failure"
            )),
        ),
    }
}

fn publish_absent(missing: bool) -> PublishDecision {
    let message = if missing {
        "publish Job created"
    } else {
        "publish Job created to resume the version create in flight"
    };
    PublishDecision::new(
        JobAction::Create,
        false,
        reasons::PUBLISHING,
        Some(message.to_string()),
    )
}

/// What to do about one superseded version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupersededAction {
    /// Keep tracking it: a machine references it, or its delete is under way.
    Keep,
    /// Stop tracking it: it is gone, or it is not ours to delete.
    Drop,
    /// Create the delete Job.
    CreateDeleteJob,
    /// Delete the finished delete Job, so the next pass recreates it.
    RetryDeleteJob,
}

/// The delete half of the declared-state loop (ADR-0162 Decisions 5 and 8):
/// a version this `VMImage` published, no longer wanted, is deleted once no
/// machine references it. A version without this `VMImage`'s owner tag is
/// never touched. Pure.
#[must_use]
pub fn decide_superseded(
    version: Option<&GalleryImageVersion>,
    owner_uid: &str,
    referenced: bool,
    job: &JobState,
) -> SupersededAction {
    let Some(version) = version else {
        return SupersededAction::Drop;
    };
    if !owned_by(version, owner_uid) {
        return SupersededAction::Drop;
    }
    if referenced
        || version
            .provisioning_state
            .eq_ignore_ascii_case(STATE_DELETING)
    {
        return SupersededAction::Keep;
    }
    match job {
        JobState::Absent => SupersededAction::CreateDeleteJob,
        JobState::Running | JobState::Failed { retry_due: false } => SupersededAction::Keep,
        JobState::Succeeded | JobState::Failed { retry_due: true } => {
            SupersededAction::RetryDeleteJob
        }
    }
}

/// The versions to delete for `row`, given the version now wanted: what the
/// row already tracked, plus its `resolvedRef` when banlieue published it
/// (`sourceDigest` set) for another digest. `current` is the current
/// version's `(definition, version)`, never returned; `None` when nothing is
/// published any more (a `Template` source, or the `VMImage` deleted).
/// Deduplicated case-insensitively, in order. Pure.
#[must_use]
pub fn superseded_refs(
    row: Option<&ImagePerProviderStatus>,
    current: Option<(&str, &str)>,
) -> Vec<String> {
    let Some(row) = row else {
        return Vec::new();
    };
    let published = row
        .resolved_ref
        .as_ref()
        .filter(|_| row.source_digest.is_some());
    let mut out: Vec<String> = Vec::new();
    for id in row.superseded_refs.iter().chain(published) {
        let is_current = current.is_some_and(|(definition, version)| {
            VersionPath::parse(id).is_some_and(|p| {
                p.image.eq_ignore_ascii_case(definition) && p.version.eq_ignore_ascii_case(version)
            })
        });
        if is_current || out.iter().any(|o| o.eq_ignore_ascii_case(id)) {
            continue;
        }
        out.push(id.clone());
    }
    out
}

/// The parts of a gallery image version resource ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionPath {
    /// The gallery's resource group.
    pub resource_group: String,
    /// The gallery.
    pub gallery: String,
    /// The image definition.
    pub image: String,
    /// The version.
    pub version: String,
}

impl VersionPath {
    /// Parse `/subscriptions/<s>/resourceGroups/<rg>/providers/Microsoft.Compute/galleries/<g>/images/<i>/versions/<v>`,
    /// segment names compared case-insensitively as ARM does, values kept as
    /// written. `None` for any other shape. Pure.
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        let segments: Vec<&str> = id.trim().trim_matches('/').split('/').collect();
        let value_after = |key: &str| {
            segments
                .windows(2)
                .find(|w| w[0].eq_ignore_ascii_case(key))
                .map(|w| w[1].to_string())
                .filter(|v| !v.is_empty())
        };
        let provider_ok =
            value_after("providers").is_some_and(|p| p.eq_ignore_ascii_case("Microsoft.Compute"));
        if !provider_ok || !segments.first()?.eq_ignore_ascii_case("subscriptions") {
            return None;
        }
        Some(Self {
            resource_group: value_after("resourceGroups")?,
            gallery: value_after("galleries")?,
            image: value_after("images")?,
            version: value_after("versions")?,
        })
    }
}

/// Whether `id` names the same version as `other`, compared as ARM IDs are.
#[must_use]
pub fn same_id(id: &str, other: &str) -> bool {
    id.trim().eq_ignore_ascii_case(other.trim())
}

/// The ID a row's `resolvedRef` names when the `VMImage` it belongs to is
/// being deleted: banlieue's own published version only. Pure.
#[must_use]
pub fn owned_refs_on_delete(row: &ImagePerProviderStatus) -> Vec<String> {
    superseded_refs(Some(row), None)
}

/// Whether the deletion finalizer can come off: no row of a `Provider` that
/// still exists has a version left to delete. A deleted `Provider`'s
/// versions cannot be reached with its identity, so its row does not hold
/// the delete (mirrors the vSphere provider's rule, ADR-0028). Pure.
#[must_use]
pub fn finalizer_can_go(
    rows: &[ImagePerProviderStatus],
    live_providers: &std::collections::BTreeSet<(String, String)>,
) -> bool {
    rows.iter()
        .filter(|r| !owned_refs_on_delete(r).is_empty())
        .all(|r| !live_providers.contains(&(r.provider_namespace.clone(), r.provider_name.clone())))
}

#[cfg(test)]
#[path = "gallery_tests.rs"]
mod gallery_tests;
