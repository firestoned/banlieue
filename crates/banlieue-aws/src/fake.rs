// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! An in-memory EC2 for unit tests (ADR-0174 Decision 4).
//!
//! It refuses what EC2 refused in the roadmap 24 phase 0 spike, and what
//! the IAM policy shipped in `deploy/provider-ec2/` denies
//! (rules/testing.md, rule 1). Every refusal is the fault EC2 answers, run
//! through [`classify`], so a reconciler sees exactly the typed [`Error`] a
//! real call produces:
//!
//! - **A reused client token with different parameters**:
//!   `IdempotentParameterMismatch` (400). The whole parameter set is
//!   compared, as EC2 does; the same token and parameters return the first
//!   instance, even once capacity or quota has run out (spike, ADR-0174
//!   finding 1 and Decision 3).
//! - **A type not offered in the zone**: `Unsupported` (400), from the
//!   configured offerings ([`FakeEc2::set_offered`]) (ADR-0174 Decision 3,
//!   ADR-0173).
//! - **No capacity** for a (zone, type) pair:
//!   `InsufficientInstanceCapacity` (500), from
//!   [`FakeEc2::set_insufficient_capacity`] (ADR-0173).
//! - **The vCPU quota**: `VcpuLimitExceeded` (400) when the new instance's
//!   vCPUs (from the configured type table) plus those of every `pending`,
//!   `running` and `stopping` instance exceed [`FakeEc2::set_vcpu_quota`];
//!   a start is counted the same way. The spike's account had the default
//!   of 5 and raised it to 16 (ADR-0174 finding 8).
//! - **The shipped IAM policy's conditions**: `UnauthorizedOperation`
//!   (403) unless `MetadataOptions.HttpTokens=required`,
//!   `MetadataOptions.HttpPutResponseHopLimit=1`,
//!   `BlockDeviceMapping.1.Ebs.Encrypted=true`,
//!   `NetworkInterface.1.AssociatePublicIpAddress=false` and an `instance`
//!   tag specification carrying `banlieue-owner-uid`; and for any
//!   `IamInstanceProfile*` parameter (ADR-0174 finding 5, Decisions 3 and
//!   6). The spike's live run refused IMDSv1, hop limit 2, an untagged
//!   launch and a public IP this way. A denial comes before anything else,
//!   an idempotent retry included, as IAM is evaluated first.
//! - **User data over 16,384 raw bytes**, or not base64:
//!   `InvalidParameterValue` (400), measured after decoding, so it holds
//!   even for parameters that bypass the builder (spike).
//! - **Eventual consistency**: a fresh instance ID is
//!   `InvalidInstanceID.NotFound` on its first
//!   [`DEFAULT_VISIBILITY_DELAY`] describes, by ID or by tag
//!   ([`FakeEc2::set_visibility_delay`]); [`Ec2Api::describe_instance`]
//!   reads that as `Ok(None)` (spike, ADR-0174 Decision 2).
//! - **Throttling**: `RequestLimitExceeded` (503) for the next calls of any
//!   method ([`FakeEc2::throttle_next`]).
//! - **Lifecycle**: `IncorrectInstanceState` (400) for starting an instance
//!   that is `stopping`, `shutting-down` or `terminated` (a `pending` one
//!   answers `pending` and a `running` one `running`, as EC2 does), for stopping a `pending`, `shutting-down` or `terminated` one,
//!   and for the EK or the metadata endpoint of a `shutting-down` or
//!   `terminated` one. `InvalidInstanceID.NotFound` (400) for an unknown
//!   ID, `InvalidInstanceID.Malformed` (400) for an ID that is not
//!   `i-<hex>`.
//! - **NitroTPM**: `UnsupportedOperation` (400) for the EK of an instance
//!   whose type has no NitroTPM (ADR-0167). `InstanceTpmEkPub.NotFound`
//!   (400) for the EK of a `pending` instance, and for the first
//!   [`FakeEc2::set_ek_not_found_reads`] reads after each launch or start
//!   (none by default): EC2 answered not found for an instance that had
//!   just reached `running` and the key on the next read (ADR-0167
//!   Decision 1, seen live 2026-11-27).
//! - **Unknown names**: `InvalidParameterValue` (400) for a zone or an
//!   instance type the fake does not know at launch, `InvalidInstanceType`
//!   (400) for one asked of `DescribeInstanceTypes`, `MissingParameter`
//!   (400) without `ImageId`, `InstanceType`, `MinCount` or `MaxCount`.
//!
//! What EC2 allows, the fake allows: terminating an instance that is
//! already terminating, terminated or gone (`Ok(None)` once it has
//! vanished); stopping a `stopping` or `stopped` instance; disabling the
//! metadata endpoint twice; a tag search that lists `terminated` instances
//! until they vanish; changing an instance (stop, terminate, ...) while
//! describes do not show it yet, since EC2's delay is on reads.
//!
//! Lifecycle: a new instance is `pending`; a stop answers `stopping`, a
//! start `pending`, a terminate `shutting-down`; each settles (`running`,
//! `stopped`, `terminated`) on the describe after
//! [`FakeEc2::set_transition_describes`] describes (none by default). A
//! `terminated` instance is shown for [`DEFAULT_TERMINATED_DESCRIBES`]
//! describes ([`FakeEc2::set_terminated_describes`]), then vanishes, as
//! EC2 drops it after about an hour. A start boots a fresh console: the
//! ready line is gone until the guest writes it again (ADR-0175).
//!
//! Where the fake is stricter than EC2, it says so here: it requires
//! `Placement.AvailabilityZone` (`MissingParameter`), where EC2 would pick
//! a zone; banlieue always names one. Where it is simpler: the vCPU quota is
//! one number, where EC2 counts per instance family bucket; NitroTPM
//! depends on the type only, not also on the AMI's `TpmSupport`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use base64::Engine as _;

use crate::api::Ec2Api;
use crate::console::READY_MARKER;
use crate::error::{AwsFault, Error, Result, classify};
use crate::instance::{self, EkKeyType, MAX_USER_DATA_BYTES, RunInstancesRequest};
use crate::tags::{TAG_OWNER_UID, TagResourceType};
use crate::types::{
    AvailabilityZone, Instance, InstanceState, InstanceTypeInfo, InstanceTypeOffering, TpmEkPub,
};

/// Describes that answer `InvalidInstanceID.NotFound` for a fresh instance
/// (seen once in the spike).
pub const DEFAULT_VISIBILITY_DELAY: u32 = 1;
/// Describes that still show a `terminated` instance before it vanishes.
pub const DEFAULT_TERMINATED_DESCRIBES: u32 = 3;

const HTTP_BAD_REQUEST: u16 = 400;
const HTTP_FORBIDDEN: u16 = 403;
const HTTP_INTERNAL_SERVER_ERROR: u16 = 500;
const HTTP_SERVICE_UNAVAILABLE: u16 = 503;

const CODE_IDEMPOTENT_MISMATCH: &str = "IdempotentParameterMismatch";
const CODE_UNSUPPORTED: &str = "Unsupported";
const CODE_INSUFFICIENT_CAPACITY: &str = "InsufficientInstanceCapacity";
const CODE_VCPU_LIMIT: &str = "VcpuLimitExceeded";
const CODE_UNAUTHORIZED: &str = "UnauthorizedOperation";
const CODE_INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";
const CODE_MISSING_PARAMETER: &str = "MissingParameter";
const CODE_INSTANCE_NOT_FOUND: &str = "InvalidInstanceID.NotFound";
const CODE_INSTANCE_MALFORMED: &str = "InvalidInstanceID.Malformed";
const CODE_THROTTLED: &str = "RequestLimitExceeded";
const CODE_INCORRECT_STATE: &str = "IncorrectInstanceState";
const CODE_UNSUPPORTED_OPERATION: &str = "UnsupportedOperation";
const CODE_INVALID_INSTANCE_TYPE: &str = "InvalidInstanceType";
const CODE_EK_NOT_FOUND: &str = "InstanceTpmEkPub.NotFound";

const P_IMAGE_ID: &str = "ImageId";
const P_INSTANCE_TYPE: &str = "InstanceType";
const P_MIN_COUNT: &str = "MinCount";
const P_MAX_COUNT: &str = "MaxCount";
const P_CLIENT_TOKEN: &str = "ClientToken";
const P_USER_DATA: &str = "UserData";
const P_ZONE: &str = "Placement.AvailabilityZone";
const P_HTTP_TOKENS: &str = "MetadataOptions.HttpTokens";
const P_HOP_LIMIT: &str = "MetadataOptions.HttpPutResponseHopLimit";
const P_HTTP_ENDPOINT: &str = "MetadataOptions.HttpEndpoint";
const P_ROOT_ENCRYPTED: &str = "BlockDeviceMapping.1.Ebs.Encrypted";
const P_PUBLIC_IP: &str = "NetworkInterface.1.AssociatePublicIpAddress";
const P_INSTANCE_PROFILE_PREFIX: &str = "IamInstanceProfile";
const P_TAG_SPECIFICATION_PREFIX: &str = "TagSpecification.";
const P_RESOURCE_TYPE_SUFFIX: &str = ".ResourceType";
/// What EC2 or the fake requires of every `RunInstances`.
const REQUIRED_PARAMS: [&str; 5] = [
    P_IMAGE_ID,
    P_INSTANCE_TYPE,
    P_MIN_COUNT,
    P_MAX_COUNT,
    P_ZONE,
];

const TRUE: &str = "true";
const FALSE: &str = "false";
const DISABLED: &str = "disabled";
const IMDS_TOKENS_REQUIRED: &str = "required";
const IMDS_HOP_LIMIT: &str = "1";
const ZONE_AVAILABLE: &str = "available";
const KEY_FORMAT_DER: &str = "der";

const INSTANCE_ID_PREFIX: &str = "i-";
/// EC2's current instance IDs: `i-` and 17 lowercase hex digits.
const INSTANCE_ID_HEX_DIGITS: usize = 17;
/// The first fake instance number; any value is fine, this one reads as an
/// instance ID rather than as zeroes.
const FIRST_INSTANCE_NUMBER: u64 = 0x0fa_4e00_0000_0001;
const REQUEST_ID_PREFIX: &str = "00000000-0000-4000-8000-";
/// Hex digits of the request ID's last UUID group.
const REQUEST_ID_SUFFIX_DIGITS: usize = 12;
/// AWS keeps the first four addresses of each subnet; the first instance
/// gets `.4`.
const FIRST_HOST_OCTET: u32 = 4;
/// Host octets above this wrap, so a long test cannot build an invalid
/// address.
const MAX_HOST_OCTET: u32 = 254;
const FAKE_SUBNET_PREFIX: &str = "10.0.0.";
const PRIVATE_DNS_SUFFIX: &str = "compute.internal";
/// What prefixes the derived EK input, so the bytes cannot pass for a key
/// derived any other way.
const FAKE_EK_DOMAIN: &[u8] = b"banlieue-fake-ek";
/// A kernel log line before the guest's own, as the serial console has.
const CONSOLE_BOOT_LINE: &str = "[    0.000000] Linux version 6.12.0 (fake)\r\n";
/// The kmsg timestamp the fake writes before the ready line.
const CONSOLE_READY_TIMESTAMP: &str = "[   12.345678] ";
const CONSOLE_LINE_END: &str = "\r\n";

/// The `ca-central-1` preset's zones: name and ID, as in the spike account.
const CA_CENTRAL_1_ZONES: [(&str, &str); 3] = [
    ("ca-central-1a", "cac1-az1"),
    ("ca-central-1b", "cac1-az2"),
    ("ca-central-1d", "cac1-az4"),
];
const CA_CENTRAL_1_TYPES: [&str; 2] = ["t3.small", "m7i.large"];
/// Both preset types have 2 default vCPUs.
const PRESET_VCPUS: u32 = 2;
/// The spike's raised vCPU quota (`L-1216C47A`, default 5).
const PRESET_VCPU_QUOTA: u32 = 16;
const ARCH_X86_64: &str = "x86_64";
const NITRO_TPM_2_0: &str = "2.0";
const BOOT_MODE_LEGACY_BIOS: &str = "legacy-bios";
const BOOT_MODE_UEFI: &str = "uefi";

struct FakeInstance {
    instance_type: String,
    zone_name: String,
    state: InstanceState,
    tags: BTreeMap<String, String>,
    private_ip: String,
    launch_time: jiff::Timestamp,
    /// Describes left that answer not found (eventual consistency).
    hidden_describes: u32,
    /// Describes left that still show the transient state.
    transient_describes: u32,
    /// Describes that showed it `terminated`.
    terminated_shown: u32,
    /// Describes that showed it `running` since its last start.
    running_describes: u32,
    console_ready: bool,
    imds_enabled: bool,
    /// EK reads left that answer not found since the last launch or start.
    ek_not_found_left: u32,
}

impl FakeInstance {
    fn snapshot(&self, instance_id: &str) -> Instance {
        let terminated = self.state == InstanceState::Terminated;
        Instance {
            instance_id: instance_id.to_string(),
            state: self.state.clone(),
            private_ip: (!terminated).then(|| self.private_ip.clone()),
            private_dns_name: (!terminated)
                .then(|| private_dns_name(&self.private_ip, &self.zone_name)),
            zone_name: self.zone_name.clone(),
            launch_time: Some(self.launch_time),
            tags: self.tags.clone(),
        }
    }

    fn is_gone(&self) -> bool {
        self.state.is_gone()
    }

    /// Whether the instance counts against the vCPU quota.
    fn holds_vcpus(&self) -> bool {
        matches!(
            self.state,
            InstanceState::Pending | InstanceState::Running | InstanceState::Stopping
        )
    }
}

/// What a client token was first used for.
struct TokenUse {
    instance_id: String,
    params: BTreeMap<String, String>,
}

#[derive(Default)]
struct State {
    zones: Vec<AvailabilityZone>,
    types: BTreeMap<String, InstanceTypeInfo>,
    /// Zone name to the instance types offered there.
    offerings: BTreeMap<String, BTreeSet<String>>,
    /// (zone name, instance type) pairs with no capacity.
    no_capacity: BTreeSet<(String, String)>,
    vcpu_quota: u32,
    throttle_remaining: u32,
    visibility_delay: u32,
    transition_describes: u32,
    terminated_describes: u32,
    console_ready_after: Option<u32>,
    ek_not_found_reads: u32,
    instances: BTreeMap<String, FakeInstance>,
    tokens: BTreeMap<String, TokenUse>,
    next_instance: u64,
    next_host: u32,
    next_request: u64,
}

impl State {
    fn fault(&mut self, status: u16, code: &str, message: &str) -> Error {
        self.next_request += 1;
        let request_id = format!(
            "{REQUEST_ID_PREFIX}{:0width$x}",
            self.next_request,
            width = REQUEST_ID_SUFFIX_DIGITS
        );
        classify(AwsFault::new(status, code, message, &request_id))
    }

    fn bad_request(&mut self, code: &str, message: &str) -> Error {
        self.fault(HTTP_BAD_REQUEST, code, message)
    }

    /// Spends one throttled call, if any are left.
    fn throttle(&mut self) -> Result<()> {
        if self.throttle_remaining == 0 {
            return Ok(());
        }
        self.throttle_remaining -= 1;
        Err(self.fault(
            HTTP_SERVICE_UNAVAILABLE,
            CODE_THROTTLED,
            "Request limit exceeded.",
        ))
    }

    /// Refuses an ID that is not `i-<hex>`.
    fn check_id(&mut self, instance_id: &str) -> Result<()> {
        let well_formed = instance_id
            .strip_prefix(INSTANCE_ID_PREFIX)
            .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()));
        if well_formed {
            return Ok(());
        }
        Err(self.bad_request(
            CODE_INSTANCE_MALFORMED,
            &format!("Invalid id: \"{instance_id}\""),
        ))
    }

    fn not_found(&mut self, instance_id: &str) -> Error {
        self.bad_request(
            CODE_INSTANCE_NOT_FOUND,
            &format!("The instance ID '{instance_id}' does not exist"),
        )
    }

    fn incorrect_state(&mut self, instance_id: &str, action: &str) -> Error {
        self.bad_request(
            CODE_INCORRECT_STATE,
            &format!(
                "The instance '{instance_id}' is not in a state from which it can be {action}."
            ),
        )
    }

    /// A known, well-formed instance, or the fault EC2 answers.
    fn instance(&mut self, instance_id: &str) -> Result<&mut FakeInstance> {
        self.check_id(instance_id)?;
        if !self.instances.contains_key(instance_id) {
            return Err(self.not_found(instance_id));
        }
        self.instances
            .get_mut(instance_id)
            .ok_or_else(|| Error::Decode(format!("{instance_id} vanished under the lock")))
    }

    fn vcpus_of(&self, instance_type: &str) -> u32 {
        self.types.get(instance_type).map_or(0, |t| t.default_vcpus)
    }

    fn vcpus_in_use(&self) -> u32 {
        self.instances
            .values()
            .filter(|i| i.holds_vcpus())
            .map(|i| self.vcpus_of(&i.instance_type))
            .sum()
    }

    /// Refuses `instance_type` when its vCPUs do not fit the quota.
    fn check_quota(&mut self, instance_type: &str) -> Result<()> {
        if self.vcpus_in_use() + self.vcpus_of(instance_type) <= self.vcpu_quota {
            return Ok(());
        }
        let message = format!(
            "You have requested more vCPU capacity than your current vCPU limit of {} allows for the instance bucket that the specified instance type belongs to. Please visit http://aws.amazon.com/contact-us/ec2-request to request an adjustment to this limit.",
            self.vcpu_quota
        );
        Err(self.bad_request(CODE_VCPU_LIMIT, &message))
    }

    /// One describe of `instance_id`: hidden while fresh, then its state,
    /// settling a transient one; `None` once it has vanished.
    fn observe(&mut self, instance_id: &str) -> Option<Instance> {
        let retention = self.terminated_describes;
        let console_after = self.console_ready_after;
        let instance = self.instances.get_mut(instance_id)?;
        if instance.hidden_describes > 0 {
            instance.hidden_describes -= 1;
            return None;
        }
        settle(instance);
        if instance.state == InstanceState::Terminated {
            if instance.terminated_shown >= retention {
                self.instances.remove(instance_id);
                return None;
            }
            instance.terminated_shown += 1;
        }
        if instance.state == InstanceState::Running {
            instance.running_describes += 1;
            if console_after.is_some_and(|n| instance.running_describes >= n) {
                instance.console_ready = true;
            }
        }
        Some(instance.snapshot(instance_id))
    }

    fn launch(&mut self, params: &BTreeMap<String, String>) -> String {
        let id = format!(
            "{INSTANCE_ID_PREFIX}{:0width$x}",
            FIRST_INSTANCE_NUMBER + self.next_instance,
            width = INSTANCE_ID_HEX_DIGITS
        );
        self.next_instance += 1;
        let host = FIRST_HOST_OCTET + self.next_host % (MAX_HOST_OCTET - FIRST_HOST_OCTET + 1);
        self.next_host += 1;
        let imds_enabled = params.get(P_HTTP_ENDPOINT).is_none_or(|v| v != DISABLED);
        self.instances.insert(
            id.clone(),
            FakeInstance {
                instance_type: param(params, P_INSTANCE_TYPE).to_string(),
                zone_name: param(params, P_ZONE).to_string(),
                state: InstanceState::Pending,
                tags: instance_tags(params),
                private_ip: format!("{FAKE_SUBNET_PREFIX}{host}"),
                launch_time: jiff::Timestamp::now(),
                hidden_describes: self.visibility_delay,
                transient_describes: self.transition_describes,
                terminated_shown: 0,
                running_describes: 0,
                console_ready: false,
                imds_enabled,
                ek_not_found_left: self.ek_not_found_reads,
            },
        );
        id
    }
}

/// Moves a transient state on, once its held describes are spent.
fn settle(instance: &mut FakeInstance) {
    let next = match instance.state {
        InstanceState::Pending => InstanceState::Running,
        InstanceState::Stopping => InstanceState::Stopped,
        InstanceState::ShuttingDown => InstanceState::Terminated,
        _ => return,
    };
    if instance.transient_describes > 0 {
        instance.transient_describes -= 1;
        return;
    }
    instance.state = next;
}

fn param<'a>(params: &'a BTreeMap<String, String>, key: &str) -> &'a str {
    params.get(key).map_or("", String::as_str)
}

/// The tags of the request's `instance` tag specification.
fn instance_tags(params: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let Some(prefix) = params.iter().find_map(|(k, v)| {
        if v != TagResourceType::Instance.as_str() || !k.starts_with(P_TAG_SPECIFICATION_PREFIX) {
            return None;
        }
        k.strip_suffix(P_RESOURCE_TYPE_SUFFIX)
    }) else {
        return BTreeMap::new();
    };
    let mut tags = BTreeMap::new();
    for n in 1.. {
        let Some(key) = params.get(&format!("{prefix}.Tag.{n}.Key")) else {
            break;
        };
        let value = params
            .get(&format!("{prefix}.Tag.{n}.Value"))
            .cloned()
            .unwrap_or_default();
        tags.insert(key.clone(), value);
    }
    tags
}

/// The first condition of the shipped policy that `params` fails, if any.
fn policy_denial(params: &BTreeMap<String, String>) -> Option<&'static str> {
    if param(params, P_HTTP_TOKENS) != IMDS_TOKENS_REQUIRED {
        return Some("ec2:MetadataHttpTokens must be required");
    }
    if param(params, P_HOP_LIMIT) != IMDS_HOP_LIMIT {
        return Some("ec2:MetadataHttpPutResponseHopLimit must be 1");
    }
    if param(params, P_ROOT_ENCRYPTED) != TRUE {
        return Some("ec2:Encrypted must be true");
    }
    if param(params, P_PUBLIC_IP) != FALSE {
        return Some("ec2:AssociatePublicIpAddress must be false");
    }
    if !instance_tags(params).contains_key(TAG_OWNER_UID) {
        return Some("aws:RequestTag/banlieue-owner-uid is required on the instance");
    }
    if params
        .keys()
        .any(|k| k.starts_with(P_INSTANCE_PROFILE_PREFIX))
    {
        return Some("no instance profile may be passed");
    }
    None
}

/// EC2's private DNS form, `ip-10-0-0-4.<region>.compute.internal`, the
/// region being the zone name without its letter.
fn private_dns_name(private_ip: &str, zone_name: &str) -> String {
    let region = zone_name.trim_end_matches(|c: char| c.is_ascii_lowercase());
    format!(
        "ip-{}.{region}.{PRIVATE_DNS_SUFFIX}",
        private_ip.replace('.', "-")
    )
}

/// The fake EK: the SHA-256 of a fixed domain, the instance ID and the key
/// type. **These are not real keys**, nor DER `SubjectPublicKeyInfo`: they
/// are only distinct per instance and key type, and stable across stop and
/// start, as a real EK is.
fn fake_ek(instance_id: &str, key_type: EkKeyType) -> Vec<u8> {
    let mut input = FAKE_EK_DOMAIN.to_vec();
    input.push(0);
    input.extend_from_slice(instance_id.as_bytes());
    input.push(0);
    input.extend_from_slice(key_type.as_str().as_bytes());
    aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &input)
        .as_ref()
        .to_vec()
}

/// Required parameters, user data, and names the region knows.
fn check_parameters(s: &mut State, params: &BTreeMap<String, String>) -> Result<()> {
    if let Some(missing) = REQUIRED_PARAMS
        .into_iter()
        .find(|k| param(params, k).is_empty())
    {
        return Err(s.bad_request(
            CODE_MISSING_PARAMETER,
            &format!("The request must contain the parameter {missing}"),
        ));
    }
    if let Some(encoded) = params.get(P_USER_DATA) {
        let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
            return Err(s.bad_request(
                CODE_INVALID_PARAMETER_VALUE,
                "Invalid BASE64 encoding of user data.",
            ));
        };
        if raw.len() > MAX_USER_DATA_BYTES {
            return Err(s.bad_request(
                CODE_INVALID_PARAMETER_VALUE,
                &format!("User data is limited to {MAX_USER_DATA_BYTES} bytes"),
            ));
        }
    }
    let zone = param(params, P_ZONE);
    if !s.zones.iter().any(|z| z.name == zone) {
        return Err(s.bad_request(
            CODE_INVALID_PARAMETER_VALUE,
            &format!("Invalid availability zone: [{zone}]"),
        ));
    }
    let instance_type = param(params, P_INSTANCE_TYPE);
    if !s.types.contains_key(instance_type) {
        return Err(s.bad_request(
            CODE_INVALID_PARAMETER_VALUE,
            &format!("Invalid value '{instance_type}' for InstanceType."),
        ));
    }
    Ok(())
}

/// Offering, capacity and quota for a new instance.
fn check_placement(s: &mut State, params: &BTreeMap<String, String>) -> Result<()> {
    let zone = param(params, P_ZONE).to_string();
    let instance_type = param(params, P_INSTANCE_TYPE).to_string();
    let offered_in: Vec<String> = s
        .zones
        .iter()
        .filter(|z| {
            s.offerings
                .get(&z.name)
                .is_some_and(|t| t.contains(&instance_type))
        })
        .map(|z| z.name.clone())
        .collect();
    if !offered_in.contains(&zone) {
        let message = format!(
            "Your requested instance type ({instance_type}) is not supported in your requested Availability Zone ({zone}). Please retry your request by not specifying an Availability Zone or choosing {}.",
            offered_in.join(", ")
        );
        return Err(s.bad_request(CODE_UNSUPPORTED, &message));
    }
    if s.no_capacity
        .contains(&(zone.clone(), instance_type.clone()))
    {
        let message = format!(
            "We currently do not have sufficient {instance_type} capacity in the Availability Zone you requested ({zone}). Our system will be working on provisioning additional capacity."
        );
        return Err(s.fault(
            HTTP_INTERNAL_SERVER_ERROR,
            CODE_INSUFFICIENT_CAPACITY,
            &message,
        ));
    }
    s.check_quota(&instance_type)
}

/// In-memory [`Ec2Api`] that refuses what EC2 refuses (see the module
/// docs).
pub struct FakeEc2 {
    state: Mutex<State>,
}

impl std::fmt::Debug for FakeEc2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeEc2")
            .field("instances", &self.instance_count())
            .finish_non_exhaustive()
    }
}

impl Default for FakeEc2 {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeEc2 {
    /// A region with no zones, no instance types and a vCPU quota of 0.
    /// Configure it with [`Self::add_zone`], [`Self::add_instance_type`],
    /// [`Self::set_offered`] and [`Self::set_vcpu_quota`], or start from
    /// [`Self::ca_central_1`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                visibility_delay: DEFAULT_VISIBILITY_DELAY,
                terminated_describes: DEFAULT_TERMINATED_DESCRIBES,
                ..State::default()
            }),
        }
    }

    /// The spike's region: zones `ca-central-1a`, `-1b` and `-1d` (IDs
    /// `cac1-az1`, `-az2` and `-az4`); `t3.small` and `m7i.large` (2 vCPUs,
    /// `x86_64`, NitroTPM 2.0, legacy BIOS and UEFI) offered in all three;
    /// a vCPU quota of 16, as raised in the spike.
    #[must_use]
    pub fn ca_central_1() -> Self {
        let fake = Self::new();
        for (name, id) in CA_CENTRAL_1_ZONES {
            fake.add_zone(name, id);
        }
        for instance_type in CA_CENTRAL_1_TYPES {
            fake.add_instance_type(InstanceTypeInfo {
                instance_type: instance_type.to_string(),
                default_vcpus: PRESET_VCPUS,
                architectures: vec![ARCH_X86_64.to_string()],
                nitro_tpm_supported: true,
                nitro_tpm_versions: vec![NITRO_TPM_2_0.to_string()],
                boot_modes: vec![
                    BOOT_MODE_LEGACY_BIOS.to_string(),
                    BOOT_MODE_UEFI.to_string(),
                ],
            });
            for (zone, _) in CA_CENTRAL_1_ZONES {
                fake.set_offered(zone, instance_type, true);
            }
        }
        fake.set_vcpu_quota(PRESET_VCPU_QUOTA);
        fake
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panicking test leaves the fake poisoned; the data is still usable.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The lock, after spending a throttled call if any are left.
    fn call(&self) -> Result<MutexGuard<'_, State>> {
        let mut s = self.lock();
        s.throttle()?;
        Ok(s)
    }

    // ----- configuration -------------------------------------------------

    /// Adds an `available` zone with its account name and global ID.
    pub fn add_zone(&self, name: &str, id: &str) {
        self.lock().zones.push(AvailabilityZone {
            name: name.to_string(),
            id: id.to_string(),
            state: ZONE_AVAILABLE.to_string(),
        });
    }

    /// Adds (or replaces) an instance type: its vCPUs count against the
    /// quota, and its NitroTPM support decides [`Ec2Api::tpm_ek_pub`].
    pub fn add_instance_type(&self, info: InstanceTypeInfo) {
        self.lock().types.insert(info.instance_type.clone(), info);
    }

    /// Offers `instance_type` in zone `zone_name`, or withdraws it.
    pub fn set_offered(&self, zone_name: &str, instance_type: &str, offered: bool) {
        let mut s = self.lock();
        let types = s.offerings.entry(zone_name.to_string()).or_default();
        if offered {
            types.insert(instance_type.to_string());
            return;
        }
        types.remove(instance_type);
    }

    /// Runs zone `zone_name` out of capacity for `instance_type`, or
    /// restores it.
    pub fn set_insufficient_capacity(&self, zone_name: &str, instance_type: &str, short: bool) {
        let key = (zone_name.to_string(), instance_type.to_string());
        let mut s = self.lock();
        if short {
            s.no_capacity.insert(key);
            return;
        }
        s.no_capacity.remove(&key);
    }

    /// The account's vCPU quota for On-Demand instances.
    pub fn set_vcpu_quota(&self, vcpus: u32) {
        self.lock().vcpu_quota = vcpus;
    }

    /// The next `calls` calls, of any method, answer `RequestLimitExceeded`.
    pub fn throttle_next(&self, calls: u32) {
        self.lock().throttle_remaining = calls;
    }

    /// Describes that answer not found for instances launched from now on
    /// ([`DEFAULT_VISIBILITY_DELAY`] by default).
    pub fn set_visibility_delay(&self, describes: u32) {
        self.lock().visibility_delay = describes;
    }

    /// Describes that still show `pending`, `stopping` or `shutting-down`
    /// after the call that entered it, for transitions from now on (0 by
    /// default: the next describe shows the settled state).
    pub fn set_transition_describes(&self, describes: u32) {
        self.lock().transition_describes = describes;
    }

    /// Describes that show a `terminated` instance before it vanishes
    /// ([`DEFAULT_TERMINATED_DESCRIBES`] by default).
    pub fn set_terminated_describes(&self, describes: u32) {
        self.lock().terminated_describes = describes;
    }

    /// The guest writes its ready line after this many describes showing
    /// it `running` since its last start; `None` (the default) waits for
    /// [`Self::guest_reports_ready`].
    pub fn set_console_ready_after_describes(&self, describes: Option<u32>) {
        self.lock().console_ready_after = describes;
    }

    /// The first `reads` EK reads of an instance after each launch or start,
    /// once it is past `pending`, answer `InstanceTpmEkPub.NotFound`, as EC2
    /// did for an instance that had just reached `running` (ADR-0167
    /// Decision 1). 0 by default; applies to launches and starts from now
    /// on.
    pub fn set_ek_not_found_reads(&self, reads: u32) {
        self.lock().ek_not_found_reads = reads;
    }

    /// The guest wrote its ready line to `/dev/kmsg` (ADR-0175): the
    /// console now carries it, until the next start.
    pub fn guest_reports_ready(&self, instance_id: &str) {
        if let Some(instance) = self.lock().instances.get_mut(instance_id) {
            instance.console_ready = true;
        }
    }

    // ----- inspection ----------------------------------------------------

    /// Instances the fake holds, vanished ones excepted.
    #[must_use]
    pub fn instance_count(&self) -> usize {
        self.lock().instances.len()
    }

    /// The instance's state without a describe (nothing advances, nothing
    /// is hidden); `None` when unknown or vanished.
    #[must_use]
    pub fn peek_state(&self, instance_id: &str) -> Option<InstanceState> {
        self.lock()
            .instances
            .get(instance_id)
            .map(|i| i.state.clone())
    }

    /// Whether the instance's metadata endpoint is on; `None` when unknown
    /// or vanished.
    #[must_use]
    pub fn imds_enabled(&self, instance_id: &str) -> Option<bool> {
        self.lock()
            .instances
            .get(instance_id)
            .map(|i| i.imds_enabled)
    }

    // ----- RunInstances at the Query level -------------------------------

    /// `RunInstances` with these Query parameters, as EC2 receives them.
    /// [`Ec2Api::run_instance`] delegates here with
    /// [`RunInstancesRequest::to_params`]; tests call it directly to send
    /// what the builder never would, and see EC2 and the policy refuse it.
    ///
    /// # Errors
    /// Each launch refusal in the module docs, in EC2's order: throttling,
    /// the policy, the parameters, the client token, then offering,
    /// capacity and quota.
    pub fn run_instance_params(&self, params: &[(String, String)]) -> Result<String> {
        let params: BTreeMap<String, String> = params.iter().cloned().collect();
        let mut s = self.call()?;
        if let Some(reason) = policy_denial(&params) {
            let message = format!(
                "You are not authorized to perform this operation. Encoded authorization failure message: (fake) {reason}"
            );
            return Err(s.fault(HTTP_FORBIDDEN, CODE_UNAUTHORIZED, &message));
        }
        check_parameters(&mut s, &params)?;
        let token = param(&params, P_CLIENT_TOKEN).to_string();
        if let Some(used) = s.tokens.get(&token) {
            if used.params == params {
                return Ok(used.instance_id.clone());
            }
            return Err(s.bad_request(
                CODE_IDEMPOTENT_MISMATCH,
                "Arguments on this idempotent request are inconsistent with arguments used in previous request(s).",
            ));
        }
        check_placement(&mut s, &params)?;
        let id = s.launch(&params);
        if !token.is_empty() {
            s.tokens.insert(
                token,
                TokenUse {
                    instance_id: id.clone(),
                    params,
                },
            );
        }
        Ok(id)
    }
}

#[async_trait]
impl Ec2Api for FakeEc2 {
    async fn run_instance(&self, request: &RunInstancesRequest) -> Result<String> {
        let params = request.to_params()?;
        self.run_instance_params(&params)
    }

    async fn describe_instance(&self, instance_id: &str) -> Result<Option<Instance>> {
        instance::describe_instance(instance_id)?;
        let mut s = self.call()?;
        s.check_id(instance_id)?;
        Ok(s.observe(instance_id))
    }

    async fn find_instances_by_tag(&self, key: &str, value: &str) -> Result<Vec<Instance>> {
        instance::describe_instances_by_tag(key, value)?;
        let mut s = self.call()?;
        let ids: Vec<String> = s
            .instances
            .iter()
            .filter(|(_, i)| i.tags.get(key).is_some_and(|v| v == value))
            .map(|(id, _)| id.clone())
            .collect();
        Ok(ids.iter().filter_map(|id| s.observe(id)).collect())
    }

    async fn terminate_instance(&self, instance_id: &str) -> Result<Option<InstanceState>> {
        instance::terminate_instances(instance_id)?;
        let mut s = self.call()?;
        s.check_id(instance_id)?;
        let transition = s.transition_describes;
        let Some(instance) = s.instances.get_mut(instance_id) else {
            return Ok(None);
        };
        if !instance.is_gone() {
            instance.state = InstanceState::ShuttingDown;
            instance.transient_describes = transition;
        }
        Ok(Some(instance.state.clone()))
    }

    async fn stop_instance(&self, instance_id: &str) -> Result<InstanceState> {
        instance::stop_instances(instance_id)?;
        let mut s = self.call()?;
        let transition = s.transition_describes;
        let instance = s.instance(instance_id)?;
        match instance.state {
            InstanceState::Running => {
                instance.state = InstanceState::Stopping;
                instance.transient_describes = transition;
                Ok(InstanceState::Stopping)
            }
            InstanceState::Stopping | InstanceState::Stopped => Ok(instance.state.clone()),
            _ => Err(s.incorrect_state(instance_id, "stopped")),
        }
    }

    async fn start_instance(&self, instance_id: &str) -> Result<InstanceState> {
        instance::start_instances(instance_id)?;
        let mut s = self.call()?;
        let instance = s.instance(instance_id)?;
        let state = instance.state.clone();
        let instance_type = instance.instance_type.clone();
        match state {
            InstanceState::Pending => return Ok(InstanceState::Pending),
            InstanceState::Running => return Ok(InstanceState::Running),
            InstanceState::Stopped => {}
            _ => return Err(s.incorrect_state(instance_id, "started")),
        }
        s.check_quota(&instance_type)?;
        let transition = s.transition_describes;
        let ek_not_found = s.ek_not_found_reads;
        let instance = s.instance(instance_id)?;
        instance.ek_not_found_left = ek_not_found;
        instance.state = InstanceState::Pending;
        instance.transient_describes = transition;
        instance.launch_time = jiff::Timestamp::now();
        instance.running_describes = 0;
        instance.console_ready = false;
        Ok(InstanceState::Pending)
    }

    async fn console_output(&self, instance_id: &str) -> Result<String> {
        instance::get_console_output(instance_id)?;
        let mut s = self.call()?;
        if !s.instance(instance_id)?.console_ready {
            return Ok(String::new());
        }
        Ok(format!(
            "{CONSOLE_BOOT_LINE}{CONSOLE_READY_TIMESTAMP}{READY_MARKER}{instance_id}{CONSOLE_LINE_END}"
        ))
    }

    async fn tpm_ek_pub(&self, instance_id: &str, key_type: EkKeyType) -> Result<TpmEkPub> {
        instance::get_instance_tpm_ek_pub(instance_id, key_type)?;
        let mut s = self.call()?;
        let instance = s.instance(instance_id)?;
        let gone = instance.is_gone();
        let pending = instance.state == InstanceState::Pending;
        let instance_type = instance.instance_type.clone();
        if gone {
            return Err(s.incorrect_state(instance_id, "queried"));
        }
        let supported = s
            .types
            .get(&instance_type)
            .is_some_and(|t| t.nitro_tpm_supported);
        if !supported {
            return Err(s.bad_request(
                CODE_UNSUPPORTED_OPERATION,
                &format!("The instance '{instance_id}' does not have NitroTPM enabled."),
            ));
        }
        let not_yet = pending || {
            let instance = s.instance(instance_id)?;
            let refuse = instance.ek_not_found_left > 0;
            instance.ek_not_found_left = instance.ek_not_found_left.saturating_sub(1);
            refuse
        };
        if not_yet {
            return Err(s.bad_request(
                CODE_EK_NOT_FOUND,
                &format!("The TPM EK public key for instance '{instance_id}' was not found."),
            ));
        }
        Ok(TpmEkPub {
            instance_id: instance_id.to_string(),
            key_type,
            key_format: KEY_FORMAT_DER.to_string(),
            key_value: fake_ek(instance_id, key_type),
        })
    }

    async fn disable_instance_metadata(&self, instance_id: &str) -> Result<()> {
        instance::disable_instance_metadata(instance_id)?;
        let mut s = self.call()?;
        if s.instance(instance_id)?.is_gone() {
            return Err(s.incorrect_state(instance_id, "modified"));
        }
        s.instance(instance_id)?.imds_enabled = false;
        Ok(())
    }

    async fn availability_zones(&self) -> Result<Vec<AvailabilityZone>> {
        let s = self.call()?;
        Ok(s.zones.clone())
    }

    async fn instance_type_offerings(
        &self,
        instance_types: &[String],
    ) -> Result<Vec<InstanceTypeOffering>> {
        instance::describe_instance_type_offerings(instance_types)?;
        let s = self.call()?;
        let mut offerings = Vec::new();
        for zone in &s.zones {
            let Some(offered) = s.offerings.get(&zone.name) else {
                continue;
            };
            offerings.extend(
                instance_types
                    .iter()
                    .filter(|t| offered.contains(*t))
                    .map(|t| InstanceTypeOffering {
                        instance_type: t.clone(),
                        location: zone.id.clone(),
                    }),
            );
        }
        Ok(offerings)
    }

    async fn instance_types(&self, instance_types: &[String]) -> Result<Vec<InstanceTypeInfo>> {
        instance::describe_instance_types(instance_types)?;
        let mut s = self.call()?;
        let unknown: Vec<String> = instance_types
            .iter()
            .filter(|t| !s.types.contains_key(*t))
            .cloned()
            .collect();
        if !unknown.is_empty() {
            let message = format!(
                "The following supplied instance types do not exist: [{}]",
                unknown.join(", ")
            );
            return Err(s.bad_request(CODE_INVALID_INSTANCE_TYPE, &message));
        }
        Ok(instance_types
            .iter()
            .filter_map(|t| s.types.get(t).cloned())
            .collect())
    }
}

#[cfg(test)]
#[path = "fake_tests.rs"]
mod fake_tests;
