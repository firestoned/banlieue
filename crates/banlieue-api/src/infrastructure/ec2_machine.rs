// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `infrastructure.banlieue.io/v1alpha1` EC2Machine CRD (ADR-0174).
//!
//! banlieue's implementation of the CAPI v1beta2 InfraMachine contract for
//! Amazon EC2 instances. Created by banlieue's main controller once a
//! `VirtualMachine` has been scheduled onto an `ec2` `Provider`, and
//! reconciled by the EC2 provider through the EC2 API.
//!
//! # Shape
//!
//! Fully resolved, like
//! [`AzureVirtualMachine`](super::azure_virtual_machine::AzureVirtualMachine):
//! the zone, instance type, AMI, volume type, subnet and security groups are
//! already chosen by the controller from the `Provider`'s mappings
//! (ADR-0160), so the provider interprets none of them. A create is one
//! `RunInstances` with a client token derived from the object's UID, tagging
//! the instance and its volumes in the same call (ADR-0174 Decision 3).
//!
//! What every instance gets is not configurable here and has no field:
//! IMDSv2 required with a hop limit of 1, encrypted volumes deleted on
//! termination, no public IP and no instance profile (ADR-0174 Decision 3).
//!
//! The CAPI contract label is emitted by `crdgen` (ADR-0005). The kind is
//! not `AWSMachine`, which Cluster API's AWS provider owns (ADR-0174
//! Decision 1).

use crate::aws;
use crate::common::*;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// URI scheme of an EC2 instance's `providerID`, as the AWS cloud controller
/// manager and Cluster API's AWS provider write it.
pub const PROVIDER_ID_SCHEME: &str = "aws";

/// Largest user data EC2 accepts, in bytes before base64 encoding
/// (ADR-0174 Decision 4; the spike's fake refuses one byte more).
pub const MAX_USER_DATA_BYTES: usize = 16 * 1024;

/// Largest root volume, GiB: io2 Block Express's ceiling. A smaller limit of
/// another volume type is left to EC2 to refuse.
pub const MAX_ROOT_VOLUME_GIB: u32 = 65_536;

/// The root device the publisher registers AMIs with (ADR-0162 Decision 9).
pub const DEFAULT_ROOT_DEVICE_NAME: &str = "/dev/xvda";

/// The `/dev/` prefix of a block device name.
const DEVICE_PREFIX: &str = "/dev/";

/// `aws:///`, the prefix of every EC2 `providerID`.
const PROVIDER_ID_PREFIX: &str = "aws:///";

/// The `providerID` of an EC2 instance: `aws:///<zone-name>/<instance-id>`
/// (ADR-0174 Decision 1).
#[must_use]
pub fn ec2_provider_id(zone: &str, instance_id: &str) -> String {
    format!("{PROVIDER_ID_SCHEME}:///{zone}/{instance_id}")
}

/// The zone name and instance ID of an EC2 `providerID`, or `None` when
/// `provider_id` is not `aws:///<zone-name>/<instance-id>` with a non-empty
/// zone and an `i-` instance ID.
#[must_use]
pub fn parse_ec2_provider_id(provider_id: &str) -> Option<(&str, &str)> {
    let rest = provider_id.strip_prefix(PROVIDER_ID_PREFIX)?;
    let (zone, instance_id) = rest.split_once('/')?;
    if zone.is_empty() || !aws::is_instance_id(instance_id) {
        return None;
    }
    Some((zone, instance_id))
}

fn default_root_device_name() -> String {
    DEFAULT_ROOT_DEVICE_NAME.to_string()
}

/// An EBS volume type banlieue creates root volumes as.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Ec2VolumeType {
    /// General purpose SSD, baseline IOPS and throughput settable.
    #[default]
    Gp3,
    /// Previous-generation general purpose SSD: IOPS follow the size.
    Gp2,
    /// Provisioned IOPS SSD; `iops` required.
    Io1,
    /// Provisioned IOPS SSD (Block Express); `iops` required.
    Io2,
}

impl Ec2VolumeType {
    /// The value EC2's `VolumeType` takes.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gp3 => "gp3",
            Self::Gp2 => "gp2",
            Self::Io1 => "io1",
            Self::Io2 => "io2",
        }
    }

    fn provisioned_iops(self) -> bool {
        matches!(self, Self::Io1 | Self::Io2)
    }
}

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "EC2Machine",
    plural = "ec2machines",
    shortname = "ec2m",
    namespaced,
    status = "EC2MachineStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"Provider","type":"string","jsonPath":".spec.providerRef.name"}"#,
    printcolumn = r#"{"name":"Zone","type":"string","jsonPath":".spec.zone"}"#,
    printcolumn = r#"{"name":"InstanceType","type":"string","jsonPath":".spec.instanceType"}"#,
    printcolumn = r#"{"name":"Provisioned","type":"boolean","jsonPath":".status.initialization.provisioned"}"#,
    printcolumn = r#"{"name":"State","type":"string","jsonPath":".status.instanceState"}"#,
    printcolumn = r#"{"name":"Power","type":"string","jsonPath":".status.observedPowerState"}"#,
    printcolumn = r#"{"name":"ProviderID","type":"string","jsonPath":".spec.providerID","priority":1}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
// The volume rules EC2 itself applies, refused when the object is stored
// rather than at `RunInstances` (mirrored by `EC2MachineSpec::validate`).
#[schemars(extend("x-kubernetes-validations" = [
    {
        "rule": "!has(self.throughput) || !has(self.volumeType) || self.volumeType == 'gp3'",
        "message": "throughput is set only with volumeType gp3",
    },
    {
        "rule": "!has(self.volumeType) || !(self.volumeType in ['io1', 'io2']) || has(self.iops)",
        "message": "iops is required with volumeType io1 or io2",
    },
    {
        "rule": "!has(self.iops) || !has(self.volumeType) || self.volumeType != 'gp2'",
        "message": "iops cannot be set with volumeType gp2, whose IOPS follow its size",
    },
]))]
/// EC2Machine: the concrete, scheduled instance request for Amazon EC2.
///
/// You normally do not create this by hand: banlieue's controller does,
/// owned by the `VirtualMachine` it was scheduled from, and the EC2 provider
/// launches the instance and reports CAPI-shaped status.
pub struct EC2MachineSpec {
    // ------------------------------------------------------------------
    // CAPI v1beta2 contract fields
    // ------------------------------------------------------------------
    /// CAPI contract: Provider ID for the resulting Node. Format:
    /// `aws:///<zone-name>/<instance-id>`, the form the AWS cloud controller
    /// manager writes. Set by the provider once the instance exists.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "providerID"
    )]
    pub provider_id: Option<String>,

    /// CAPI contract (optional): failure domain placement, the name of one of
    /// the `Provider`'s failure domains (one per availability zone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    // ------------------------------------------------------------------
    // banlieue / EC2-specific
    // ------------------------------------------------------------------
    /// The `Provider` whose connection names the region and the role to
    /// assume (ADR-0133: a shared provider serves tenant namespaces).
    pub provider_ref: ProviderReference,

    /// Availability zone **name** the instance is launched in, as the account
    /// names it (`ca-central-1a`). Resolved from the failure domain's
    /// `labels.zone`; the zone ID is reported in status (ADR-0158
    /// Decision 3).
    #[schemars(length(min = 1), pattern(r"^[a-z0-9-]+$"))]
    pub zone: String,

    /// Instance type (`m7i.large`), resolved from the `Provider`'s
    /// `instanceSizes` mapping for the `VMClass` (ADR-0160).
    #[schemars(pattern(r"^[a-z0-9-]+[.][a-z0-9-]+$"))]
    pub instance_type: String,

    /// The AMI to launch (`ami-` and 8 or 17 hex digits), from the
    /// `VMImage`'s `ec2` source: published by banlieue (ADR-0162 Decision 9)
    /// or a hand-published `Template`. When `tpmEnabled` is set, the AMI
    /// must be registered with `TpmSupport=v2.0` and `BootMode=uefi`
    /// (ADR-0167 Decision 5).
    #[schemars(pattern(aws::AMI_ID_PATTERN))]
    pub image_id: String,

    /// The AMI's root device name, which the root volume's block device
    /// mapping names. Defaults to `/dev/xvda`, the name banlieue registers
    /// its AMIs with.
    #[serde(default = "default_root_device_name")]
    #[schemars(pattern(r"^/dev/[a-z0-9]+$"))]
    pub root_device_name: String,

    /// Root volume size in GiB, from the `VMClass` (ADR-0160 Decision 4).
    /// At least the AMI's snapshot size, which EC2 enforces.
    #[serde(rename = "rootVolumeGiB")]
    #[schemars(range(min = 1, max = MAX_ROOT_VOLUME_GIB))]
    pub root_volume_gi_b: u32,

    /// EBS volume type of the root volume, from the storage class's
    /// `volumeType` target. Defaults to `gp3`.
    #[serde(default)]
    pub volume_type: Ec2VolumeType,

    /// Provisioned IOPS: required for `io1` and `io2`, optional for `gp3`
    /// (above its baseline), refused for `gp2`. From the storage class's
    /// `iops` target. EC2 refuses a value outside the type's range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub iops: Option<u32>,

    /// Throughput in MiB/s, `gp3` only, from the storage class's
    /// `throughput` target. EC2 refuses a value outside the type's range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub throughput: Option<u32>,

    /// KMS key that encrypts the root volume: a key ID, key ARN, alias name
    /// or alias ARN, from the `Provider`'s `connection.ec2.kmsKeyId`. Unset:
    /// the account's default EBS key. Volumes are encrypted either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(pattern(aws::KMS_KEY_PATTERN))]
    pub kms_key_id: Option<String>,

    /// Subnet the primary network interface is created in, from the network
    /// class's `subnetId` target for this zone. No public IP is assigned.
    #[schemars(pattern(aws::SUBNET_ID_PATTERN))]
    pub subnet_id: String,

    /// Security groups of the primary network interface, from the network
    /// class's `securityGroupIds` target. At least one.
    #[schemars(length(min = 1), inner(pattern(aws::SECURITY_GROUP_ID_PATTERN)))]
    pub security_group_ids: Vec<String>,

    /// NitroTPM: the instance gets a TPM 2.0 (ADR-0167). Set from the
    /// `VMClass`'s `tpmEnabled`; the AMI must be registered with
    /// `TpmSupport=v2.0` and the instance type must support NitroTPM.
    #[serde(default, skip_serializing_if = "is_false")]
    pub tpm_enabled: bool,

    /// Guest bootstrap payload, already resolved and placeholder-substituted
    /// by `banlieue-controller` (ADR-0025, ADR-0038). Sent as instance user
    /// data; at most 16,384 bytes before encoding. The provider reads no
    /// Secret or ConfigMap. IMDS is disabled once the guest is ready, so the
    /// guest reads it only during its first boot (ADR-0174 Decision 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = MAX_USER_DATA_BYTES))]
    pub user_data: Option<String>,

    /// Desired power state, resolved from the parent `VirtualMachine`.
    /// `PoweredOff` stops the instance, which stops compute billing (volumes
    /// are still billed). `Suspended` is refused.
    #[serde(default)]
    pub desired_power_state: PowerState,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// Whether `name` is `/dev/` and one or more lowercase letters or digits.
fn is_device_name(name: &str) -> bool {
    name.strip_prefix(DEVICE_PREFIX).is_some_and(|dev| {
        !dev.is_empty()
            && dev
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

/// Whether `t` is `<family>.<size>` of lowercase letters, digits and dashes.
fn is_instance_type(t: &str) -> bool {
    let part = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    t.split_once('.')
        .is_some_and(|(family, size)| part(family) && part(size))
}

impl EC2MachineSpec {
    /// Checks every rule the schema states, and those it cannot: user data
    /// within EC2's limit in bytes, no `Suspended` power state, identifiers of
    /// the right shape, a root volume of at least 1 GiB, and the volume
    /// type's IOPS and throughput rules.
    ///
    /// # Errors
    /// A human-readable reason for the first rule broken.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(user_data) = &self.user_data
            && user_data.len() > MAX_USER_DATA_BYTES
        {
            return Err(format!(
                "userData is {} bytes; EC2 user data allows {MAX_USER_DATA_BYTES}",
                user_data.len()
            ));
        }
        if self.desired_power_state == PowerState::Suspended {
            return Err("desiredPowerState Suspended is not supported on EC2".to_string());
        }
        if self.zone.is_empty()
            || !self
                .zone
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(format!(
                "zone {:?} is not an availability zone name",
                self.zone
            ));
        }
        if !is_instance_type(&self.instance_type) {
            return Err(format!(
                "instanceType {:?} is not an instance type such as m7i.large",
                self.instance_type
            ));
        }
        if !aws::is_ami_id(&self.image_id) {
            return Err(format!(
                "imageId {:?} is not an AMI ID (ami- and 8 or 17 hex digits)",
                self.image_id
            ));
        }
        if !is_device_name(&self.root_device_name) {
            return Err(format!(
                "rootDeviceName {:?} is not a device such as /dev/xvda",
                self.root_device_name
            ));
        }
        if self.root_volume_gi_b == 0 || self.root_volume_gi_b > MAX_ROOT_VOLUME_GIB {
            return Err(format!(
                "rootVolumeGiB must be between 1 and {MAX_ROOT_VOLUME_GIB}"
            ));
        }
        self.validate_volume()?;
        if !aws::is_subnet_id(&self.subnet_id) {
            return Err(format!(
                "subnetId {:?} is not a subnet ID (subnet- and 8 or 17 hex digits)",
                self.subnet_id
            ));
        }
        if self.security_group_ids.is_empty() {
            return Err("securityGroupIds needs at least one security group".to_string());
        }
        if let Some(bad) = self
            .security_group_ids
            .iter()
            .find(|id| !aws::is_security_group_id(id))
        {
            return Err(format!(
                "securityGroupIds: {bad:?} is not a security group ID (sg- and 8 or 17 hex digits)"
            ));
        }
        Ok(())
    }

    /// The IOPS, throughput and KMS key rules of [`Self::validate`].
    fn validate_volume(&self) -> Result<(), String> {
        if self.iops == Some(0) {
            return Err("iops must be at least 1".to_string());
        }
        if self.throughput == Some(0) {
            return Err("throughput must be at least 1".to_string());
        }
        if self.throughput.is_some() && self.volume_type != Ec2VolumeType::Gp3 {
            return Err(format!(
                "throughput is set only with volumeType gp3 (got {})",
                self.volume_type.as_str()
            ));
        }
        if self.volume_type.provisioned_iops() && self.iops.is_none() {
            return Err(format!(
                "iops is required with volumeType {}",
                self.volume_type.as_str()
            ));
        }
        if self.volume_type == Ec2VolumeType::Gp2 && self.iops.is_some() {
            return Err("iops cannot be set with volumeType gp2".to_string());
        }
        if let Some(key) = &self.kms_key_id
            && !aws::is_kms_key(key)
        {
            return Err(format!(
                "kmsKeyId {key:?} is not a KMS key ID, key ARN, alias or alias ARN"
            ));
        }
        Ok(())
    }
}

/// Observed state of an EC2Machine, shaped to the CAPI v1beta2 InfraMachine
/// status contract. The non-contract fields mean what they mean on the
/// siblings, so the controller's status mirror treats backends alike.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EC2MachineStatus {
    /// CAPI contract field.
    #[serde(default)]
    pub initialization: InitializationStatus,

    /// CAPI contract field (optional): observed failure domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    /// CAPI contract field (optional): the primary private IPv4 address
    /// (`InternalIP`) and the instance's private DNS name (`InternalDNS`).
    /// No public or Elastic IP in v1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<MachineAddress>,

    /// The instance ID (`i-...`), once `RunInstances` returned it or the
    /// provider adopted the instance by its ownership tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,

    /// The zone **ID** (`cac1-az1`) of `spec.zone` in this account: zone names
    /// differ per account, IDs do not, so two accounts compare by ID
    /// (ADR-0158 Decision 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone_id: Option<String>,

    /// The instance's EC2 state name (`pending`, `running`, `stopping`,
    /// `stopped`, `shutting-down`, `terminated`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_state: Option<String>,

    /// Sticky: the guest reported ready once on the serial console
    /// (ADR-0175, ADR-0043 Decision 5).
    #[serde(default, skip_serializing_if = "is_false")]
    pub guest_installed: bool,

    /// Sticky: the provider turned the instance metadata endpoint off after
    /// the guest reported ready (ADR-0174 Decision 3). Never turned back on
    /// automatically.
    #[serde(default, skip_serializing_if = "is_false")]
    pub imds_disabled: bool,

    /// The NitroTPM's endorsement key public keys, PEM
    /// `SubjectPublicKeyInfo` (RSA-2048 and ECC P-384), read host-side by the
    /// provider with `GetInstanceTpmEkPub` once the instance runs (ADR-0167
    /// Decision 1). Never read from the guest. Empty unless `tpmEnabled`.
    /// banlieue publishes these and vouches for nothing: the source is the
    /// EC2 API its own identity called.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tpm_endorsement_public_keys: Vec<String>,

    /// Consecutive failed creates of this generation: the provider waits
    /// `min(5 min * 2^(n - 1), 1 h)` before creating again (ADR-0173
    /// Decision 5). Reset when the instance exists.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub create_failures: u32,

    /// The instance's last observed run state (`stopped` reads
    /// `PoweredOff`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_power_state: Option<PowerState>,

    /// Standard Kubernetes conditions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["type"],
    ))]
    pub conditions: Vec<Condition>,

    /// The `metadata.generation` this status was computed from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

// ----------------------------------------------------------------------
// EC2MachineTemplate: required by CAPI for MachineDeployment use
// ----------------------------------------------------------------------

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "EC2MachineTemplate",
    plural = "ec2machinetemplates",
    shortname = "ec2mt",
    namespaced,
    derive = "PartialEq"
)]
#[serde(rename_all = "camelCase")]
/// EC2MachineTemplate: a stamped-out EC2Machine spec.
///
/// CAPI requires an InfraMachineTemplate so a MachineSet or MachineDeployment
/// can mint identical machines. banlieue ships it for CAPI compatibility;
/// standalone VirtualMachine users do not need it.
pub struct EC2MachineTemplateSpec {
    /// The spec stamped into every machine created from this template.
    pub template: EC2MachineTemplateResource,
}

/// Wrapper matching CAPI's `template: { spec: {...} }` shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EC2MachineTemplateResource {
    /// The EC2Machine spec for machines created from this template.
    pub spec: EC2MachineSpec,
}

#[cfg(test)]
#[path = "ec2_machine_tests.rs"]
mod ec2_machine_tests;
