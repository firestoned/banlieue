// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! What EC2 returns, read into the facts the provider decides on
//! (ADR-0174 Decision 2).
//!
//! EC2 answers in XML, wraps every list in `<item>` elements, and adds
//! elements over time ("the order of the elements in the response ... might
//! vary"). Every reader here takes only the elements it needs, ignores the
//! rest, and tolerates an absent optional element:
//!
//! - [`parse_run_instances`]: the new instance's ID.
//! - [`parse_describe_instances`]: state, private address and DNS name,
//!   zone, launch time and tags, per instance.
//! - [`parse_instance_state_changes`]: the state after
//!   `TerminateInstances`, `StopInstances` or `StartInstances`.
//! - [`parse_console_output`]: the serial console, decoded, for
//!   [`crate::console`].
//! - [`parse_tpm_ek_pub`] and [`to_pem_spki`]: the NitroTPM EK public key
//!   (ADR-0167).
//! - [`parse_availability_zones`], [`parse_instance_type_offerings`],
//!   [`parse_instance_types`]: what the `Provider` publishes.
//! - [`parse_describe_images`]: an AMI's state, architecture, root device,
//!   boot mode and TPM support (roadmap 24 phase E). The owner account is
//!   not read.
//! - [`parse_next_token`]: a paginated response's `nextToken`.
//! - [`parse_error`]: EC2's, STS's or S3's error body, classified.
//!
//! The readers take the body as text; a caller holding bytes converts with
//! `String::from_utf8_lossy` (AWS answers in UTF-8).

use std::collections::BTreeMap;

use base64::Engine as _;
use jiff::Timestamp;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::{AwsFault, Error, Result, classify};
use crate::instance::EkKeyType;

const PEM_BEGIN: &str = "-----BEGIN PUBLIC KEY-----";
const PEM_END: &str = "-----END PUBLIC KEY-----";
/// RFC 7468: PEM base64 lines are 64 characters.
const PEM_LINE_LEN: usize = 64;
/// `nitroTpmSupport` for a type with NitroTPM.
const NITRO_TPM_SUPPORTED: &str = "supported";
const BOOT_MODE_UEFI: &str = "uefi";

const STATE_PENDING: &str = "pending";
const STATE_RUNNING: &str = "running";
const STATE_SHUTTING_DOWN: &str = "shutting-down";
const STATE_TERMINATED: &str = "terminated";
const STATE_STOPPING: &str = "stopping";
const STATE_STOPPED: &str = "stopped";

fn decode<T: DeserializeOwned>(what: &str, body: &str) -> Result<T> {
    quick_xml::de::from_str(body).map_err(|e| Error::Decode(format!("{what}: {e}")))
}

/// EC2's list wrapper: `<listName><item>...</item>...</listName>`.
#[derive(Deserialize)]
struct Items<T> {
    #[serde(rename = "item", default = "Vec::new")]
    item: Vec<T>,
}

impl<T> Default for Items<T> {
    fn default() -> Self {
        Self { item: Vec::new() }
    }
}

// ----- instances ---------------------------------------------------------

/// An instance's lifecycle state (`instanceState/name`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstanceState {
    /// `pending`: launching or starting.
    Pending,
    /// `running`.
    Running,
    /// `shutting-down`: terminating.
    ShuttingDown,
    /// `terminated`: gone, still listed for about an hour.
    Terminated,
    /// `stopping`.
    Stopping,
    /// `stopped`.
    Stopped,
    /// A name EC2 does not document (yet), kept as given.
    Other(String),
}

impl InstanceState {
    /// The state for EC2's name.
    #[must_use]
    pub fn parse(name: &str) -> Self {
        match name {
            STATE_PENDING => Self::Pending,
            STATE_RUNNING => Self::Running,
            STATE_SHUTTING_DOWN => Self::ShuttingDown,
            STATE_TERMINATED => Self::Terminated,
            STATE_STOPPING => Self::Stopping,
            STATE_STOPPED => Self::Stopped,
            other => Self::Other(other.to_string()),
        }
    }

    /// EC2's name for the state.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Pending => STATE_PENDING,
            Self::Running => STATE_RUNNING,
            Self::ShuttingDown => STATE_SHUTTING_DOWN,
            Self::Terminated => STATE_TERMINATED,
            Self::Stopping => STATE_STOPPING,
            Self::Stopped => STATE_STOPPED,
            Self::Other(name) => name,
        }
    }

    /// True once the instance is terminating or terminated: it will not
    /// come back, and a create must not adopt it.
    #[must_use]
    pub fn is_gone(&self) -> bool {
        matches!(self, Self::ShuttingDown | Self::Terminated)
    }
}

/// What the provider needs of one instance from `DescribeInstances`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    /// Instance ID, such as `i-0123456789abcdef0`.
    pub instance_id: String,
    /// Lifecycle state.
    pub state: InstanceState,
    /// Primary private IPv4 (`InternalIP`); absent while pending and once
    /// terminated.
    pub private_ip: Option<String>,
    /// Private DNS name (`InternalDNS`); absent or empty while pending.
    pub private_dns_name: Option<String>,
    /// Zone name (`placement/availabilityZone`); empty if EC2 left it out.
    pub zone_name: String,
    /// When the instance was last launched or started.
    pub launch_time: Option<Timestamp>,
    /// The instance's tags.
    pub tags: BTreeMap<String, String>,
}

/// An instance's state change from `TerminateInstances`, `StopInstances`
/// or `StartInstances`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceStateChange {
    /// Instance ID.
    pub instance_id: String,
    /// The state after the call.
    pub current: InstanceState,
    /// The state before the call.
    pub previous: InstanceState,
}

#[derive(Deserialize, Default)]
struct RawState {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Default)]
struct RawPlacement {
    #[serde(rename = "availabilityZone", default)]
    availability_zone: String,
}

#[derive(Deserialize)]
struct RawTag {
    #[serde(default)]
    key: String,
    #[serde(default)]
    value: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawInstance {
    #[serde(default)]
    instance_id: String,
    #[serde(default)]
    instance_state: RawState,
    #[serde(default)]
    private_ip_address: Option<String>,
    #[serde(default)]
    private_dns_name: Option<String>,
    #[serde(default)]
    placement: RawPlacement,
    #[serde(default)]
    launch_time: Option<String>,
    #[serde(default)]
    tag_set: Items<RawTag>,
}

impl RawInstance {
    fn into_instance(self) -> Result<Instance> {
        if self.instance_id.is_empty() {
            return Err(Error::Decode("an instance has no instanceId".to_string()));
        }
        let launch_time = match self.launch_time.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(text) => Some(text.parse::<Timestamp>().map_err(|e| {
                Error::Decode(format!("launchTime {text:?} of {}: {e}", self.instance_id))
            })?),
        };
        Ok(Instance {
            instance_id: self.instance_id,
            state: InstanceState::parse(&self.instance_state.name),
            private_ip: non_empty(self.private_ip_address),
            private_dns_name: non_empty(self.private_dns_name),
            zone_name: self.placement.availability_zone,
            launch_time,
            tags: self
                .tag_set
                .item
                .into_iter()
                .map(|t| (t.key, t.value))
                .collect(),
        })
    }
}

#[derive(Deserialize)]
struct RawInstancesResponse {
    #[serde(rename = "instancesSet", default)]
    instances_set: Items<RawInstance>,
}

#[derive(Deserialize)]
struct RawDescribeInstances {
    #[serde(rename = "reservationSet", default)]
    reservation_set: Items<RawInstancesResponse>,
}

/// The instance ID from a `RunInstances` response.
///
/// # Errors
/// [`Error::Decode`] for malformed XML or a response with no instance.
pub fn parse_run_instances(body: &str) -> Result<String> {
    let raw: RawInstancesResponse = decode("RunInstances", body)?;
    raw.instances_set
        .item
        .into_iter()
        .map(|i| i.instance_id)
        .find(|id| !id.is_empty())
        .ok_or_else(|| Error::Decode("RunInstances returned no instance".to_string()))
}

/// Every instance in a `DescribeInstances` response, across reservations.
///
/// # Errors
/// [`Error::Decode`] for malformed XML, an instance with no ID, or a
/// launch time that is not RFC 3339.
pub fn parse_describe_instances(body: &str) -> Result<Vec<Instance>> {
    let raw: RawDescribeInstances = decode("DescribeInstances", body)?;
    raw.reservation_set
        .item
        .into_iter()
        .flat_map(|r| r.instances_set.item)
        .map(RawInstance::into_instance)
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStateChange {
    #[serde(default)]
    instance_id: String,
    #[serde(default)]
    current_state: RawState,
    #[serde(default)]
    previous_state: RawState,
}

#[derive(Deserialize)]
struct RawStateChanges {
    #[serde(rename = "instancesSet", default)]
    instances_set: Items<RawStateChange>,
}

/// The state changes in a `TerminateInstances`, `StopInstances` or
/// `StartInstances` response.
///
/// # Errors
/// [`Error::Decode`] for malformed XML.
pub fn parse_instance_state_changes(body: &str) -> Result<Vec<InstanceStateChange>> {
    let raw: RawStateChanges = decode("instance state change", body)?;
    Ok(raw
        .instances_set
        .item
        .into_iter()
        .map(|c| InstanceStateChange {
            instance_id: c.instance_id,
            current: InstanceState::parse(&c.current_state.name),
            previous: InstanceState::parse(&c.previous_state.name),
        })
        .collect())
}

// ----- console -----------------------------------------------------------

#[derive(Deserialize)]
struct RawConsoleOutput {
    #[serde(default)]
    output: Option<String>,
}

/// The decoded console text of a `GetConsoleOutput` response; empty when
/// EC2 has none yet (no `<output>`). Bytes that are not UTF-8 are replaced,
/// since a console carries whatever the firmware printed.
///
/// # Errors
/// [`Error::Decode`] for malformed XML or an `<output>` that is not base64.
pub fn parse_console_output(body: &str) -> Result<String> {
    let raw: RawConsoleOutput = decode("GetConsoleOutput", body)?;
    let Some(encoded) = raw.output else {
        return Ok(String::new());
    };
    let bytes = decode_base64("console output", &encoded)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

// ----- NitroTPM EK -------------------------------------------------------

/// The NitroTPM endorsement key from `GetInstanceTpmEkPub` (ADR-0167).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TpmEkPub {
    /// Instance ID.
    pub instance_id: String,
    /// RSA-2048 or ECC P-384.
    pub key_type: EkKeyType,
    /// `der` (what banlieue asks for) or `tpmt`.
    pub key_format: String,
    /// The key, base64-decoded. For `der`, a DER `SubjectPublicKeyInfo`
    /// ([`to_pem_spki`]).
    pub key_value: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTpmEkPub {
    #[serde(default)]
    instance_id: String,
    #[serde(default)]
    key_type: String,
    #[serde(default)]
    key_format: String,
    #[serde(default)]
    key_value: String,
}

/// The EK of a `GetInstanceTpmEkPub` response.
///
/// # Errors
/// [`Error::Decode`] for malformed XML, a key type EC2 does not document,
/// or a key that is empty or not base64.
pub fn parse_tpm_ek_pub(body: &str) -> Result<TpmEkPub> {
    let raw: RawTpmEkPub = decode("GetInstanceTpmEkPub", body)?;
    let key_type = EkKeyType::parse(raw.key_type.trim()).ok_or_else(|| {
        Error::Decode(format!(
            "GetInstanceTpmEkPub: unknown keyType {:?}",
            raw.key_type
        ))
    })?;
    let key_value = decode_base64("keyValue", &raw.key_value)?;
    if key_value.is_empty() {
        return Err(Error::Decode(
            "GetInstanceTpmEkPub returned an empty key".to_string(),
        ));
    }
    Ok(TpmEkPub {
        instance_id: raw.instance_id,
        key_type,
        key_format: raw.key_format.trim().to_string(),
        key_value,
    })
}

/// `der` as a PEM `PUBLIC KEY` block, 64 characters a line, the form
/// ADR-0167 publishes in `tpmEndorsementPublicKeys`.
///
/// AWS documents the `der` format as "a DER-encoded public key that is
/// compatible with OpenSSL" (GetInstanceTpmEkPub, `KeyFormat`). OpenSSL's
/// DER public key is the X.509 `SubjectPublicKeyInfo` (`i2d_PUBKEY`), which
/// is exactly what a `PUBLIC KEY` PEM wraps; the reference does not name
/// SPKI itself. The roadmap 24 spike found the API's DER equal, byte for
/// byte, to the key the guest's TPM reports (ADR-0167 finding 2). The bytes
/// are wrapped as given, not parsed.
#[must_use]
pub fn to_pem_spki(der: &[u8]) -> String {
    let encoded: Vec<char> = base64::engine::general_purpose::STANDARD
        .encode(der)
        .chars()
        .collect();
    let mut pem = String::new();
    pem.push_str(PEM_BEGIN);
    pem.push('\n');
    for line in encoded.chunks(PEM_LINE_LEN) {
        pem.extend(line);
        pem.push('\n');
    }
    pem.push_str(PEM_END);
    pem.push('\n');
    pem
}

// ----- zones and instance types ------------------------------------------

/// A zone from `DescribeAvailabilityZones`: the account's name for it and
/// the global ID (ADR-0174 finding 8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailabilityZone {
    /// Zone name in this account, such as `ca-central-1a`.
    pub name: String,
    /// Zone ID, the same in every account, such as `cac1-az1`.
    pub id: String,
    /// `available`, `information`, `impaired`, `unavailable` or
    /// `constrained`.
    pub state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawZone {
    #[serde(default)]
    zone_name: String,
    #[serde(default)]
    zone_id: String,
    #[serde(default)]
    zone_state: String,
}

#[derive(Deserialize)]
struct RawZones {
    #[serde(rename = "availabilityZoneInfo", default)]
    availability_zone_info: Items<RawZone>,
}

/// The zones of a `DescribeAvailabilityZones` response.
///
/// # Errors
/// [`Error::Decode`] for malformed XML.
pub fn parse_availability_zones(body: &str) -> Result<Vec<AvailabilityZone>> {
    let raw: RawZones = decode("DescribeAvailabilityZones", body)?;
    Ok(raw
        .availability_zone_info
        .item
        .into_iter()
        .map(|z| AvailabilityZone {
            name: z.zone_name,
            id: z.zone_id,
            state: z.zone_state,
        })
        .collect())
}

/// An instance type offered at a location, from
/// `DescribeInstanceTypeOfferings`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceTypeOffering {
    /// Instance type, such as `m6i.large`.
    pub instance_type: String,
    /// The location, of the type the request asked for (a zone ID from
    /// [`crate::instance::describe_instance_type_offerings`]).
    pub location: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOffering {
    #[serde(default)]
    instance_type: String,
    #[serde(default)]
    location: String,
}

#[derive(Deserialize)]
struct RawOfferings {
    #[serde(rename = "instanceTypeOfferingSet", default)]
    instance_type_offering_set: Items<RawOffering>,
}

/// The offerings of a `DescribeInstanceTypeOfferings` response.
///
/// # Errors
/// [`Error::Decode`] for malformed XML.
pub fn parse_instance_type_offerings(body: &str) -> Result<Vec<InstanceTypeOffering>> {
    let raw: RawOfferings = decode("DescribeInstanceTypeOfferings", body)?;
    Ok(raw
        .instance_type_offering_set
        .item
        .into_iter()
        .map(|o| InstanceTypeOffering {
            instance_type: o.instance_type,
            location: o.location,
        })
        .collect())
}

/// What the provider publishes of an instance type, from
/// `DescribeInstanceTypes` (ADR-0161, ADR-0167 Decision 5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceTypeInfo {
    /// Instance type, such as `m6i.large`.
    pub instance_type: String,
    /// Default vCPUs (`vCpuInfo/defaultVCpus`), what the vCPU quota counts.
    pub default_vcpus: u32,
    /// Supported architectures (`processorInfo/supportedArchitectures`).
    pub architectures: Vec<String>,
    /// `nitroTpmSupport` is `supported`.
    pub nitro_tpm_supported: bool,
    /// NitroTPM versions (`nitroTpmInfo/supportedVersions`), such as `2.0`.
    pub nitro_tpm_versions: Vec<String>,
    /// Supported boot modes (`supportedBootModes`): `legacy-bios`, `uefi`.
    pub boot_modes: Vec<String>,
}

impl InstanceTypeInfo {
    /// True when the type boots UEFI, which a NitroTPM AMI requires
    /// (ADR-0167 finding 5).
    #[must_use]
    pub fn supports_uefi(&self) -> bool {
        self.boot_modes.iter().any(|m| m == BOOT_MODE_UEFI)
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawProcessorInfo {
    #[serde(default)]
    supported_architectures: Items<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawVCpuInfo {
    #[serde(default)]
    default_v_cpus: u32,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawNitroTpmInfo {
    #[serde(default)]
    supported_versions: Items<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawInstanceType {
    #[serde(default)]
    instance_type: String,
    #[serde(default)]
    processor_info: RawProcessorInfo,
    #[serde(rename = "vCpuInfo", default)]
    v_cpu_info: RawVCpuInfo,
    #[serde(default)]
    nitro_tpm_support: String,
    #[serde(default)]
    nitro_tpm_info: RawNitroTpmInfo,
    #[serde(default)]
    supported_boot_modes: Items<String>,
}

#[derive(Deserialize)]
struct RawInstanceTypes {
    #[serde(rename = "instanceTypeSet", default)]
    instance_type_set: Items<RawInstanceType>,
}

/// The instance types of a `DescribeInstanceTypes` response.
///
/// # Errors
/// [`Error::Decode`] for malformed XML.
pub fn parse_instance_types(body: &str) -> Result<Vec<InstanceTypeInfo>> {
    let raw: RawInstanceTypes = decode("DescribeInstanceTypes", body)?;
    Ok(raw
        .instance_type_set
        .item
        .into_iter()
        .map(|t| InstanceTypeInfo {
            instance_type: t.instance_type,
            default_vcpus: t.v_cpu_info.default_v_cpus,
            architectures: t.processor_info.supported_architectures.item,
            nitro_tpm_supported: t.nitro_tpm_support == NITRO_TPM_SUPPORTED,
            nitro_tpm_versions: t.nitro_tpm_info.supported_versions.item,
            boot_modes: t.supported_boot_modes.item,
        })
        .collect())
}

// ----- images ------------------------------------------------------------

/// `imageState` of an image that can be launched.
const IMAGE_STATE_AVAILABLE: &str = "available";
/// `rootDeviceType` of an EBS-backed image.
const ROOT_DEVICE_TYPE_EBS: &str = "ebs";
/// `tpmSupport` of an image registered for NitroTPM 2.0 (ADR-0167
/// Decision 5).
const TPM_SUPPORT_V2: &str = "v2.0";

/// What banlieue reads of an AMI, from `DescribeImages`. The owner account
/// is deliberately not kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageInfo {
    /// The AMI ID.
    pub image_id: String,
    /// `imageState`: `available`, `pending`, `failed`, `deregistered`, ...
    pub state: String,
    /// `architecture`: `x86_64`, `arm64`, `i386`, ...
    pub architecture: String,
    /// `rootDeviceName`, such as `/dev/xvda` or `/dev/sda1`.
    pub root_device_name: String,
    /// `rootDeviceType`: `ebs` or `instance-store`.
    pub root_device_type: String,
    /// `bootMode` (`uefi`, `legacy-bios`, `uefi-preferred`); `None` when
    /// the image does not set one and the instance type's default applies.
    pub boot_mode: Option<String>,
    /// `tpmSupport`: `v2.0` for an image registered for NitroTPM.
    pub tpm_support: Option<String>,
    /// `enaSupport`.
    pub ena_support: bool,
    /// The root device's EBS volume size, GiB, from its block device
    /// mapping.
    pub root_volume_size_gib: Option<u32>,
}

impl ImageInfo {
    /// `imageState` is `available`.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.state == IMAGE_STATE_AVAILABLE
    }

    /// `rootDeviceType` is `ebs`, the only kind banlieue launches.
    #[must_use]
    pub fn is_ebs_backed(&self) -> bool {
        self.root_device_type == ROOT_DEVICE_TYPE_EBS
    }

    /// `bootMode` is `uefi`, which NitroTPM requires (ADR-0167 finding 5).
    #[must_use]
    pub fn is_uefi(&self) -> bool {
        self.boot_mode.as_deref() == Some(BOOT_MODE_UEFI)
    }

    /// `tpmSupport` is `v2.0` (ADR-0167 Decision 5).
    #[must_use]
    pub fn has_tpm_v2(&self) -> bool {
        self.tpm_support.as_deref() == Some(TPM_SUPPORT_V2)
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawEbs {
    #[serde(default)]
    volume_size: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBlockDeviceMapping {
    #[serde(default)]
    device_name: String,
    #[serde(default)]
    ebs: Option<RawEbs>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawImage {
    #[serde(default)]
    image_id: String,
    #[serde(default)]
    image_state: String,
    #[serde(default)]
    architecture: String,
    #[serde(default)]
    root_device_name: String,
    #[serde(default)]
    root_device_type: String,
    #[serde(default)]
    boot_mode: Option<String>,
    #[serde(default)]
    tpm_support: Option<String>,
    #[serde(default)]
    ena_support: Option<bool>,
    #[serde(default)]
    block_device_mapping: Items<RawBlockDeviceMapping>,
}

#[derive(Deserialize)]
struct RawImages {
    #[serde(rename = "imagesSet", default)]
    images_set: Items<RawImage>,
}

/// The images of a `DescribeImages` response. The owner account and every
/// element banlieue does not decide on are ignored.
///
/// # Errors
/// [`Error::Decode`] for malformed XML.
pub fn parse_describe_images(body: &str) -> Result<Vec<ImageInfo>> {
    let raw: RawImages = decode("DescribeImages", body)?;
    Ok(raw
        .images_set
        .item
        .into_iter()
        .map(|i| {
            let root_volume_size_gib = i
                .block_device_mapping
                .item
                .iter()
                .find(|m| m.device_name == i.root_device_name)
                .and_then(|m| m.ebs.as_ref())
                .and_then(|e| e.volume_size);
            ImageInfo {
                image_id: i.image_id.trim().to_string(),
                state: i.image_state.trim().to_string(),
                architecture: i.architecture.trim().to_string(),
                root_device_name: i.root_device_name.trim().to_string(),
                root_device_type: i.root_device_type.trim().to_string(),
                boot_mode: non_empty(i.boot_mode),
                tpm_support: non_empty(i.tpm_support),
                ena_support: i.ena_support.unwrap_or(false),
                root_volume_size_gib,
            }
        })
        .collect())
}

// ----- pagination --------------------------------------------------------

/// Any paginated response; only `nextToken` is read.
#[derive(Deserialize)]
struct RawNextToken {
    #[serde(rename = "nextToken", default)]
    next_token: Option<String>,
}

/// The `nextToken` of a paginated response (`DescribeInstances`,
/// `DescribeInstanceTypeOfferings`, ...): `None` when absent or empty,
/// which is the last page.
///
/// # Errors
/// [`Error::Decode`] for malformed XML.
pub fn parse_next_token(body: &str) -> Result<Option<String>> {
    let raw: RawNextToken = decode("nextToken", body)?;
    Ok(non_empty(raw.next_token))
}

// ----- errors ------------------------------------------------------------

#[derive(Deserialize, Default)]
struct RawErrorDetail {
    #[serde(rename = "Code", default)]
    code: String,
    #[serde(rename = "Message", default)]
    message: String,
}

#[derive(Deserialize, Default)]
struct RawErrors {
    #[serde(rename = "Error", default)]
    error: Vec<RawErrorDetail>,
}

/// Any of the three shapes. The root element's name is not read, so one
/// struct covers EC2's `<Response>` (`Errors/Error`, `RequestID`), STS's
/// `<ErrorResponse>` (`Error`, `RequestId`) and S3's `<Error>` (`Code`,
/// `Message`, `RequestId` at the root).
#[derive(Deserialize, Default)]
struct RawErrorBody {
    #[serde(rename = "Errors", default)]
    errors: Option<RawErrors>,
    #[serde(rename = "Error", default)]
    error: Option<RawErrorDetail>,
    #[serde(rename = "Code", default)]
    code: Option<String>,
    #[serde(rename = "Message", default)]
    message: Option<String>,
    /// EC2's spelling.
    #[serde(rename = "RequestID", default)]
    request_id_ec2: Option<String>,
    /// STS's and S3's spelling.
    #[serde(rename = "RequestId", default)]
    request_id: Option<String>,
}

impl RawErrorBody {
    /// (code, message, request ID), first match wins.
    fn into_parts(self) -> (String, String, String) {
        let detail = self
            .errors
            .and_then(|e| e.error.into_iter().next())
            .or(self.error);
        let (code, message) = match detail {
            Some(d) => (d.code, d.message),
            None => (
                self.code.unwrap_or_default(),
                self.message.unwrap_or_default(),
            ),
        };
        let request_id = self.request_id_ec2.or(self.request_id).unwrap_or_default();
        (
            code.trim().to_string(),
            message.trim().to_string(),
            request_id.trim().to_string(),
        )
    }
}

/// The typed error for a failed EC2, STS or S3 call with HTTP `status` and
/// response `body`, through [`classify`].
///
/// A body that is not one of AWS's XML error shapes (an empty body, plain
/// text, an HTML page from a proxy) still gives a classified error: the
/// code is empty, so the status decides, and the body (cut to
/// [`crate::error::MAX_MESSAGE_LEN`]) is the message.
#[must_use]
pub fn parse_error(status: u16, body: &str) -> Error {
    let parsed = quick_xml::de::from_str::<RawErrorBody>(body)
        .ok()
        .map(RawErrorBody::into_parts)
        .filter(|(code, _, _)| !code.is_empty());
    let Some((code, message, request_id)) = parsed else {
        return classify(AwsFault::new(status, "", body.trim(), ""));
    };
    classify(AwsFault::new(status, &code, &message, &request_id))
}

// ----- helpers -----------------------------------------------------------

/// Decodes base64 that EC2 may wrap over several lines.
fn decode_base64(what: &str, encoded: &str) -> Result<Vec<u8>> {
    let compact: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(compact)
        .map_err(|e| Error::Decode(format!("{what} is not base64: {e}")))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod types_tests;
