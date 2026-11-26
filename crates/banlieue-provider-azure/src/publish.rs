// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue provider azure image-publish`: the publish Job (ADR-0162).
//!
//! One run publishes one raw disk as one image version in an Azure Compute
//! Gallery (`--mode publish`), or deletes one version banlieue published
//! (`--mode delete`), then exits. It runs as its own federated identity
//! (ADR-0159 Decision 3); the runtime provider never holds these rights.
//!
//! Publish, in order, each step adopted when a restarted Job finds it done
//! or in flight (every name is deterministic, ADR-0162 Decision 3 amended):
//!
//! 1. the source's SHA-256 is checked against `--checksum` before any
//!    network call, and `--version` must be the name that digest maps to;
//! 2. the image definition is ensured (always `DiskControllerTypes=SCSI,
//!    NVMe`; Trusted Launch when asked);
//! 3. a version that already exists for this digest is adopted: done when
//!    `Succeeded`, polled when `Creating`;
//! 4. otherwise the raw disk is uploaded as a fixed VHD (zero padding to a
//!    MiB plus the footer from `banlieue-diskformat`, all-zero ranges
//!    skipped) to an upload disk through a write SAS, whose digest is
//!    checked again as it streams; the disk is finalised, snapshotted, and
//!    the version created from the snapshot and polled to a long deadline;
//! 5. the snapshot and upload disk are deleted (finding 6).
//!
//! The result is one line of JSON on the termination log, which the
//! provider's `VMImage` reconciler reads: never a token, never a SAS.

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use banlieue_azure::gallery::{
    DISK_STATE_ACTIVE_UPLOAD, DISK_STATE_READY_TO_UPLOAD, DISK_STATE_UNATTACHED,
    GalleryImageVersionRequest, ImageDefinitionRequest, PROVISIONING_FAILED,
    PROVISIONING_SUCCEEDED, SnapshotRequest, UPLOAD_ACCESS_SECONDS, UploadDiskRequest, image_offer,
    is_valid_image_definition_name, is_valid_version_name, snapshot_name, upload_disk_name,
};
use banlieue_azure::tags::TAG_OWNER_UID;
use banlieue_azure::upload::{HttpPageWriterFactory, UploadOptions, upload_pages};
use banlieue_azure::{
    ArmClient, ArmConfig, DiskAccess, FederatedConfig, FederatedToken, OperationHandle,
    OperationStatus, OwnershipTags, PageWriterFactory, PublishApi, SasUrl, TAG_SOURCE_SHA256,
    gallery_version_name,
};
use banlieue_diskformat::vhd::{FixedVhdLayout, fixed_footer, vhd_timestamp};
use banlieue_provider_sdk::artifact::{digest_hex, hex_encode};
use clap::{Args, ValueEnum};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use tokio::time::Instant;
use tracing::{info, warn};

use crate::client::DEFAULT_FEDERATED_TOKEN_FILE;
use crate::cloud::{resolve_cloud, upload_host_suffix};

/// The kind recorded in the ownership tags of everything a publish creates.
pub const OWNER_KIND_VMIMAGE: &str = "VMImage";
/// Where Kubernetes reads a container's termination message.
pub const DEFAULT_TERMINATION_LOG: &str = "/dev/termination-log";
/// How long the version create may take before the Job fails loudly. The
/// spike saw 7 to 22 minutes; the provider's retry adopts what still runs.
pub const VERSION_DEADLINE: Duration = Duration::from_secs(90 * 60);
/// How long any other operation (disk, access, snapshot, delete) may take.
pub const OPERATION_DEADLINE: Duration = Duration::from_secs(30 * 60);
/// Between reads of something in flight.
pub const POLL_INTERVAL: Duration = Duration::from_secs(15);
/// The only checksum algorithm a publish accepts: the version's tag
/// records a SHA-256 (ADR-0162 Decision 6).
pub const CHECKSUM_PREFIX: &str = "sha256:";
/// Hex characters of a SHA-256.
pub const SHA256_HEX_LEN: usize = 64;
/// Kubernetes keeps at most 4096 bytes of a termination message.
const MAX_TERMINATION_MESSAGE_LEN: usize = 1024;
const UUID_LEN: usize = 16;
const UUID_VERSION_BYTE: usize = 6;
const UUID_VARIANT_BYTE: usize = 8;
const UUID_VERSION_4: u8 = 0x40;
const UUID_VERSION_MASK: u8 = 0x0f;
const UUID_VARIANT_RFC4122: u8 = 0x80;
const UUID_VARIANT_MASK: u8 = 0x3f;
const SAS_SIGNATURE_MARKER: &str = "sig=";
const REDACTED: &str = "<redacted>";

/// What the Job does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Publish `--source` as `--version`.
    Publish,
    /// Delete `--version` (and its definition when it was the last).
    Delete,
}

/// Flags of `banlieue provider azure image-publish`. Set by the `VMImage`
/// reconciler's Job; stable, so a failed publish can be reproduced by hand.
#[derive(Debug, Args)]
pub struct PublishArgs {
    /// `publish` or `delete`.
    #[arg(long, value_enum)]
    pub mode: Mode,
    /// Resource Manager endpoint; must be a known Azure cloud's.
    #[arg(long)]
    pub endpoint: String,
    /// Entra ID login host; defaults to the endpoint's cloud's, and must be
    /// it when set.
    #[arg(long)]
    pub authority_host: Option<String>,
    /// Tenant of the publisher's identity.
    #[arg(long)]
    pub tenant_id: String,
    /// Client ID of the publisher's identity (ADR-0159 Decision 3).
    #[arg(long)]
    pub client_id: String,
    /// Subscription holding the gallery.
    #[arg(long)]
    pub subscription_id: String,
    /// Region of the gallery, the temporaries and the one replica.
    #[arg(long)]
    pub location: String,
    /// Resource group of the gallery (and the temporaries).
    #[arg(long)]
    pub gallery_resource_group: String,
    /// The gallery, which an admin created (ADR-0162 Decision 8).
    #[arg(long)]
    pub gallery_name: String,
    /// Image definition, from `image_definition_name`.
    #[arg(long)]
    pub image_definition: String,
    /// Image version, from `gallery_version_name` of the source digest.
    #[arg(long)]
    pub version: String,
    /// The raw disk, on the mounted artifacts volume (publish only).
    #[arg(long)]
    pub source: Option<PathBuf>,
    /// `sha256:<hex>` of the raw disk (publish only).
    #[arg(long)]
    pub checksum: Option<String>,
    /// Declare Trusted Launch on the definition.
    #[arg(long, default_value_t = false)]
    pub trusted_launch: bool,
    /// The `VMImage`'s `metadata.uid`: the owner tag.
    #[arg(long)]
    pub owner_uid: String,
    /// The owner's namespace; `VMImage` is cluster-scoped, so usually unset.
    #[arg(long)]
    pub owner_namespace: Option<String>,
    /// `<namespace>.<name>` of the `Provider`: the provider tag.
    #[arg(long)]
    pub provider: String,
    /// The projected ServiceAccount token exchanged at Entra ID.
    #[arg(
        long,
        env = "AZURE_FEDERATED_TOKEN_FILE",
        default_value = DEFAULT_FEDERATED_TOKEN_FILE,
    )]
    pub federated_token_file: PathBuf,
    /// Where the one-line JSON result goes.
    #[arg(long, default_value = DEFAULT_TERMINATION_LOG)]
    pub termination_log: PathBuf,
}

/// Why a publish or delete failed. [`PublishError::code`] is the stable
/// `error` of the termination log.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// A flag is missing or malformed.
    #[error("{0}")]
    InvalidArgument(String),
    /// The source is not what `--checksum` says.
    #[error("{0}")]
    ChecksumMismatch(String),
    /// The source could not be read.
    #[error("{0}")]
    Source(String),
    /// A resource of the deterministic name exists but is not ours.
    #[error("{0}")]
    ForeignResource(String),
    /// A resource is in a state the Job cannot continue from.
    #[error("{0}")]
    UnexpectedState(String),
    /// The image version create ended `Failed`.
    #[error("{0}")]
    VersionFailed(String),
    /// Something in flight did not finish by its deadline.
    #[error("{0}")]
    Timeout(String),
    /// Azure refused, failed, or was unreachable.
    #[error("{0}")]
    Azure(#[from] banlieue_azure::Error),
}

impl PublishError {
    /// The stable code written as the termination log's `error`.
    #[must_use]
    pub fn code(&self) -> String {
        match self {
            Self::InvalidArgument(_) => "InvalidArgument".to_string(),
            Self::ChecksumMismatch(_) => "ChecksumMismatch".to_string(),
            Self::Source(_) => "SourceUnreadable".to_string(),
            Self::ForeignResource(_) => "ForeignResource".to_string(),
            Self::UnexpectedState(_) => "UnexpectedState".to_string(),
            Self::VersionFailed(_) => "VersionFailed".to_string(),
            Self::Timeout(_) => "Timeout".to_string(),
            Self::Azure(e) => azure_code(e),
        }
    }
}

fn azure_code(e: &banlieue_azure::Error) -> String {
    use banlieue_azure::Error as E;
    match e {
        E::UntrustedUploadHost(_) => "UntrustedUploadHost".to_string(),
        E::UntrustedUrl(_) => "UntrustedUrl".to_string(),
        E::UploadSource(_) => "SourceUnreadable".to_string(),
        E::Throttled { .. } => "Throttled".to_string(),
        E::Transport(_) => "Transport".to_string(),
        E::InvalidTag(_) => "InvalidTag".to_string(),
        E::Decode(_) => "UnexpectedResponse".to_string(),
        other => other
            .code()
            .filter(|c| !c.is_empty())
            .unwrap_or("AzureError")
            .to_string(),
    }
}

/// What a publish produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Published {
    /// The image version's resource ID.
    pub version_id: String,
    /// The source's SHA-256, lowercase hex.
    pub source_sha256: String,
    /// The smallest OS disk that holds the image (ADR-0162 finding 7).
    pub min_os_disk_size_gib: u32,
}

/// How long to wait and how to upload; [`Timings::default`] in production,
/// short in tests.
#[derive(Clone, Debug)]
pub struct Timings {
    /// Between reads of something in flight.
    pub poll_interval: Duration,
    /// The version create's deadline.
    pub version_deadline: Duration,
    /// Every other operation's deadline.
    pub operation_deadline: Duration,
    /// Put Page pacing.
    pub upload: UploadOptions,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            poll_interval: POLL_INTERVAL,
            version_deadline: VERSION_DEADLINE,
            operation_deadline: OPERATION_DEADLINE,
            upload: UploadOptions::default(),
        }
    }
}

/// Where a publish or delete acts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The gallery's resource group.
    pub resource_group: String,
    /// The gallery.
    pub gallery: String,
    /// The image definition.
    pub definition: String,
    /// The image version.
    pub version: String,
    /// Region.
    pub location: String,
}

/// One publish.
#[derive(Clone, Debug)]
pub struct PublishPlan {
    /// Where.
    pub target: Target,
    /// Trusted Launch definition.
    pub trusted_launch: bool,
    /// Ownership tags of everything created.
    pub tags: OwnershipTags,
    /// The raw disk.
    pub source: PathBuf,
    /// Its expected SHA-256, lowercase hex.
    pub checksum_hex: String,
}

/// One delete.
#[derive(Clone, Debug)]
pub struct DeletePlan {
    /// Where.
    pub target: Target,
    /// Only resources tagged with this owner UID are deleted.
    pub owner_uid: String,
}

/// The termination log line for a publish result.
#[must_use]
pub fn published_line(published: &Published) -> String {
    json!({
        "versionId": published.version_id,
        "sourceDigest": format!("{CHECKSUM_PREFIX}{}", published.source_sha256),
        "minOsDiskSizeGiB": published.min_os_disk_size_gib,
    })
    .to_string()
}

/// The termination log line for a completed delete.
#[must_use]
pub fn deleted_line() -> String {
    json!({ "deleted": true }).to_string()
}

/// `message` with anything after a SAS signature marker removed, cut to a
/// length Kubernetes keeps. Belt and braces: no error built here carries a
/// SAS or a token.
#[must_use]
pub fn redact(message: &str) -> String {
    let cut = match message.find(SAS_SIGNATURE_MARKER) {
        Some(at) => format!("{}{REDACTED}", &message[..at + SAS_SIGNATURE_MARKER.len()]),
        None => message.to_string(),
    };
    cut.chars().take(MAX_TERMINATION_MESSAGE_LEN).collect()
}

/// The termination log line for a failure.
#[must_use]
pub fn error_line(error: &PublishError) -> String {
    json!({ "error": error.code(), "message": redact(&error.to_string()) }).to_string()
}

/// The `sha256:<hex>` argument's hex, lowercased.
///
/// # Errors
/// [`PublishError::InvalidArgument`] for any other algorithm or shape.
pub fn parse_checksum(checksum: &str) -> Result<String, PublishError> {
    let hex = checksum.strip_prefix(CHECKSUM_PREFIX).ok_or_else(|| {
        PublishError::InvalidArgument(format!(
            "--checksum must be {CHECKSUM_PREFIX}<hex>; a publish records a SHA-256"
        ))
    })?;
    if hex.len() != SHA256_HEX_LEN || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(PublishError::InvalidArgument(format!(
            "--checksum must carry {SHA256_HEX_LEN} hex characters"
        )));
    }
    Ok(hex.to_ascii_lowercase())
}

/// The footer's unique ID for a source digest: the same source always
/// gets the same footer bytes, with RFC 4122 version 4 bits set.
#[must_use]
pub fn unique_id_for(source_sha256_hex: &str) -> [u8; 16] {
    let digest = Sha256::digest(source_sha256_hex.as_bytes());
    let mut id = [0u8; UUID_LEN];
    id.copy_from_slice(&digest[..UUID_LEN]);
    id[UUID_VERSION_BYTE] = (id[UUID_VERSION_BYTE] & UUID_VERSION_MASK) | UUID_VERSION_4;
    id[UUID_VARIANT_BYTE] = (id[UUID_VARIANT_BYTE] & UUID_VARIANT_MASK) | UUID_VARIANT_RFC4122;
    id
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Streams a reader while hashing what passes through, so the bytes
/// uploaded are checked against the digest a second time.
struct HashingReader<R> {
    inner: R,
    hasher: Arc<Mutex<Sha256>>,
}

impl<R: AsyncRead + Unpin> AsyncRead for HashingReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &polled {
            self.hasher
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .update(&buf.filled()[before..]);
        }
        polled
    }
}

fn source_error(path: &Path, e: &io::Error) -> PublishError {
    PublishError::Source(format!("cannot read {}: {e}", path.display()))
}

/// Hashes the source and checks it against the plan, before anything
/// touches the network. Returns its length.
async fn verify_source(plan: &PublishPlan) -> Result<u64, PublishError> {
    let mut file = tokio::fs::File::open(&plan.source)
        .await
        .map_err(|e| source_error(&plan.source, &e))?;
    let len = file
        .metadata()
        .await
        .map_err(|e| source_error(&plan.source, &e))?
        .len();
    let actual = digest_hex::<Sha256>(&mut file)
        .await
        .map_err(|e| source_error(&plan.source, &e))?;
    if actual != plan.checksum_hex {
        return Err(PublishError::ChecksumMismatch(format!(
            "{} has sha256 {actual}, expected {}; refusing to publish a corrupted or substituted disk",
            plan.source.display(),
            plan.checksum_hex
        )));
    }
    let expected_version = gallery_version_name(&plan.checksum_hex);
    if plan.target.version != expected_version {
        return Err(PublishError::InvalidArgument(format!(
            "--version {} is not the name of this digest ({expected_version})",
            plan.target.version
        )));
    }
    Ok(len)
}

async fn wait_operation(
    api: &dyn PublishApi,
    handle: Option<OperationHandle>,
    what: &str,
    timings: &Timings,
) -> Result<(), PublishError> {
    let Some(handle) = handle else {
        return Ok(());
    };
    let deadline = Instant::now() + timings.operation_deadline;
    loop {
        match api.operation_status(&handle).await? {
            OperationStatus::Succeeded => return Ok(()),
            OperationStatus::Failed { code, message } => {
                return Err(banlieue_azure::error::operation_failure(&code, &message).into());
            }
            OperationStatus::Canceled => {
                return Err(PublishError::UnexpectedState(format!(
                    "{what} was canceled"
                )));
            }
            OperationStatus::InProgress => {}
        }
        if Instant::now() >= deadline {
            return Err(PublishError::Timeout(format!(
                "{what} did not finish within {:?}",
                timings.operation_deadline
            )));
        }
        tokio::time::sleep(handle.retry_after.unwrap_or(timings.poll_interval)).await;
    }
}

fn owned_by(tags: &std::collections::BTreeMap<String, String>, owner_uid: &str) -> bool {
    tags.get(TAG_OWNER_UID).map(String::as_str) == Some(owner_uid)
}

fn foreign(what: &str, name: &str) -> PublishError {
    PublishError::ForeignResource(format!(
        "{what} {name} exists but is not tagged as this VMImage's; banlieue does not touch it"
    ))
}

async fn ensure_definition(
    api: &dyn PublishApi,
    plan: &PublishPlan,
    timings: &Timings,
) -> Result<(), PublishError> {
    let t = &plan.target;
    if let Some(existing) = api
        .get_gallery_image_definition(&t.resource_group, &t.gallery, &t.definition)
        .await?
    {
        if !plan.tags.owns(&existing.tags) {
            return Err(foreign("image definition", &t.definition));
        }
        return Ok(());
    }
    let request = ImageDefinitionRequest {
        location: t.location.clone(),
        offer: image_offer(&t.definition, plan.trusted_launch),
        trusted_launch: plan.trusted_launch,
        tags: plan.tags.to_map(),
    };
    info!(definition = %t.definition, trusted_launch = plan.trusted_launch, "creating the image definition");
    let handle = api
        .put_gallery_image_definition(&t.resource_group, &t.gallery, &t.definition, &request)
        .await?;
    wait_operation(api, handle, "the image definition create", timings).await
}

/// Polls the version until it succeeds, fails, or the deadline passes.
async fn wait_version(
    api: &dyn PublishApi,
    plan: &PublishPlan,
    timings: &Timings,
) -> Result<String, PublishError> {
    let t = &plan.target;
    let deadline = Instant::now() + timings.version_deadline;
    loop {
        let Some(version) = api
            .get_gallery_image_version(&t.resource_group, &t.gallery, &t.definition, &t.version)
            .await?
        else {
            return Err(PublishError::UnexpectedState(format!(
                "image version {} disappeared while being created",
                t.version
            )));
        };
        match version.provisioning_state.as_str() {
            PROVISIONING_SUCCEEDED => return Ok(version.id),
            PROVISIONING_FAILED => {
                return Err(PublishError::VersionFailed(format!(
                    "image version {} ended Failed",
                    t.version
                )));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(PublishError::Timeout(format!(
                "image version {} still {} after {:?}; the provider's retry adopts it",
                t.version, version.provisioning_state, timings.version_deadline
            )));
        }
        tokio::time::sleep(timings.poll_interval).await;
    }
}

/// What the existing version, if any, means for this publish.
enum Existing {
    /// Done: its ID.
    Done(String),
    /// Nothing usable: create it.
    Create,
}

async fn adopt_version(
    api: &dyn PublishApi,
    plan: &PublishPlan,
    timings: &Timings,
) -> Result<Existing, PublishError> {
    let t = &plan.target;
    let Some(version) = api
        .get_gallery_image_version(&t.resource_group, &t.gallery, &t.definition, &t.version)
        .await?
    else {
        return Ok(Existing::Create);
    };
    let digest_matches =
        version.tags.get(TAG_SOURCE_SHA256).map(String::as_str) == Some(plan.checksum_hex.as_str());
    if !plan.tags.owns(&version.tags) || !digest_matches {
        return Err(foreign("image version", &t.version));
    }
    match version.provisioning_state.as_str() {
        PROVISIONING_SUCCEEDED => {
            info!(version = %t.version, "the image version already exists; adopting it");
            Ok(Existing::Done(version.id))
        }
        PROVISIONING_FAILED => {
            warn!(version = %t.version, "the image version failed earlier; deleting it to create it again");
            let handle = api
                .delete_gallery_image_version(
                    &t.resource_group,
                    &t.gallery,
                    &t.definition,
                    &t.version,
                )
                .await?;
            wait_operation(api, handle, "deleting the failed image version", timings).await?;
            Ok(Existing::Create)
        }
        state => {
            info!(version = %t.version, state, "the image version is in flight; adopting it");
            Ok(Existing::Done(wait_version(api, plan, timings).await?))
        }
    }
}

async fn grant_write(
    api: &dyn PublishApi,
    rg: &str,
    disk: &str,
    timings: &Timings,
) -> Result<SasUrl, PublishError> {
    let handle = match api
        .grant_disk_write_access(rg, disk, UPLOAD_ACCESS_SECONDS)
        .await?
    {
        DiskAccess::Granted(sas) => return Ok(sas),
        DiskAccess::Pending(handle) => handle,
    };
    let deadline = Instant::now() + timings.operation_deadline;
    loop {
        if let Some(sas) = api.disk_access_result(&handle).await? {
            return Ok(sas);
        }
        if Instant::now() >= deadline {
            return Err(PublishError::Timeout(
                "write access to the upload disk was not granted in time".to_string(),
            ));
        }
        tokio::time::sleep(handle.retry_after.unwrap_or(timings.poll_interval)).await;
    }
}

/// Uploads the VHD through `sas`, hashing the raw part again as it goes.
async fn upload(
    writers: &dyn PageWriterFactory,
    sas: &SasUrl,
    plan: &PublishPlan,
    layout: FixedVhdLayout,
    timings: &Timings,
) -> Result<(), PublishError> {
    let writer = writers.writer(sas)?;
    let file = tokio::fs::File::open(&plan.source)
        .await
        .map_err(|e| source_error(&plan.source, &e))?;
    let hasher = Arc::new(Mutex::new(Sha256::new()));
    let footer = fixed_footer(
        layout.virtual_size,
        vhd_timestamp(now_unix_secs()),
        unique_id_for(&plan.checksum_hex),
    );
    let stream = HashingReader {
        inner: file.take(layout.raw_len),
        hasher: hasher.clone(),
    }
    .chain(tokio::io::repeat(0).take(layout.padding))
    .chain(io::Cursor::new(footer.to_vec()));
    info!(host = ?sas.host(), bytes = layout.total_len, "uploading the VHD");
    let stats = upload_pages(stream, layout.total_len, writer, &timings.upload).await?;
    let uploaded = hex_encode(
        &hasher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .finalize(),
    );
    if uploaded != plan.checksum_hex {
        return Err(PublishError::ChecksumMismatch(format!(
            "{} changed while it was uploaded (sha256 {uploaded}); the upload disk stays open and is not used",
            plan.source.display()
        )));
    }
    info!(
        written = stats.bytes_written,
        skipped = stats.bytes_skipped,
        ranges = stats.ranges_written,
        "upload complete"
    );
    Ok(())
}

/// Ensures a finalised upload disk holding the VHD; returns its ID.
async fn ensure_uploaded_disk(
    api: &dyn PublishApi,
    writers: &dyn PageWriterFactory,
    plan: &PublishPlan,
    layout: FixedVhdLayout,
    timings: &Timings,
) -> Result<String, PublishError> {
    let t = &plan.target;
    let name = upload_disk_name(&t.gallery, &t.definition, &t.version);
    let disk = match api.get_disk(&t.resource_group, &name).await? {
        Some(disk) => disk,
        None => {
            let request = UploadDiskRequest {
                location: t.location.clone(),
                upload_size_bytes: layout.total_len,
                tags: plan.tags.to_map(),
            };
            info!(disk = %name, bytes = layout.total_len, "creating the upload disk");
            let handle = api
                .put_upload_disk(&t.resource_group, &name, &request)
                .await?;
            wait_operation(api, handle, "the upload disk create", timings).await?;
            api.get_disk(&t.resource_group, &name)
                .await?
                .ok_or_else(|| {
                    PublishError::UnexpectedState(format!(
                        "upload disk {name} is missing after create"
                    ))
                })?
        }
    };
    if !plan.tags.owns(&disk.tags) {
        return Err(foreign("disk", &name));
    }
    if disk
        .upload_size_bytes
        .is_some_and(|n| n != layout.total_len)
    {
        return Err(PublishError::UnexpectedState(format!(
            "upload disk {name} was sized for {:?} bytes, the VHD is {}",
            disk.upload_size_bytes, layout.total_len
        )));
    }
    match disk.disk_state.as_str() {
        DISK_STATE_UNATTACHED => {
            info!(disk = %name, "the upload disk is already finalised; reusing it");
            return Ok(disk.id);
        }
        DISK_STATE_READY_TO_UPLOAD | DISK_STATE_ACTIVE_UPLOAD => {}
        other => {
            return Err(PublishError::UnexpectedState(format!(
                "upload disk {name} is {other}"
            )));
        }
    }
    let sas = grant_write(api, &t.resource_group, &name, timings).await?;
    upload(writers, &sas, plan, layout, timings).await?;
    let handle = api.revoke_disk_access(&t.resource_group, &name).await?;
    wait_operation(api, handle, "finalising the upload disk", timings).await?;
    Ok(disk.id)
}

/// Ensures the snapshot exists; returns its ID.
async fn ensure_snapshot(
    api: &dyn PublishApi,
    writers: &dyn PageWriterFactory,
    plan: &PublishPlan,
    layout: FixedVhdLayout,
    timings: &Timings,
) -> Result<String, PublishError> {
    let t = &plan.target;
    let name = snapshot_name(&t.gallery, &t.definition, &t.version);
    if let Some(snapshot) = api.get_snapshot(&t.resource_group, &name).await? {
        if !plan.tags.owns(&snapshot.tags) {
            return Err(foreign("snapshot", &name));
        }
        if snapshot.provisioning_state == PROVISIONING_SUCCEEDED {
            info!(snapshot = %name, "the snapshot already exists; reusing it");
            return Ok(snapshot.id);
        }
        return wait_snapshot(api, &t.resource_group, &name, timings).await;
    }
    let disk_id = ensure_uploaded_disk(api, writers, plan, layout, timings).await?;
    let request = SnapshotRequest {
        location: t.location.clone(),
        source_disk_id: disk_id,
        tags: plan.tags.to_map(),
    };
    info!(snapshot = %name, "creating the snapshot");
    let handle = api.put_snapshot(&t.resource_group, &name, &request).await?;
    wait_operation(api, handle, "the snapshot create", timings).await?;
    wait_snapshot(api, &t.resource_group, &name, timings).await
}

async fn wait_snapshot(
    api: &dyn PublishApi,
    rg: &str,
    name: &str,
    timings: &Timings,
) -> Result<String, PublishError> {
    let deadline = Instant::now() + timings.operation_deadline;
    loop {
        let snapshot = api.get_snapshot(rg, name).await?.ok_or_else(|| {
            PublishError::UnexpectedState(format!("snapshot {name} is missing after create"))
        })?;
        match snapshot.provisioning_state.as_str() {
            PROVISIONING_SUCCEEDED => return Ok(snapshot.id),
            PROVISIONING_FAILED => {
                return Err(PublishError::UnexpectedState(format!(
                    "snapshot {name} ended Failed"
                )));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(PublishError::Timeout(format!(
                "snapshot {name} not ready in time"
            )));
        }
        tokio::time::sleep(timings.poll_interval).await;
    }
}

/// Deletes the snapshot and upload disk of `target` when they are ours
/// (finding 6: they cost more than the image).
async fn delete_temporaries(
    api: &dyn PublishApi,
    target: &Target,
    owner_uid: &str,
    timings: &Timings,
) -> Result<(), PublishError> {
    let snap = snapshot_name(&target.gallery, &target.definition, &target.version);
    if let Some(snapshot) = api.get_snapshot(&target.resource_group, &snap).await?
        && owned_by(&snapshot.tags, owner_uid)
    {
        let handle = api.delete_snapshot(&target.resource_group, &snap).await?;
        wait_operation(api, handle, "deleting the snapshot", timings).await?;
    }
    let disk = upload_disk_name(&target.gallery, &target.definition, &target.version);
    if let Some(d) = api.get_disk(&target.resource_group, &disk).await?
        && owned_by(&d.tags, owner_uid)
    {
        if d.disk_state == DISK_STATE_ACTIVE_UPLOAD {
            let handle = api
                .revoke_disk_access(&target.resource_group, &disk)
                .await?;
            wait_operation(api, handle, "closing the upload disk", timings).await?;
        }
        let handle = api.delete_disk(&target.resource_group, &disk).await?;
        wait_operation(api, handle, "deleting the upload disk", timings).await?;
    }
    Ok(())
}

fn min_disk_gib(layout: FixedVhdLayout) -> Result<u32, PublishError> {
    u32::try_from(layout.min_disk_gib())
        .map_err(|_| PublishError::Source("the disk is too large to publish".to_string()))
}

/// Publishes `plan` (see the module docs for the steps).
///
/// # Errors
/// [`PublishError`]: the checksum before any call to `api`; ownership,
/// state and deadline failures; and Azure's own refusals.
pub async fn publish(
    api: &dyn PublishApi,
    writers: &dyn PageWriterFactory,
    plan: &PublishPlan,
    timings: &Timings,
) -> Result<Published, PublishError> {
    let raw_len = verify_source(plan).await?;
    let layout =
        FixedVhdLayout::for_raw_len(raw_len).map_err(|e| PublishError::Source(e.to_string()))?;
    let min_os_disk_size_gib = min_disk_gib(layout)?;
    info!(sha256 = %plan.checksum_hex, version = %plan.target.version, "source verified");

    ensure_definition(api, plan, timings).await?;
    let version_id = match adopt_version(api, plan, timings).await? {
        Existing::Done(id) => id,
        Existing::Create => {
            let snapshot_id = ensure_snapshot(api, writers, plan, layout, timings).await?;
            let t = &plan.target;
            let mut tags = plan.tags.to_map();
            tags.insert(TAG_SOURCE_SHA256.to_string(), plan.checksum_hex.clone());
            let request = GalleryImageVersionRequest {
                location: t.location.clone(),
                snapshot_id,
                end_of_life: None,
                tags,
            };
            info!(version = %t.version, "creating the image version");
            // Observed by reading the version's provisioning state, which a
            // restarted Job can do too; the operation handle adds nothing.
            api.put_gallery_image_version(
                &t.resource_group,
                &t.gallery,
                &t.definition,
                &t.version,
                &request,
            )
            .await?;
            wait_version(api, plan, timings).await?
        }
    };
    delete_temporaries(api, &plan.target, &plan.tags.owner_uid, timings).await?;
    Ok(Published {
        version_id,
        source_sha256: plan.checksum_hex.clone(),
        min_os_disk_size_gib,
    })
}

/// Deletes the version `plan` names when its owner tag is `owner_uid`, any
/// temporaries left by a failed publish of it, and the definition when it
/// is ours and now empty. Something already gone is success.
///
/// # Errors
/// [`PublishError::ForeignResource`] for a version tagged for another
/// owner; Azure's refusals.
pub async fn unpublish(
    api: &dyn PublishApi,
    plan: &DeletePlan,
    timings: &Timings,
) -> Result<(), PublishError> {
    let t = &plan.target;
    if let Some(version) = api
        .get_gallery_image_version(&t.resource_group, &t.gallery, &t.definition, &t.version)
        .await?
    {
        if !owned_by(&version.tags, &plan.owner_uid) {
            return Err(foreign("image version", &t.version));
        }
        info!(version = %t.version, "deleting the image version");
        let handle = api
            .delete_gallery_image_version(&t.resource_group, &t.gallery, &t.definition, &t.version)
            .await?;
        wait_operation(api, handle, "deleting the image version", timings).await?;
    }
    delete_temporaries(api, t, &plan.owner_uid, timings).await?;
    let Some(definition) = api
        .get_gallery_image_definition(&t.resource_group, &t.gallery, &t.definition)
        .await?
    else {
        return Ok(());
    };
    if !owned_by(&definition.tags, &plan.owner_uid) {
        return Ok(());
    }
    let remaining = api
        .list_gallery_image_versions(&t.resource_group, &t.gallery, &t.definition)
        .await?;
    if !remaining.is_empty() {
        info!(definition = %t.definition, versions = remaining.len(), "keeping the image definition: it has other versions");
        return Ok(());
    }
    info!(definition = %t.definition, "deleting the empty image definition");
    let handle = api
        .delete_gallery_image_definition(&t.resource_group, &t.gallery, &t.definition)
        .await?;
    wait_operation(api, handle, "deleting the image definition", timings).await
}

fn required<'a>(value: Option<&'a PathBuf>, flag: &str) -> Result<&'a PathBuf, PublishError> {
    value.ok_or_else(|| PublishError::InvalidArgument(format!("{flag} is required to publish")))
}

/// The target and ownership tags the flags describe, checked.
///
/// # Errors
/// [`PublishError::InvalidArgument`] for an invalid name or provider.
pub fn target_from(args: &PublishArgs) -> Result<(Target, OwnershipTags), PublishError> {
    if !is_valid_image_definition_name(&args.image_definition) {
        return Err(PublishError::InvalidArgument(format!(
            "--image-definition {:?} is not a valid gallery image name",
            args.image_definition
        )));
    }
    if !is_valid_version_name(&args.version) {
        return Err(PublishError::InvalidArgument(format!(
            "--version {:?} must be major.minor.patch",
            args.version
        )));
    }
    let Some((provider_ns, provider_name)) = args.provider.split_once('.') else {
        return Err(PublishError::InvalidArgument(
            "--provider must be <namespace>.<name>".to_string(),
        ));
    };
    if args.owner_uid.trim().is_empty() {
        return Err(PublishError::InvalidArgument(
            "--owner-uid must not be empty".to_string(),
        ));
    }
    let mut tags = OwnershipTags::new(
        &args.owner_uid,
        OWNER_KIND_VMIMAGE,
        provider_ns,
        provider_name,
    );
    if let Some(ns) = args.owner_namespace.as_deref().filter(|n| !n.is_empty()) {
        tags = tags.with_owner_namespace(ns);
    }
    Ok((
        Target {
            resource_group: args.gallery_resource_group.clone(),
            gallery: args.gallery_name.clone(),
            definition: args.image_definition.clone(),
            version: args.version.clone(),
            location: args.location.clone(),
        },
        tags,
    ))
}

/// Runs the flags' mode against `api` and `writers`, returning the
/// termination log line.
///
/// # Errors
/// [`PublishError`], already in the shape [`error_line`] writes.
pub async fn execute(
    args: &PublishArgs,
    api: &dyn PublishApi,
    writers: &dyn PageWriterFactory,
    timings: &Timings,
) -> Result<String, PublishError> {
    let (target, tags) = target_from(args)?;
    match args.mode {
        Mode::Publish => {
            let source = required(args.source.as_ref(), "--source")?.clone();
            let checksum = args.checksum.as_deref().ok_or_else(|| {
                PublishError::InvalidArgument("--checksum is required to publish".to_string())
            })?;
            let plan = PublishPlan {
                target,
                trusted_launch: args.trusted_launch,
                tags,
                source,
                checksum_hex: parse_checksum(checksum)?,
            };
            publish(api, writers, &plan, timings)
                .await
                .map(|p| published_line(&p))
        }
        Mode::Delete => {
            let plan = DeletePlan {
                target,
                owner_uid: args.owner_uid.clone(),
            };
            unpublish(api, &plan, timings)
                .await
                .map(|()| deleted_line())
        }
    }
}

/// Writes `line` to the termination log; a failure to write is logged,
/// not fatal (the exit code still says what happened).
pub fn write_termination_log(path: &Path, line: &str) {
    if let Err(e) = std::fs::write(path, format!("{line}\n")) {
        warn!(path = %path.display(), error = %e, "cannot write the termination log");
    }
}

fn build_client(args: &PublishArgs) -> Result<(ArmClient, HttpPageWriterFactory), PublishError> {
    let cloud = resolve_cloud(&args.endpoint, args.authority_host.as_deref())
        .map_err(|e| PublishError::InvalidArgument(e.to_string()))?;
    let federated = FederatedConfig {
        authority: cloud.authority_host.to_string(),
        tenant_id: args.tenant_id.clone(),
        client_id: args.client_id.clone(),
        token_file: args.federated_token_file.clone(),
        scope: cloud.arm_scope(),
    };
    let mut arm = ArmConfig::new(&args.subscription_id);
    arm.endpoint = cloud.arm_endpoint.to_string();
    let token = Arc::new(FederatedToken::new(federated)?);
    let client = ArmClient::new(arm, token)?;
    Ok((
        client,
        HttpPageWriterFactory::new(upload_host_suffix(cloud)),
    ))
}

/// Runs one publish or delete to completion and writes the termination
/// log. The caller has installed the crypto provider and tracing.
///
/// # Errors
/// Any [`PublishError`], so the Job's container exits non-zero.
pub async fn run(args: PublishArgs) -> anyhow::Result<()> {
    let timings = Timings::default();
    let result = match build_client(&args) {
        Ok((client, writers)) => execute(&args, &client, &writers, &timings).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(line) => {
            write_termination_log(&args.termination_log, &line);
            info!(mode = ?args.mode, "image-publish finished");
            Ok(())
        }
        Err(e) => {
            write_termination_log(&args.termination_log, &error_line(&e));
            Err(anyhow::anyhow!(
                "image-publish failed ({}): {}",
                e.code(),
                redact(&e.to_string())
            ))
        }
    }
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod publish_tests;
