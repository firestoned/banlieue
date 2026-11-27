// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! EC2 requests, as Query parameters (ADR-0174 Decisions 2 and 3).
//!
//! Each builder returns an [`Ec2Request`]: the `Action` name and the
//! action's own parameters, in a fixed order. The caller adds `Action` and
//! `Version`, form-encodes and signs.
//!
//! [`RunInstancesRequest`] is the hardened create. What ADR-0174 Decision 3
//! requires of every instance is not a field and cannot be turned off:
//!
//! - IMDSv2 only (`MetadataOptions.HttpTokens=required`), hop limit 1, the
//!   endpoint on until the provider turns it off after `GuestReady`
//!   ([`disable_instance_metadata`]);
//! - the root volume encrypted and deleted with the instance;
//! - one network interface, no public IPv4, deleted with the instance;
//! - exactly one instance per call (`MinCount=MaxCount=1`);
//! - **no instance profile**: there is no `IamInstanceProfile` parameter,
//!   and no field to add one.
//!
//! Every query name was checked against the EC2 API reference
//! (`RunInstances`, `InstanceNetworkInterfaceSpecification`,
//! `EbsBlockDevice`, `TagSpecification`, `ModifyInstanceMetadataOptions`,
//! and each other action's page).

use std::collections::BTreeMap;

use base64::Engine as _;

use crate::error::{Error, Result};
use crate::tags::{TagResourceType, tag_filter_name, tag_specification_params};

/// EC2's limit on user data, counted on the raw bytes **before** base64
/// (seen live in the roadmap 24 spike).
pub const MAX_USER_DATA_BYTES: usize = 16_384;
/// EC2's limit on a `ClientToken`: 64 ASCII characters.
pub const MAX_CLIENT_TOKEN_LEN: usize = 64;
/// `DescribeInstanceTypes` accepts at most this many `InstanceType.N`.
pub const MAX_DESCRIBE_INSTANCE_TYPES: usize = 100;

/// `RunInstances`.
pub const ACTION_RUN_INSTANCES: &str = "RunInstances";
/// `TerminateInstances`.
pub const ACTION_TERMINATE_INSTANCES: &str = "TerminateInstances";
/// `StopInstances`.
pub const ACTION_STOP_INSTANCES: &str = "StopInstances";
/// `StartInstances`.
pub const ACTION_START_INSTANCES: &str = "StartInstances";
/// `DescribeInstances`.
pub const ACTION_DESCRIBE_INSTANCES: &str = "DescribeInstances";
/// `GetConsoleOutput`.
pub const ACTION_GET_CONSOLE_OUTPUT: &str = "GetConsoleOutput";
/// `GetInstanceTpmEkPub`.
pub const ACTION_GET_INSTANCE_TPM_EK_PUB: &str = "GetInstanceTpmEkPub";
/// `ModifyInstanceMetadataOptions`.
pub const ACTION_MODIFY_INSTANCE_METADATA_OPTIONS: &str = "ModifyInstanceMetadataOptions";
/// `DescribeAvailabilityZones`.
pub const ACTION_DESCRIBE_AVAILABILITY_ZONES: &str = "DescribeAvailabilityZones";
/// `DescribeInstanceTypeOfferings`.
pub const ACTION_DESCRIBE_INSTANCE_TYPE_OFFERINGS: &str = "DescribeInstanceTypeOfferings";
/// `DescribeInstanceTypes`.
pub const ACTION_DESCRIBE_INSTANCE_TYPES: &str = "DescribeInstanceTypes";
/// `DescribeImages`.
pub const ACTION_DESCRIBE_IMAGES: &str = "DescribeImages";
/// An AMI ID's prefix.
const AMI_ID_PREFIX: &str = "ami-";
/// The hex digits of a short and a long EC2 resource ID.
const RESOURCE_ID_HEX_LENS: [usize; 2] = [8, 17];
/// `DescribeImages` names one image.
const P_IMAGE_ID_1: &str = "ImageId.1";

const CLIENT_TOKEN_PREFIX: &str = "banlieue-";
/// Separates UID and zone in the hashed token input; neither may contain it.
const TOKEN_INPUT_SEPARATOR: u8 = 0;
/// The hex group lengths of a Kubernetes UID (`8-4-4-4-12`).
const UID_GROUP_LENS: [usize; 5] = [8, 4, 4, 4, 12];
const TRUE: &str = "true";
const FALSE: &str = "false";
const ENABLED: &str = "enabled";
const DISABLED: &str = "disabled";
/// The only instance count banlieue launches per call.
const INSTANCE_COUNT: &str = "1";
/// The primary interface's device index.
const PRIMARY_DEVICE_INDEX: &str = "0";
/// IMDS answers only on the instance itself: a container behind a bridge
/// cannot reach it (ADR-0174 Decision 3).
const IMDS_HOP_LIMIT: &str = "1";
const IMDS_TOKENS_REQUIRED: &str = "required";
const KEY_FORMAT_DER: &str = "der";
const LOCATION_TYPE_ZONE_ID: &str = "availability-zone-id";
const FILTER_INSTANCE_TYPE: &str = "instance-type";
const FILTER_1_NAME: &str = "Filter.1.Name";

const P_INSTANCE_ID: &str = "InstanceId";
const P_INSTANCE_ID_1: &str = "InstanceId.1";
const P_NIC: &str = "NetworkInterface.1";
const P_ROOT: &str = "BlockDeviceMapping.1";
const INSTANCE_ID_FIELD: &str = "instance ID";

/// An EC2 Query call: the action and its own parameters, in a fixed order.
/// `Action` and `Version` are not in `params`; the transport adds them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ec2Request {
    /// The `Action` name, such as `RunInstances`.
    pub action: &'static str,
    /// The action's parameters, as EC2 Query names and values.
    pub params: Vec<(String, String)>,
}

impl Ec2Request {
    fn new(action: &'static str) -> Self {
        Self {
            action,
            params: Vec::new(),
        }
    }

    fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        push(&mut self.params, key, value);
        self
    }
}

/// The EBS volume type of the root volume. The default is `gp3`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VolumeType {
    /// General purpose SSD, previous generation.
    Gp2,
    /// General purpose SSD; IOPS and throughput may be set.
    #[default]
    Gp3,
    /// Provisioned IOPS SSD; IOPS required.
    Io1,
    /// Provisioned IOPS SSD; IOPS required.
    Io2,
}

impl VolumeType {
    /// EC2's name for the type.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gp2 => "gp2",
            Self::Gp3 => "gp3",
            Self::Io1 => "io1",
            Self::Io2 => "io2",
        }
    }

    fn requires_iops(self) -> bool {
        matches!(self, Self::Io1 | Self::Io2)
    }

    fn accepts_throughput(self) -> bool {
        self == Self::Gp3
    }
}

/// The NitroTPM endorsement key type `GetInstanceTpmEkPub` returns
/// (ADR-0167 Decision 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EkKeyType {
    /// RSA 2048-bit.
    Rsa2048,
    /// ECC on NIST P-384.
    EccSecP384,
}

impl EkKeyType {
    /// EC2's name for the key type.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rsa2048 => "rsa-2048",
            Self::EccSecP384 => "ecc-sec-p384",
        }
    }

    /// The key type for EC2's name, or `None` for a name EC2 does not
    /// document.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Rsa2048, Self::EccSecP384]
            .into_iter()
            .find(|t| t.as_str() == value)
    }
}

/// What a `RunInstances` needs. The hardening of ADR-0174 Decision 3 is
/// not here: [`Self::to_params`] always adds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunInstancesRequest {
    /// The AMI, such as `ami-0123456789abcdef0`.
    pub image_id: String,
    /// The instance type, such as `m6i.large`.
    pub instance_type: String,
    /// The zone **name** in this account, such as `ca-central-1a`.
    pub zone_name: String,
    /// The subnet of the one network interface; it must be in the zone.
    pub subnet_id: String,
    /// Security groups of the network interface; at least one, declared
    /// rather than left to the VPC's default group.
    pub security_group_ids: Vec<String>,
    /// The AMI's root device name (`rootDeviceName`), such as `/dev/xvda`.
    pub root_device_name: String,
    /// Root volume size in GiB; at least the AMI's snapshot size.
    pub root_volume_size_gib: u32,
    /// Root volume type.
    pub volume_type: VolumeType,
    /// Provisioned IOPS; required for `io1` and `io2`.
    pub iops: Option<u32>,
    /// Throughput in MiB/s; `gp3` only.
    pub throughput: Option<u32>,
    /// A customer-managed KMS key for the root volume; the account's EBS
    /// default key when `None`. Encryption is on either way.
    pub kms_key_id: Option<String>,
    /// User data, raw; at most [`MAX_USER_DATA_BYTES`]. Empty for none.
    pub user_data: Vec<u8>,
    /// The idempotency token, from [`client_token_for`].
    pub client_token: String,
    /// Tags on the instance (ownership tags and `Name`), already validated.
    pub instance_tags: BTreeMap<String, String>,
    /// Tags on the root volume, already validated.
    pub volume_tags: BTreeMap<String, String>,
    /// Tags on the network interface, already validated.
    pub network_interface_tags: BTreeMap<String, String>,
}

impl RunInstancesRequest {
    /// Refuses a request EC2 or ADR-0174 would refuse.
    ///
    /// # Errors
    /// [`Error::Invalid`] for an empty required field or security group ID,
    /// a root volume of 0 GiB, user data over [`MAX_USER_DATA_BYTES`]
    /// before encoding, a client token that is not 1 to
    /// [`MAX_CLIENT_TOKEN_LEN`] ASCII characters, throughput on a type
    /// other than `gp3`, or `io1`/`io2` without IOPS.
    pub fn validate(&self) -> Result<()> {
        require("image ID", &self.image_id)?;
        require("instance type", &self.instance_type)?;
        require("zone name", &self.zone_name)?;
        require("subnet ID", &self.subnet_id)?;
        require("root device name", &self.root_device_name)?;
        require("client token", &self.client_token)?;
        if self.security_group_ids.is_empty() {
            return Err(invalid("at least one security group ID is required"));
        }
        if self.security_group_ids.iter().any(String::is_empty) {
            return Err(invalid("a security group ID is empty"));
        }
        if self.root_volume_size_gib == 0 {
            return Err(invalid("root volume size must be at least 1 GiB"));
        }
        if self.user_data.len() > MAX_USER_DATA_BYTES {
            return Err(Error::Invalid(format!(
                "user data is {} bytes; EC2 accepts at most {MAX_USER_DATA_BYTES} before encoding",
                self.user_data.len()
            )));
        }
        validate_client_token(&self.client_token)?;
        if self.throughput.is_some() && !self.volume_type.accepts_throughput() {
            return Err(Error::Invalid(format!(
                "throughput applies to gp3 only, not {}",
                self.volume_type.as_str()
            )));
        }
        if self.volume_type.requires_iops() && self.iops.is_none() {
            return Err(Error::Invalid(format!(
                "{} requires IOPS",
                self.volume_type.as_str()
            )));
        }
        Ok(())
    }

    /// The `RunInstances` parameters, hardening included, in a fixed order.
    ///
    /// # Errors
    /// As [`Self::validate`].
    pub fn to_params(&self) -> Result<Vec<(String, String)>> {
        self.validate()?;
        let mut p = Vec::new();
        push(&mut p, "ImageId", &self.image_id);
        push(&mut p, "InstanceType", &self.instance_type);
        push(&mut p, "MinCount", INSTANCE_COUNT);
        push(&mut p, "MaxCount", INSTANCE_COUNT);
        push(&mut p, "ClientToken", &self.client_token);
        if !self.user_data.is_empty() {
            push(
                &mut p,
                "UserData",
                base64::engine::general_purpose::STANDARD.encode(&self.user_data),
            );
        }
        push(&mut p, "Placement.AvailabilityZone", &self.zone_name);
        self.push_network_interface(&mut p);
        push(&mut p, "MetadataOptions.HttpTokens", IMDS_TOKENS_REQUIRED);
        push(
            &mut p,
            "MetadataOptions.HttpPutResponseHopLimit",
            IMDS_HOP_LIMIT,
        );
        push(&mut p, "MetadataOptions.HttpEndpoint", ENABLED);
        self.push_root_volume(&mut p);
        self.push_tag_specifications(&mut p);
        Ok(p)
    }

    /// [`Self::to_params`] with the action name.
    ///
    /// # Errors
    /// As [`Self::validate`].
    pub fn to_request(&self) -> Result<Ec2Request> {
        Ok(Ec2Request {
            action: ACTION_RUN_INSTANCES,
            params: self.to_params()?,
        })
    }

    fn push_network_interface(&self, p: &mut Vec<(String, String)>) {
        push(p, format!("{P_NIC}.DeviceIndex"), PRIMARY_DEVICE_INDEX);
        push(p, format!("{P_NIC}.SubnetId"), &self.subnet_id);
        for (i, group) in self.security_group_ids.iter().enumerate() {
            push(p, format!("{P_NIC}.SecurityGroupId.{}", i + 1), group);
        }
        push(p, format!("{P_NIC}.AssociatePublicIpAddress"), FALSE);
        push(p, format!("{P_NIC}.DeleteOnTermination"), TRUE);
    }

    fn push_root_volume(&self, p: &mut Vec<(String, String)>) {
        push(p, format!("{P_ROOT}.DeviceName"), &self.root_device_name);
        push(
            p,
            format!("{P_ROOT}.Ebs.VolumeSize"),
            self.root_volume_size_gib.to_string(),
        );
        push(
            p,
            format!("{P_ROOT}.Ebs.VolumeType"),
            self.volume_type.as_str(),
        );
        push(p, format!("{P_ROOT}.Ebs.Encrypted"), TRUE);
        push(p, format!("{P_ROOT}.Ebs.DeleteOnTermination"), TRUE);
        if let Some(key) = &self.kms_key_id {
            push(p, format!("{P_ROOT}.Ebs.KmsKeyId"), key);
        }
        if let Some(iops) = self.iops {
            push(p, format!("{P_ROOT}.Ebs.Iops"), iops.to_string());
        }
        if let Some(throughput) = self.throughput {
            push(
                p,
                format!("{P_ROOT}.Ebs.Throughput"),
                throughput.to_string(),
            );
        }
    }

    /// `TagSpecification.N`, 1-based and contiguous, in the order instance,
    /// volume, network interface; a resource with no tags is skipped. The
    /// pairs come from [`tag_specification_params`], the one renderer.
    fn push_tag_specifications(&self, p: &mut Vec<(String, String)>) {
        let specs = [
            (TagResourceType::Instance, &self.instance_tags),
            (TagResourceType::Volume, &self.volume_tags),
            (
                TagResourceType::NetworkInterface,
                &self.network_interface_tags,
            ),
        ];
        let mut index = 0;
        for (resource_type, tags) in specs {
            if tags.is_empty() {
                continue;
            }
            index += 1;
            p.extend(tag_specification_params(index, resource_type, tags));
        }
    }
}

/// The `ClientToken` for the object with `uid` placed in `zone_name`: the
/// same pair always gives the same token, so a retried create in the same
/// zone returns the same instance (ADR-0174 Decision 3).
///
/// The zone is part of the token because the token stands for every
/// parameter: a re-placement in another zone (ADR-0173) is a different
/// request, and reusing the token would get `IdempotentParameterMismatch`.
///
/// A Kubernetes UID (the fixed `8-4-4-4-12` hex shape) and a zone name of
/// lowercase letters, digits and `-` give `banlieue-<uid>-<zone>`, readable
/// and searchable with the `client-token` filter; the UID's fixed length
/// keeps the two parts from being confused. Anything else gives `banlieue-`
/// and the leading hex digits of the SHA-256 of the UID, a NUL and the zone,
/// within [`MAX_CLIENT_TOKEN_LEN`].
#[must_use]
pub fn client_token_for(uid: &str, zone_name: &str) -> String {
    let readable = format!("{CLIENT_TOKEN_PREFIX}{uid}-{zone_name}");
    if is_kubernetes_uid(uid) && is_zone_name(zone_name) && readable.len() <= MAX_CLIENT_TOKEN_LEN {
        return readable;
    }
    let mut input = Vec::with_capacity(uid.len() + 1 + zone_name.len());
    input.extend_from_slice(uid.as_bytes());
    input.push(TOKEN_INPUT_SEPARATOR);
    input.extend_from_slice(zone_name.as_bytes());
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &input);
    let room = MAX_CLIENT_TOKEN_LEN - CLIENT_TOKEN_PREFIX.len();
    let hex: String = digest
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
        .chars()
        .take(room)
        .collect();
    format!("{CLIENT_TOKEN_PREFIX}{hex}")
}

/// True for the canonical `8-4-4-4-12` lowercase or uppercase hex UID shape
/// Kubernetes assigns.
fn is_kubernetes_uid(uid: &str) -> bool {
    let groups: Vec<&str> = uid.split('-').collect();
    groups.len() == UID_GROUP_LENS.len()
        && groups
            .iter()
            .zip(UID_GROUP_LENS)
            .all(|(g, len)| g.len() == len && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// True for a non-empty zone name of lowercase ASCII letters, digits and `-`.
fn is_zone_name(zone_name: &str) -> bool {
    !zone_name.is_empty()
        && zone_name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `TerminateInstances` for one instance.
///
/// # Errors
/// [`Error::Invalid`] when `instance_id` is empty.
pub fn terminate_instances(instance_id: &str) -> Result<Ec2Request> {
    one_instance(ACTION_TERMINATE_INSTANCES, instance_id)
}

/// `StopInstances` for one instance (`PoweredOff`).
///
/// # Errors
/// [`Error::Invalid`] when `instance_id` is empty.
pub fn stop_instances(instance_id: &str) -> Result<Ec2Request> {
    one_instance(ACTION_STOP_INSTANCES, instance_id)
}

/// `StartInstances` for one instance.
///
/// # Errors
/// [`Error::Invalid`] when `instance_id` is empty.
pub fn start_instances(instance_id: &str) -> Result<Ec2Request> {
    one_instance(ACTION_START_INSTANCES, instance_id)
}

/// `DescribeInstances` for one instance ID.
///
/// # Errors
/// [`Error::Invalid`] when `instance_id` is empty.
pub fn describe_instance(instance_id: &str) -> Result<Ec2Request> {
    one_instance(ACTION_DESCRIBE_INSTANCES, instance_id)
}

/// Whether `image_id` is `ami-` and 8 or 17 lowercase hex digits, the shape
/// EC2 issues (the same rule as `banlieue_api::aws::is_ami_id`).
#[must_use]
pub fn is_ami_id(image_id: &str) -> bool {
    image_id.strip_prefix(AMI_ID_PREFIX).is_some_and(|hex| {
        RESOURCE_ID_HEX_LENS.contains(&hex.len())
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// `DescribeImages` for one AMI (`ImageId.1`): what the provider records of
/// a hand-published `Template` image (roadmap 24 phase E).
///
/// # Errors
/// [`Error::Invalid`] when `image_id` is not an AMI ID, before anything is
/// sent.
pub fn describe_image(image_id: &str) -> Result<Ec2Request> {
    if !is_ami_id(image_id) {
        return Err(Error::Invalid(format!(
            "{image_id:?} is not an AMI ID (ami- and 8 or 17 hex digits)"
        )));
    }
    Ok(Ec2Request::new(ACTION_DESCRIBE_IMAGES).with(P_IMAGE_ID_1, image_id))
}

/// `DescribeInstances` for the instances carrying tag `key` with `value`
/// (`Filter.1.Name=tag:<key>`), such as the ownership tag (ADR-0158
/// Decision 4).
///
/// # Errors
/// [`Error::Invalid`] when `key` is empty.
pub fn describe_instances_by_tag(key: &str, value: &str) -> Result<Ec2Request> {
    require("tag key", key)?;
    Ok(Ec2Request::new(ACTION_DESCRIBE_INSTANCES)
        .with(FILTER_1_NAME, tag_filter_name(key))
        .with("Filter.1.Value.1", value))
}

/// `GetConsoleOutput` with `Latest=true`: the serial console the guest's
/// ready line is read from (ADR-0175 Decision 5).
///
/// # Errors
/// [`Error::Invalid`] when `instance_id` is empty.
pub fn get_console_output(instance_id: &str) -> Result<Ec2Request> {
    require(INSTANCE_ID_FIELD, instance_id)?;
    Ok(Ec2Request::new(ACTION_GET_CONSOLE_OUTPUT)
        .with(P_INSTANCE_ID, instance_id)
        .with("Latest", TRUE))
}

/// `GetInstanceTpmEkPub` for `key_type`, in the `der` format (ADR-0167
/// Decision 1).
///
/// # Errors
/// [`Error::Invalid`] when `instance_id` is empty.
pub fn get_instance_tpm_ek_pub(instance_id: &str, key_type: EkKeyType) -> Result<Ec2Request> {
    require(INSTANCE_ID_FIELD, instance_id)?;
    Ok(Ec2Request::new(ACTION_GET_INSTANCE_TPM_EK_PUB)
        .with(P_INSTANCE_ID, instance_id)
        .with("KeyType", key_type.as_str())
        .with("KeyFormat", KEY_FORMAT_DER))
}

/// `ModifyInstanceMetadataOptions` turning the metadata endpoint off,
/// after `GuestReady` (ADR-0174 Decision 3). Nothing turns it back on.
///
/// # Errors
/// [`Error::Invalid`] when `instance_id` is empty.
pub fn disable_instance_metadata(instance_id: &str) -> Result<Ec2Request> {
    require(INSTANCE_ID_FIELD, instance_id)?;
    Ok(Ec2Request::new(ACTION_MODIFY_INSTANCE_METADATA_OPTIONS)
        .with(P_INSTANCE_ID, instance_id)
        .with("HttpEndpoint", DISABLED))
}

/// `DescribeAvailabilityZones` for the region's enabled zones.
#[must_use]
pub fn describe_availability_zones() -> Ec2Request {
    Ec2Request::new(ACTION_DESCRIBE_AVAILABILITY_ZONES)
}

/// `DescribeInstanceTypeOfferings` by zone ID (ADR-0174 finding 8),
/// filtered to `instance_types`.
///
/// # Errors
/// [`Error::Invalid`] when `instance_types` is empty or names an empty
/// type.
pub fn describe_instance_type_offerings<S: AsRef<str>>(instance_types: &[S]) -> Result<Ec2Request> {
    require_types(instance_types)?;
    let mut call = Ec2Request::new(ACTION_DESCRIBE_INSTANCE_TYPE_OFFERINGS)
        .with("LocationType", LOCATION_TYPE_ZONE_ID)
        .with(FILTER_1_NAME, FILTER_INSTANCE_TYPE);
    for (i, t) in instance_types.iter().enumerate() {
        push(
            &mut call.params,
            format!("Filter.1.Value.{}", i + 1),
            t.as_ref(),
        );
    }
    Ok(call)
}

/// `DescribeInstanceTypes` for `instance_types` (`InstanceType.N`).
///
/// # Errors
/// [`Error::Invalid`] when `instance_types` is empty, names an empty type,
/// or has more than [`MAX_DESCRIBE_INSTANCE_TYPES`].
pub fn describe_instance_types<S: AsRef<str>>(instance_types: &[S]) -> Result<Ec2Request> {
    require_types(instance_types)?;
    if instance_types.len() > MAX_DESCRIBE_INSTANCE_TYPES {
        return Err(Error::Invalid(format!(
            "{} instance types; DescribeInstanceTypes takes at most {MAX_DESCRIBE_INSTANCE_TYPES}",
            instance_types.len()
        )));
    }
    let mut call = Ec2Request::new(ACTION_DESCRIBE_INSTANCE_TYPES);
    for (i, t) in instance_types.iter().enumerate() {
        push(
            &mut call.params,
            format!("InstanceType.{}", i + 1),
            t.as_ref(),
        );
    }
    Ok(call)
}

fn one_instance(action: &'static str, instance_id: &str) -> Result<Ec2Request> {
    require(INSTANCE_ID_FIELD, instance_id)?;
    Ok(Ec2Request::new(action).with(P_INSTANCE_ID_1, instance_id))
}

fn require_types<S: AsRef<str>>(instance_types: &[S]) -> Result<()> {
    if instance_types.is_empty() {
        return Err(invalid("at least one instance type is required"));
    }
    if instance_types.iter().any(|t| t.as_ref().is_empty()) {
        return Err(invalid("an instance type is empty"));
    }
    Ok(())
}

fn validate_client_token(token: &str) -> Result<()> {
    if !token.is_ascii() {
        return Err(invalid("client token must be ASCII"));
    }
    if token.len() > MAX_CLIENT_TOKEN_LEN {
        return Err(Error::Invalid(format!(
            "client token is {} characters; EC2 accepts at most {MAX_CLIENT_TOKEN_LEN}",
            token.len()
        )));
    }
    Ok(())
}

fn require(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(Error::Invalid(format!("{field} is required")));
    }
    Ok(())
}

fn invalid(message: &str) -> Error {
    Error::Invalid(message.to_string())
}

fn push(params: &mut Vec<(String, String)>, key: impl Into<String>, value: impl Into<String>) {
    params.push((key.into(), value.into()));
}

#[cfg(test)]
#[path = "instance_tests.rs"]
mod instance_tests;
