// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Publishing into an Azure Compute Gallery (ADR-0162).
//!
//! The requests the publish Job sends and the facts it reads back: the image
//! definition, the upload disk, its snapshot and the image version. Every
//! name is deterministic (ADR-0162 Decision 3, amended 2026-11-26), so a
//! restarted Job, or a second Job for the same digest, finds the step in
//! flight by name instead of starting another.
//!
//! What the roadmap 23 spike found, and this module encodes:
//!
//! - NVMe must be declared twice: on the image definition
//!   (`DiskControllerTypes`) and on the source (the upload disk's
//!   `supportedCapabilities.diskControllerTypes`, which its snapshot
//!   inherits). With only the first, the version create fails.
//! - Every version is created with `blockDeletionBeforeEndOfLife: false`
//!   and no implicit end-of-life, so banlieue can delete what it published.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::tags::validate_tags;

/// Azure's limit on a gallery image definition name.
pub const MAX_IMAGE_DEFINITION_NAME_LEN: usize = 80;
/// The limit banlieue keeps an image identifier's offer to.
pub const MAX_OFFER_LEN: usize = 64;
/// Suffix of a Trusted Launch definition's name: the security type is an
/// immutable feature, so each gets its own definition.
pub const TRUSTED_LAUNCH_SUFFIX: &str = "-tl";
/// The identifier's publisher on every definition banlieue creates.
pub const IMAGE_PUBLISHER: &str = "banlieue";
/// The identifier's SKU on a Trusted Launch definition.
pub const SKU_TRUSTED_LAUNCH: &str = "trustedlaunch";
/// The identifier's SKU on a standard definition.
pub const SKU_STANDARD: &str = "standard";
/// The controllers every published image declares (ADR-0162 finding 3).
pub const DISK_CONTROLLER_TYPES: &str = "SCSI, NVMe";
/// The image definition feature naming the disk controllers.
pub const FEATURE_DISK_CONTROLLER_TYPES: &str = "DiskControllerTypes";
/// The image definition feature naming the security type.
pub const FEATURE_SECURITY_TYPE: &str = "SecurityType";
/// [`FEATURE_SECURITY_TYPE`]'s value for Trusted Launch.
pub const SECURITY_TYPE_TRUSTED_LAUNCH: &str = "TrustedLaunch";
/// Generation 2 (UEFI): banlieue's raw disks boot through an EFI partition.
pub const HYPER_V_GENERATION: &str = "V2";
/// The OS type of every banlieue image.
pub const OS_TYPE_LINUX: &str = "Linux";
/// A generalized image: each VM gets its own identity at first boot.
pub const OS_STATE_GENERALIZED: &str = "Generalized";
/// The storage SKU of the temporaries and the replica: the cheapest.
pub const STANDARD_LRS: &str = "Standard_LRS";
/// One replica in the one target region.
pub const REPLICA_COUNT: u32 = 1;
/// How long a write SAS for the upload disk is valid, in seconds.
pub const UPLOAD_ACCESS_SECONDS: u32 = 3600;

/// `provisioningState` of a finished resource.
pub const PROVISIONING_SUCCEEDED: &str = "Succeeded";
/// `provisioningState` of a resource being created.
pub const PROVISIONING_CREATING: &str = "Creating";
/// `provisioningState` of a resource being updated.
pub const PROVISIONING_UPDATING: &str = "Updating";
/// `provisioningState` of a resource whose create failed.
pub const PROVISIONING_FAILED: &str = "Failed";
/// `provisioningState` of a resource being deleted.
pub const PROVISIONING_DELETING: &str = "Deleting";

/// `diskState` of an upload disk not opened for writing yet.
pub const DISK_STATE_READY_TO_UPLOAD: &str = "ReadyToUpload";
/// `diskState` of an upload disk with an active write SAS.
pub const DISK_STATE_ACTIVE_UPLOAD: &str = "ActiveUpload";
/// `diskState` of a disk after its write access was revoked: the upload is
/// final.
pub const DISK_STATE_UNATTACHED: &str = "Unattached";

/// Hex characters of a digest used to shorten an over-long name.
const NAME_HASH_HEX_LEN: usize = 8;
/// Hex characters of the digest naming a publish's temporaries.
const TEMP_HASH_HEX_LEN: usize = 16;
/// Prefix of the upload disk's and snapshot's names.
const TEMP_PREFIX: &str = "banlieue-publish-";
const UPLOAD_DISK_SUFFIX: &str = "-disk";
const SNAPSHOT_SUFFIX: &str = "-snap";
/// Fallback when a name sanitises to nothing.
const EMPTY_NAME_FALLBACK: &str = "image";
/// Version names hold the digest's first eight bytes, two 31-bit numbers.
const VERSION_MAJOR: u32 = 1;
const VERSION_PART_MASK: u32 = 0x7fff_ffff;
const DIGEST_PREFIX_HEX_LEN: usize = 16;
const HALF_WORD: usize = 4;
const HEX_RADIX: u32 = 16;
const HEX_DIGITS_PER_BYTE: usize = 2;
const CODE_INVALID_NAME: &str = "InvalidName";

fn sha256(input: &[u8]) -> Vec<u8> {
    Sha256::digest(input).to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(HEX_DIGITS_PER_BYTE) {
        return None;
    }
    s.as_bytes()
        .chunks(HEX_DIGITS_PER_BYTE)
        .map(|pair| {
            let hi = char::from(pair[0]).to_digit(HEX_RADIX)?;
            let lo = char::from(pair[1]).to_digit(HEX_RADIX)?;
            u8::try_from(hi * HEX_RADIX + lo).ok()
        })
        .collect()
}

/// The image version name for a source digest: `1.<a>.<b>`, where `a` and
/// `b` are the digest's first and second four bytes (big-endian), each
/// masked to 31 bits so they fit ARM's 32-bit signed version parts. The
/// same digest always yields the same name (ADR-0162 Decision 3, amended).
///
/// A value that is not at least 16 hex characters is hashed first, so the
/// result is always a valid name.
#[must_use]
pub fn gallery_version_name(source_sha256_hex: &str) -> String {
    let prefix = source_sha256_hex
        .get(..DIGEST_PREFIX_HEX_LEN)
        .and_then(decode_hex)
        .unwrap_or_else(|| sha256(source_sha256_hex.as_bytes()));
    let word = |at: usize| {
        let mut b = [0u8; HALF_WORD];
        b.copy_from_slice(&prefix[at..at + HALF_WORD]);
        u32::from_be_bytes(b) & VERSION_PART_MASK
    };
    format!("{VERSION_MAJOR}.{}.{}", word(0), word(HALF_WORD))
}

/// Whether `name` is `int.int.int` with each part a non-negative 32-bit
/// signed integer: the only shape ARM accepts for an image version.
#[must_use]
pub fn is_valid_version_name(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && p.parse::<i32>().is_ok()
        })
}

/// Whether `name` is a valid gallery image definition name: 1 to 80
/// characters of letters, digits, `-`, `_` and `.`, starting and ending
/// with a letter or digit.
#[must_use]
pub fn is_valid_image_definition_name(name: &str) -> bool {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    !name.is_empty()
        && name.len() <= MAX_IMAGE_DEFINITION_NAME_LEN
        && name.chars().all(allowed)
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name.ends_with(|c: char| c.is_ascii_alphanumeric())
}

/// `raw` with anything but letters, digits, `-`, `_` and `.` replaced by
/// `-`, trimmed to start and end on a letter or digit.
fn sanitize(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if trimmed.is_empty() {
        return EMPTY_NAME_FALLBACK.to_string();
    }
    trimmed.to_string()
}

/// `base` cut to `max` characters; when cut, the tail is replaced by a
/// digest of `original`, so two long names that share a prefix stay apart.
fn fit(base: &str, max: usize, original: &str) -> String {
    if base.len() <= max {
        return base.to_string();
    }
    let digest = hex(&sha256(original.as_bytes()));
    let keep = max - NAME_HASH_HEX_LEN - 1;
    let head = base[..keep].trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    format!("{head}-{}", &digest[..NAME_HASH_HEX_LEN])
}

/// The image definition for a `VMImage` and security type: the `VMImage`'s
/// name, made valid, with [`TRUSTED_LAUNCH_SUFFIX`] for Trusted Launch.
/// Deterministic, and always valid ([`is_valid_image_definition_name`]):
/// a name too long is shortened with a digest of the original.
#[must_use]
pub fn image_definition_name(vmimage_name: &str, trusted_launch: bool) -> String {
    let suffix = if trusted_launch {
        TRUSTED_LAUNCH_SUFFIX
    } else {
        ""
    };
    let base = fit(
        &sanitize(vmimage_name),
        MAX_IMAGE_DEFINITION_NAME_LEN - suffix.len(),
        vmimage_name,
    );
    format!("{base}{suffix}")
}

/// The identifier's offer for a definition: its name without the security
/// suffix (the `VMImage`'s name as [`image_definition_name`] made it), kept
/// to [`MAX_OFFER_LEN`]. `(publisher, offer, sku)` is unique in a gallery
/// because the definition name is.
#[must_use]
pub fn image_offer(definition_name: &str, trusted_launch: bool) -> String {
    let base = if trusted_launch {
        definition_name
            .strip_suffix(TRUSTED_LAUNCH_SUFFIX)
            .unwrap_or(definition_name)
    } else {
        definition_name
    };
    fit(&sanitize(base), MAX_OFFER_LEN, base)
}

/// The identifier's SKU for a security type.
#[must_use]
pub fn image_sku(trusted_launch: bool) -> &'static str {
    if trusted_launch {
        SKU_TRUSTED_LAUNCH
    } else {
        SKU_STANDARD
    }
}

fn temp_base(gallery: &str, definition: &str, version: &str) -> String {
    let digest = hex(&sha256(
        format!("{gallery}/{definition}/{version}").as_bytes(),
    ));
    format!("{TEMP_PREFIX}{}", &digest[..TEMP_HASH_HEX_LEN])
}

/// The upload disk's name for publishing `version` of `definition` in
/// `gallery`: deterministic, so a restarted Job finds it.
#[must_use]
pub fn upload_disk_name(gallery: &str, definition: &str, version: &str) -> String {
    format!(
        "{}{UPLOAD_DISK_SUFFIX}",
        temp_base(gallery, definition, version)
    )
}

/// The snapshot's name, as [`upload_disk_name`].
#[must_use]
pub fn snapshot_name(gallery: &str, definition: &str, version: &str) -> String {
    format!(
        "{}{SNAPSHOT_SUFFIX}",
        temp_base(gallery, definition, version)
    )
}

/// `PUT .../galleries/{g}/images/{def}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageDefinitionRequest {
    /// The gallery's region.
    pub location: String,
    /// The identifier's offer ([`image_offer`]).
    pub offer: String,
    /// Whether the definition is Trusted Launch.
    pub trusted_launch: bool,
    /// Ownership tags.
    pub tags: BTreeMap<String, String>,
}

impl ImageDefinitionRequest {
    /// The features this definition declares: always the disk controllers,
    /// and the security type for Trusted Launch.
    #[must_use]
    pub fn features(&self) -> BTreeMap<String, String> {
        let mut features = BTreeMap::from([(
            FEATURE_DISK_CONTROLLER_TYPES.to_string(),
            DISK_CONTROLLER_TYPES.to_string(),
        )]);
        if self.trusted_launch {
            features.insert(
                FEATURE_SECURITY_TYPE.to_string(),
                SECURITY_TYPE_TRUSTED_LAUNCH.to_string(),
            );
        }
        features
    }

    /// The request body.
    ///
    /// # Errors
    /// [`Error::InvalidTag`] for a tag Azure would refuse.
    pub fn to_body(&self) -> Result<Value> {
        validate_tags(&self.tags)?;
        let features: Vec<Value> = self
            .features()
            .into_iter()
            .map(|(name, value)| json!({ "name": name, "value": value }))
            .collect();
        Ok(json!({
            "location": self.location,
            "tags": self.tags,
            "properties": {
                "osType": OS_TYPE_LINUX,
                "osState": OS_STATE_GENERALIZED,
                "hyperVGeneration": HYPER_V_GENERATION,
                "identifier": {
                    "publisher": IMAGE_PUBLISHER,
                    "offer": self.offer,
                    "sku": image_sku(self.trusted_launch),
                },
                "features": features,
            }
        }))
    }
}

/// `PUT .../disks/{name}` with `createOption: Upload`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadDiskRequest {
    /// Region.
    pub location: String,
    /// The VHD's whole length, footer included.
    pub upload_size_bytes: u64,
    /// Ownership tags.
    pub tags: BTreeMap<String, String>,
}

impl UploadDiskRequest {
    /// The request body. The disk declares both controllers so the
    /// snapshot inherits them (ADR-0162 finding 3). No `securityProfile`:
    /// the spike published Trusted Launch versions from upload disks
    /// without one; Trusted Launch is the definition's feature.
    ///
    /// # Errors
    /// [`Error::InvalidTag`] for a tag Azure would refuse.
    pub fn to_body(&self) -> Result<Value> {
        validate_tags(&self.tags)?;
        Ok(json!({
            "location": self.location,
            "tags": self.tags,
            "sku": { "name": STANDARD_LRS },
            "properties": {
                "osType": OS_TYPE_LINUX,
                "hyperVGeneration": HYPER_V_GENERATION,
                "creationData": {
                    "createOption": "Upload",
                    "uploadSizeBytes": self.upload_size_bytes,
                },
                "supportedCapabilities": { "diskControllerTypes": DISK_CONTROLLER_TYPES },
            }
        }))
    }
}

/// `PUT .../snapshots/{name}`, a full copy of the upload disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotRequest {
    /// Region.
    pub location: String,
    /// The upload disk's resource ID.
    pub source_disk_id: String,
    /// Ownership tags.
    pub tags: BTreeMap<String, String>,
}

impl SnapshotRequest {
    /// The request body; declares both controllers, as the disk does.
    ///
    /// # Errors
    /// [`Error::InvalidTag`] for a tag Azure would refuse.
    pub fn to_body(&self) -> Result<Value> {
        validate_tags(&self.tags)?;
        Ok(json!({
            "location": self.location,
            "tags": self.tags,
            "sku": { "name": STANDARD_LRS },
            "properties": {
                "osType": OS_TYPE_LINUX,
                "hyperVGeneration": HYPER_V_GENERATION,
                "creationData": {
                    "createOption": "Copy",
                    "sourceResourceId": self.source_disk_id,
                },
                "incremental": false,
                "supportedCapabilities": { "diskControllerTypes": DISK_CONTROLLER_TYPES },
            }
        }))
    }
}

/// `PUT .../galleries/{g}/images/{def}/versions/{ver}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GalleryImageVersionRequest {
    /// Region, and the one target region.
    pub location: String,
    /// The snapshot's resource ID.
    pub snapshot_id: String,
    /// An end-of-life date (`YYYY-MM-DD`), or none.
    pub end_of_life: Option<String>,
    /// Ownership tags and [`crate::tags::TAG_SOURCE_SHA256`].
    pub tags: BTreeMap<String, String>,
}

impl GalleryImageVersionRequest {
    /// The request body: one replica in the one region, no implicit
    /// end-of-life, deletion never blocked (ADR-0162 finding 5).
    ///
    /// # Errors
    /// [`Error::InvalidTag`] for a tag Azure would refuse.
    pub fn to_body(&self) -> Result<Value> {
        validate_tags(&self.tags)?;
        let mut publishing = json!({
            "targetRegions": [{
                "name": self.location,
                "regionalReplicaCount": REPLICA_COUNT,
                "storageAccountType": STANDARD_LRS,
            }],
            "replicaCount": REPLICA_COUNT,
            "excludeFromLatest": false,
        });
        if let Some(date) = &self.end_of_life {
            publishing["endOfLifeDate"] = json!(date);
        }
        Ok(json!({
            "location": self.location,
            "tags": self.tags,
            "properties": {
                "publishingProfile": publishing,
                "storageProfile": {
                    "osDiskImage": { "source": { "id": self.snapshot_id } }
                },
                "safetyProfile": {
                    "allowDeletionOfReplicatedLocations": true,
                    "blockDeletionBeforeEndOfLife": false,
                },
            }
        }))
    }
}

/// An image version as ARM reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GalleryImageVersion {
    /// Resource ID: what a VM's `imageReference.id` names.
    pub id: String,
    /// `Creating`, `Succeeded`, `Failed`, ...
    pub provisioning_state: String,
    /// Tags.
    pub tags: BTreeMap<String, String>,
}

/// An image definition as ARM reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GalleryImageDefinition {
    /// Resource ID.
    pub id: String,
    /// Tags.
    pub tags: BTreeMap<String, String>,
    /// Features by name.
    pub features: BTreeMap<String, String>,
}

/// A managed disk as ARM reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedDisk {
    /// Resource ID.
    pub id: String,
    /// `ReadyToUpload`, `ActiveUpload`, `Unattached`, ...
    pub disk_state: String,
    /// `creationData.uploadSizeBytes`, for an upload disk.
    pub upload_size_bytes: Option<u64>,
    /// `supportedCapabilities.diskControllerTypes`.
    pub disk_controller_types: Option<String>,
    /// Tags.
    pub tags: BTreeMap<String, String>,
}

/// A snapshot as ARM reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Resource ID.
    pub id: String,
    /// `Creating`, `Succeeded`, ...
    pub provisioning_state: String,
    /// Tags.
    pub tags: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Resource {
    #[serde(default)]
    id: String,
    #[serde(default)]
    tags: Option<BTreeMap<String, String>>,
    #[serde(default)]
    properties: Option<Props>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Props {
    provisioning_state: Option<String>,
    disk_state: Option<String>,
    creation_data: Option<CreationData>,
    supported_capabilities: Option<Capabilities>,
    #[serde(default)]
    features: Vec<Feature>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreationData {
    upload_size_bytes: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    disk_controller_types: Option<String>,
}

#[derive(Deserialize)]
struct Feature {
    name: String,
    value: String,
}

fn parse_resource(body: &[u8], what: &str) -> Result<(String, BTreeMap<String, String>, Props)> {
    let r: Resource =
        serde_json::from_slice(body).map_err(|e| Error::Decode(format!("{what}: {e}")))?;
    if r.id.is_empty() {
        return Err(Error::Decode(format!("{what} without an id")));
    }
    Ok((
        r.id,
        r.tags.unwrap_or_default(),
        r.properties.unwrap_or_default(),
    ))
}

/// Parses `GET .../versions/{ver}`.
///
/// # Errors
/// [`Error::Decode`] for a body that is not an image version.
pub fn parse_gallery_image_version(body: &[u8]) -> Result<GalleryImageVersion> {
    let (id, tags, props) = parse_resource(body, "image version")?;
    Ok(GalleryImageVersion {
        id,
        provisioning_state: props.provisioning_state.unwrap_or_default(),
        tags,
    })
}

/// Parses a list of image versions (`{"value":[...]}`).
///
/// # Errors
/// [`Error::Decode`] for a body that is not such a list.
pub fn parse_gallery_image_versions(body: &[u8]) -> Result<Vec<GalleryImageVersion>> {
    #[derive(Deserialize)]
    struct List {
        #[serde(default)]
        value: Vec<Value>,
    }
    let list: List = serde_json::from_slice(body)
        .map_err(|e| Error::Decode(format!("image version list: {e}")))?;
    list.value
        .iter()
        .map(|v| parse_gallery_image_version(v.to_string().as_bytes()))
        .collect()
}

/// Parses `GET .../images/{def}`.
///
/// # Errors
/// [`Error::Decode`] for a body that is not an image definition.
pub fn parse_gallery_image_definition(body: &[u8]) -> Result<GalleryImageDefinition> {
    let (id, tags, props) = parse_resource(body, "image definition")?;
    Ok(GalleryImageDefinition {
        id,
        tags,
        features: props
            .features
            .into_iter()
            .map(|f| (f.name, f.value))
            .collect(),
    })
}

/// Parses `GET .../disks/{name}`.
///
/// # Errors
/// [`Error::Decode`] for a body that is not a disk.
pub fn parse_managed_disk(body: &[u8]) -> Result<ManagedDisk> {
    let (id, tags, props) = parse_resource(body, "disk")?;
    Ok(ManagedDisk {
        id,
        disk_state: props.disk_state.unwrap_or_default(),
        upload_size_bytes: props.creation_data.and_then(|c| c.upload_size_bytes),
        disk_controller_types: props
            .supported_capabilities
            .and_then(|c| c.disk_controller_types),
        tags,
    })
}

/// Parses `GET .../snapshots/{name}`.
///
/// # Errors
/// [`Error::Decode`] for a body that is not a snapshot.
pub fn parse_snapshot(body: &[u8]) -> Result<Snapshot> {
    let (id, tags, props) = parse_resource(body, "snapshot")?;
    Ok(Snapshot {
        id,
        provisioning_state: props.provisioning_state.unwrap_or_default(),
        tags,
    })
}

/// The write SAS from a `beginGetAccess` result: `accessSAS` at the top
/// (the final resource of a `Location` operation, or a synchronous 200) or
/// under `properties.output` (an `Azure-AsyncOperation` status body).
///
/// # Errors
/// [`Error::Decode`] when there is none. The message never includes the
/// body, which may hold a SAS.
pub fn parse_access_sas(body: &[u8]) -> Result<String> {
    const ACCESS_SAS: &str = "accessSAS";
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| Error::Decode("disk access result is not JSON".to_string()))?;
    value
        .get(ACCESS_SAS)
        .or_else(|| value.pointer("/properties/output/accessSAS"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| Error::Decode("disk access result without accessSAS".to_string()))
}

/// Whether a declared controller list (`"SCSI, NVMe"`) includes NVMe.
#[must_use]
pub fn declares_nvme(controllers: &str) -> bool {
    controllers
        .split(',')
        .any(|c| c.trim().eq_ignore_ascii_case("NVMe"))
}

/// Refuses a name ARM would refuse as an image version.
///
/// # Errors
/// [`Error::InvalidRequest`].
pub fn check_version_name(name: &str) -> Result<()> {
    if is_valid_version_name(name) {
        return Ok(());
    }
    Err(Error::InvalidRequest {
        code: CODE_INVALID_NAME.to_string(),
        message: format!(
            "image version name {name:?} must be major.minor.patch with 32-bit integers"
        ),
    })
}

#[cfg(test)]
#[path = "gallery_tests.rs"]
mod gallery_tests;
