// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `EC2Machine` reconciler: a scheduled VM becomes a hardened EC2 instance
//! (ADR-0174 Decision 3).
//!
//! ```text
//! describe the stored instance, or adopt one by tag, or RunInstances
//!   ->  observe state, address, zone  ->  GuestReady from the console
//!   ->  IMDS off  ->  EK public keys  ->  power  ->  patch status
//! ```
//!
//! The decisions live in [`converge`] and [`finalize`], which take
//! `&dyn Ec2Api` and no Kubernetes client, so the whole lifecycle is
//! unit-tested against `FakeEc2`. [`reconcile`] only wires them to the API
//! server.
//!
//! # Never blocking on EC2
//!
//! EC2 has no operation handles: `RunInstances`, `StopInstances`,
//! `StartInstances` and `TerminateInstances` return at once. A create stores
//! the instance ID in `status.instanceId` and returns; later reconciles
//! describe it. A booting instance is requeued on ADR-0043 Decision 8's
//! cadence: every [`FAST_POLL`] for [`FAST_POLL_WINDOW`] after its launch,
//! then every [`SLOW_POLL`] (the serial console refreshes about every 30 s).
//!
//! # Idempotent create and adoption
//!
//! The create's `ClientToken` is derived from the object's UID and its zone
//! ([`client_token_for`]): a retry in the same zone returns the same
//! instance, so a create whose response (or status patch) was lost makes no
//! second instance. Before any create the instance is also looked up by its
//! `banlieue-owner-uid` tag and adopted (ADR-0158 Decision 4). EC2 is
//! eventually consistent: an ID `RunInstances` just returned may describe as
//! not found for a few seconds, which reads as "not visible yet", never as
//! gone (ADR-0174 Decision 2).
//!
//! An instance ID is never evidence of ownership; the `banlieue-owner-uid`
//! tag is. A stored ID whose instance carries another owner's tag, or none,
//! is dropped and reported (`ForeignResource`), never observed, powered or
//! terminated.
//!
//! # Hardening and the `Provider`'s settings
//!
//! Every instance is IMDSv2-only with a hop limit of 1, an encrypted root
//! volume deleted with it, no public IP and no instance profile
//! ([`RunInstancesRequest`] adds these and has no field to remove them). The
//! `Provider`'s extra tags go on the instance, its volume and its interface
//! beside the ownership tags (which always win), and its KMS key encrypts
//! the root volume. Both are read from the `Provider` at create time, so
//! whoever writes the `EC2Machine` cannot drop them; `spec.kmsKeyId` applies
//! only when the `Provider` names none.
//!
//! # After the guest is ready
//!
//! - `GuestReady` is the image's line on the serial console naming **this**
//!   instance (ADR-0175); a line naming another instance is reported and
//!   never taken. `status.guestInstalled` is sticky (ADR-0043 Decision 5).
//! - IMDS is then turned off, once (`status.imdsDisabled`), and never back
//!   on (ADR-0174 Decision 3).
//! - With `tpmEnabled`, the NitroTPM's RSA-2048 and ECC P-384 endorsement
//!   public keys are read host-side and published as PEM (ADR-0167). EC2 can
//!   answer not found for an instance that just reached `running`: that is
//!   "not yet", read again soon.
//!
//! # Conditions
//!
//! - `Ready`: the instance exists, has run, and is at its desired power
//!   state.
//! - `GuestReady`: the console ready line (above).
//! - `VMCreated`: `True` once `RunInstances` returned or an instance was
//!   adopted. `False` after a refused create; its `lastTransitionTime` is the
//!   time of the latest failure and the create is not retried for
//!   [`create_backoff`] of `status.createFailures` (5 minutes, doubling to an
//!   hour) unless the spec changes (ADR-0173 Decision 5).
//!
//! # A zone that refuses the instance
//!
//! `InsufficientInstanceCapacity` and `Unsupported` (the type not offered in
//! the zone) are `VMCreated=False` with reason `ZoneUnavailable`, the
//! contract in `banlieue_api::common::allocation`. The provider never moves
//! the machine: the controller reads that condition and re-places it in
//! another zone when its placement allows, deleting this object (ADR-0173).
//! When it cannot, this object stays and the create is retried here after
//! the back-off.
//!
//! # An instance that ends outside banlieue
//!
//! An instance terminated by someone else is reported (`Ready=False`,
//! `InstanceTerminated`) and not replaced: its client token would return the
//! terminated instance for a while, and its disk is gone anyway. Deleting
//! the `EC2Machine` (or the pool replacing its member) is the recovery.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use banlieue_api::aws::is_instance_id;
use banlieue_api::banlieue::Provider;
use banlieue_api::common::{
    MachineAddress, MachineAddressType, PowerState, allocation, condition_types,
};
use banlieue_api::infrastructure::{
    EC2Machine, EC2MachineSpec, EC2MachineStatus, Ec2VolumeType, ec2_provider_id,
};
use banlieue_aws::console::{ReadyState, ready_state};
use banlieue_aws::instance::{EkKeyType, RunInstancesRequest, VolumeType, client_token_for};
use banlieue_aws::tags::{OwnershipTags, TAG_OWNER_UID, owned_by};
use banlieue_aws::types::{Instance, InstanceState, to_pem_spki};
use banlieue_aws::{Ec2Api, Error as AwsError};
use banlieue_provider_sdk::finalizer::{ensure_finalizer, remove_finalizer};
use banlieue_provider_sdk::ssa::{FIELD_MANAGER_PROVIDER_EC2, apply_provider_id, apply_status};
use banlieue_provider_sdk::status::{condition_status, find_condition, set_condition};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube::{ResourceExt, api::Api, runtime::controller::Action};
use tracing::{debug, info, warn};

use crate::client::checked_ec2_connection;
use crate::context::Context;
use crate::error::{Error, Result};
use crate::reconciler::instances::{OWNER_KIND, owned_instances};

/// Finalizer holding an `EC2Machine` until its instances are terminated.
pub const MACHINE_FINALIZER: &str = "banlieue.io/ec2machine";
/// Condition type: the instance exists (or why creating it failed). The
/// controller reads it (ADR-0173).
pub const VM_CREATED: &str = allocation::VM_CREATED;
/// How soon a booting instance is read again (ADR-0043 Decision 8).
pub const FAST_POLL: Duration = Duration::from_secs(2);
/// How long after a launch or start the fast cadence lasts: longer than the
/// measured create-to-ready times of a pre-laid image (49.9 s) and of the
/// first unsealed images (144 to 156 s), ADR-0174 finding 9.
pub const FAST_POLL_WINDOW: Duration = Duration::from_secs(180);
/// The cadence past [`FAST_POLL_WINDOW`]: the serial console refreshes about
/// every 30 s (ADR-0174 finding 6), so polling faster sees nothing new.
pub const SLOW_POLL: Duration = Duration::from_secs(30);
/// How soon a terminating instance is read again during deletion.
pub const DELETE_POLL: Duration = Duration::from_secs(10);
/// How long the first refused create waits before it is tried again
/// (ADR-0161 Decision 2 starts its back-off at 5 minutes).
pub const CREATE_RETRY_BACKOFF: Duration = Duration::from_secs(300);
/// The longest wait between create attempts (ADR-0173 Decision 5).
pub const CREATE_RETRY_BACKOFF_MAX: Duration = Duration::from_secs(3600);
/// Each consecutive failed create multiplies the wait by this.
pub const CREATE_RETRY_BACKOFF_FACTOR: u32 = 2;
/// How often a settled instance is observed again.
pub const STEADY_REQUEUE: Duration = Duration::from_secs(300);
/// After EC2 throttled a call: the client has already retried it with
/// jittered back-off (`banlieue_aws::retry`), so the next pass waits longer.
pub const THROTTLED_REQUEUE: Duration = Duration::from_secs(30);
/// After an AWS failure that may pass (a 5xx, the transport, the credential).
/// Not the SDK's short error requeue: a refused credential would otherwise
/// send the ServiceAccount token to STS every few seconds per machine.
pub const AWS_ERROR_REQUEUE: Duration = Duration::from_secs(30);
/// How long after `RunInstances` returned an instance ID that describes as
/// not found is still "not visible yet" rather than gone, during deletion.
/// EC2's read delay is seconds (ADR-0174 Decision 2).
pub const VISIBILITY_GRACE: Duration = Duration::from_secs(300);

/// Stable `reason` strings on `Ready`, `GuestReady` and `VMCreated`.
pub mod reasons {
    /// The instance is running (or stopped, as desired).
    pub const INSTANCE_READY: &str = "InstanceReady";
    /// The instance exists.
    pub const INSTANCE_EXISTS: &str = "InstanceExists";
    /// `spec.validate()` refused the spec; nothing was sent to EC2.
    pub const INVALID_SPEC: &str = "InvalidSpec";
    /// `RunInstances` returned the instance; it is not visible yet.
    pub const CREATING: &str = "Creating";
    /// The instance is `pending`.
    pub const INSTANCE_PENDING: &str = "InstancePending";
    /// No capacity, or the type not offered, in this zone (ADR-0173): the
    /// controller re-places it elsewhere when it can.
    pub const ZONE_UNAVAILABLE: &str = banlieue_api::common::allocation::ZONE_UNAVAILABLE;
    /// The account's vCPU or instance quota would be exceeded.
    pub const QUOTA_EXCEEDED: &str = "QuotaExceeded";
    /// EC2 or the role's policy refused the create for another reason.
    pub const CREATE_REFUSED: &str = "CreateRefused";
    /// The stored instance ID names an instance not tagged as this machine's.
    pub const FOREIGN_RESOURCE: &str = "ForeignResource";
    /// EC2 throttled the call.
    pub const THROTTLED: &str = "Throttled";
    /// EC2 or STS failed or was unreachable.
    pub const AWS_ERROR: &str = "AwsError";
    /// The instance was terminated outside banlieue.
    pub const INSTANCE_TERMINATED: &str = "InstanceTerminated";
    /// Not yet at the desired power state.
    pub const AWAITING_POWER_STATE: &str = "AwaitingPowerState";
    /// `GuestReady=True`: the console carries this instance's ready line.
    pub const GUEST_REPORTED_READY: &str = "GuestReportedReady";
    /// `GuestReady=False`: no ready line yet.
    pub const AWAITING_GUEST: &str = "AwaitingGuest";
    /// `GuestReady=False`: a ready line names another instance, which is
    /// never taken as this one's.
    pub const READY_LINE_MISMATCH: &str = "ReadyLineMismatch";
}

/// Reasons after which a create waits for [`create_backoff`].
const BACKOFF_REASONS: [&str; 4] = [
    reasons::ZONE_UNAVAILABLE,
    reasons::QUOTA_EXCEEDED,
    reasons::CREATE_REFUSED,
    reasons::FOREIGN_RESOURCE,
];

/// What a `Provider` puts on every instance it creates, from its
/// `spec.connection.ec2`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The `Provider`'s namespace.
    pub provider_namespace: String,
    /// The `Provider`'s name.
    pub provider_name: String,
    /// Extra tags on every instance, volume and interface, beside the
    /// ownership tags.
    pub extra_tags: BTreeMap<String, String>,
    /// The KMS key for every root volume; the account's default EBS key when
    /// `None` (and `spec.kmsKeyId` is unset).
    pub kms_key_id: Option<String>,
}

impl Target {
    /// The target `provider` declares.
    ///
    /// # Errors
    /// [`Error::Invalid`] per [`checked_ec2_connection`].
    pub fn from_provider(provider: &Provider) -> Result<Self> {
        let ec2 = checked_ec2_connection(&provider.spec.connection)?;
        Ok(Self {
            provider_namespace: provider.namespace().unwrap_or_default(),
            provider_name: provider.name_any(),
            extra_tags: ec2.tags.clone(),
            kms_key_id: ec2.kms_key_id.clone(),
        })
    }

    /// The ownership tags for the machine with `uid` in `namespace`.
    #[must_use]
    pub fn ownership(&self, uid: &str, namespace: &str) -> OwnershipTags {
        OwnershipTags::new(
            uid,
            OWNER_KIND,
            &self.provider_namespace,
            &self.provider_name,
        )
        .with_owner_namespace(namespace)
    }
}

/// Who a machine is.
#[derive(Debug, Clone, Copy)]
pub struct MachineRef<'a> {
    /// `metadata.name`: the instance's `Name` tag, for humans.
    pub name: &'a str,
    /// `metadata.uid`: the client token and the ownership tag.
    pub uid: &'a str,
    /// `metadata.namespace`: tagged, so a home-only sweep can tell its own
    /// machines' instances from a tenant namespace's.
    pub namespace: &'a str,
}

/// What one convergence pass decided.
#[derive(Clone, Debug, PartialEq)]
pub struct Pass {
    /// The status to apply.
    pub status: EC2MachineStatus,
    /// `spec.providerID` to set, once the instance ID is known.
    pub provider_id: Option<String>,
    /// When to reconcile again.
    pub requeue: Duration,
}

/// The EBS volume type of `volume_type`.
#[must_use]
pub fn volume_type(volume_type: Ec2VolumeType) -> VolumeType {
    match volume_type {
        Ec2VolumeType::Gp3 => VolumeType::Gp3,
        Ec2VolumeType::Gp2 => VolumeType::Gp2,
        Ec2VolumeType::Io1 => VolumeType::Io1,
        Ec2VolumeType::Io2 => VolumeType::Io2,
    }
}

/// The `RunInstances` request for machine `m` with `spec` under `target`:
/// one instance in `spec.zone`, its client token from the UID and the zone,
/// tagged on the instance, its root volume and its interface with the
/// ownership tags, the `Name` tag (the machine's name) and the `Provider`'s
/// extra tags.
///
/// # Errors
/// [`banlieue_aws::Error::Invalid`] when the tags break EC2's rules or the
/// request fails [`RunInstancesRequest::validate`].
pub fn run_request(
    target: &Target,
    m: &MachineRef<'_>,
    spec: &EC2MachineSpec,
) -> banlieue_aws::Result<RunInstancesRequest> {
    let tags = target.ownership(m.uid, m.namespace).resource_tags(
        Some(m.name),
        None,
        &target.extra_tags,
    )?;
    let request = RunInstancesRequest {
        image_id: spec.image_id.clone(),
        instance_type: spec.instance_type.clone(),
        zone_name: spec.zone.clone(),
        subnet_id: spec.subnet_id.clone(),
        security_group_ids: spec.security_group_ids.clone(),
        root_device_name: spec.root_device_name.clone(),
        root_volume_size_gib: spec.root_volume_gi_b,
        volume_type: volume_type(spec.volume_type),
        iops: spec.iops,
        throughput: spec.throughput,
        kms_key_id: target
            .kms_key_id
            .clone()
            .or_else(|| spec.kms_key_id.clone()),
        user_data: spec.user_data.clone().unwrap_or_default().into_bytes(),
        client_token: client_token_for(m.uid, &spec.zone),
        instance_tags: tags.clone(),
        volume_tags: tags.clone(),
        network_interface_tags: tags,
    };
    request.validate()?;
    Ok(request)
}

/// The power state an instance reads as: `pending` and `running` are
/// `PoweredOn`, `stopping` and `stopped` are `PoweredOff`; `None` for a
/// terminating, terminated or undocumented state.
#[must_use]
pub fn observed_power(state: &InstanceState) -> Option<PowerState> {
    match state {
        InstanceState::Pending | InstanceState::Running => Some(PowerState::PoweredOn),
        InstanceState::Stopping | InstanceState::Stopped => Some(PowerState::PoweredOff),
        InstanceState::ShuttingDown | InstanceState::Terminated | InstanceState::Other(_) => None,
    }
}

/// Whether `state` is `desired`: `PoweredOff` means **stopped**, which ends
/// compute billing.
#[must_use]
pub fn at_desired_power(desired: &PowerState, state: &InstanceState) -> bool {
    match desired {
        PowerState::PoweredOn => *state == InstanceState::Running,
        PowerState::PoweredOff => *state == InstanceState::Stopped,
        PowerState::Suspended => false,
    }
}

/// A power change: start or stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerChange {
    /// `StartInstances`.
    Start,
    /// `StopInstances`.
    Stop,
}

/// The power change that moves `state` toward `desired`, if any. An
/// instance in transition (`pending`, `stopping`) is left to finish.
#[must_use]
pub fn power_change(desired: &PowerState, state: &InstanceState) -> Option<PowerChange> {
    match (desired, state) {
        (PowerState::PoweredOn, InstanceState::Stopped) => Some(PowerChange::Start),
        (PowerState::PoweredOff, InstanceState::Running) => Some(PowerChange::Stop),
        _ => None,
    }
}

/// When to look at a booting instance again (ADR-0043 Decision 8): every
/// [`FAST_POLL`] while it was launched (or started) less than
/// [`FAST_POLL_WINDOW`] ago, or when its launch time is unknown; every
/// [`SLOW_POLL`] after.
#[must_use]
pub fn guest_poll_interval(launched: Option<jiff::Timestamp>, now: jiff::Timestamp) -> Duration {
    let Some(launched) = launched else {
        return FAST_POLL;
    };
    match Duration::try_from(now.duration_since(launched)) {
        Ok(age) if age >= FAST_POLL_WINDOW => SLOW_POLL,
        _ => FAST_POLL,
    }
}

/// The instance's addresses, as CAPI reports them: the primary private IPv4
/// (`InternalIP`) and the private DNS name (`InternalDNS`).
#[must_use]
pub fn addresses(instance: &Instance) -> Vec<MachineAddress> {
    let ip = instance.private_ip.iter().map(|address| MachineAddress {
        address_type: MachineAddressType::InternalIP,
        address: address.clone(),
    });
    let dns = instance
        .private_dns_name
        .iter()
        .map(|address| MachineAddress {
            address_type: MachineAddressType::InternalDNS,
            address: address.clone(),
        });
    ip.chain(dns).collect()
}

/// How long to wait after the `failures`-th consecutive failed create:
/// [`CREATE_RETRY_BACKOFF`] doubled for each failure after the first, at
/// most [`CREATE_RETRY_BACKOFF_MAX`] (ADR-0173 Decision 5). Zero failures
/// waits as one does.
#[must_use]
pub fn create_backoff(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1);
    CREATE_RETRY_BACKOFF_FACTOR
        .checked_pow(doublings)
        .map_or(CREATE_RETRY_BACKOFF_MAX, |factor| {
            CREATE_RETRY_BACKOFF
                .saturating_mul(factor)
                .min(CREATE_RETRY_BACKOFF_MAX)
        })
}

/// Time left before a failed create may be tried again: `Some` only while
/// `VMCreated` is `False` for a back-off reason, for this generation, and
/// younger than [`create_backoff`] of `status.createFailures`.
#[must_use]
pub fn create_backoff_remaining(
    status: &EC2MachineStatus,
    generation: i64,
    now: jiff::Timestamp,
) -> Option<Duration> {
    let c = find_condition(&status.conditions, VM_CREATED)?;
    if c.status != condition_status::FALSE
        || !BACKOFF_REASONS.contains(&c.reason.as_str())
        || c.observed_generation != Some(generation)
    {
        return None;
    }
    let elapsed = Duration::try_from(now.duration_since(c.last_transition_time.0)).ok()?;
    create_backoff(status.create_failures)
        .checked_sub(elapsed)
        .filter(|d| !d.is_zero())
}

/// The `VMCreated` reason for a create EC2 refused; `None` for a failure a
/// retry may fix soon (throttling, a 5xx, the transport, the credential),
/// which is not counted as a failed create.
#[must_use]
pub fn create_failure_reason(error: &AwsError) -> Option<&'static str> {
    if error.is_zone_unavailable() {
        return Some(reasons::ZONE_UNAVAILABLE);
    }
    match error {
        AwsError::QuotaExceeded(_) => Some(reasons::QUOTA_EXCEEDED),
        AwsError::Throttled(_)
        | AwsError::Server(_)
        | AwsError::Transport(_)
        | AwsError::Decode(_)
        | AwsError::Unauthenticated(_) => None,
        _ => Some(reasons::CREATE_REFUSED),
    }
}

fn cond(
    status: &mut EC2MachineStatus,
    type_: &str,
    state: &str,
    reason: &str,
    message: impl Into<String>,
    generation: i64,
) {
    set_condition(
        &mut status.conditions,
        type_,
        state,
        reason,
        message,
        generation,
    );
}

fn not_ready(
    status: &mut EC2MachineStatus,
    reason: &str,
    message: impl Into<String>,
    generation: i64,
) {
    cond(
        status,
        condition_types::READY,
        condition_status::FALSE,
        reason,
        message,
        generation,
    );
}

/// Sets `VMCreated`'s `lastTransitionTime` to `now`.
fn stamp_vm_created(status: &mut EC2MachineStatus, now: jiff::Timestamp) {
    if let Some(c) = status.conditions.iter_mut().find(|c| c.type_ == VM_CREATED) {
        c.last_transition_time = Time(now);
    }
}

/// Records a failed create on `VMCreated` and `Ready`. `lastTransitionTime`
/// becomes `now` on every failure, so the back-off counts from the latest,
/// and the controller sees each failure as new (ADR-0173 Decision 3).
/// `createFailures` counts consecutive failures of this generation; a new
/// generation starts again at one.
fn record_create_failure(
    status: &mut EC2MachineStatus,
    reason: &str,
    message: &str,
    generation: i64,
    now: jiff::Timestamp,
) {
    warn!(reason, message, "instance create failed");
    let same_generation = find_condition(&status.conditions, VM_CREATED).is_some_and(|c| {
        c.status == condition_status::FALSE && c.observed_generation == Some(generation)
    });
    status.create_failures = if same_generation {
        status.create_failures.saturating_add(1)
    } else {
        1
    };
    cond(
        status,
        VM_CREATED,
        condition_status::FALSE,
        reason,
        message,
        generation,
    );
    stamp_vm_created(status, now);
    not_ready(status, reason, message, generation);
}

/// The pass after an AWS error: throttling and transient failures wait and
/// come back. The status keeps everything known.
fn aws_error(
    mut status: EC2MachineStatus,
    error: &AwsError,
    provider_id: Option<String>,
    generation: i64,
) -> Pass {
    if error.is_throttled() {
        not_ready(
            &mut status,
            reasons::THROTTLED,
            format!(
                "EC2 throttled the call; retrying in {}s",
                THROTTLED_REQUEUE.as_secs()
            ),
            generation,
        );
        return Pass {
            status,
            provider_id,
            requeue: THROTTLED_REQUEUE,
        };
    }
    warn!(error = %error, "EC2 call failed");
    not_ready(
        &mut status,
        reasons::AWS_ERROR,
        error.to_string(),
        generation,
    );
    Pass {
        status,
        provider_id,
        requeue: AWS_ERROR_REQUEUE,
    }
}

/// The `providerID` of the stored instance in `spec.zone`, if one is stored.
fn stored_provider_id(status: &EC2MachineStatus, spec: &EC2MachineSpec) -> Option<String> {
    status
        .instance_id
        .as_deref()
        .map(|id| ec2_provider_id(&spec.zone, id))
}

/// The instance a previous pass created for `uid`, found by its ownership
/// tag: one not terminating, preferably in `zone`.
///
/// # Errors
/// The tag search's EC2 error.
async fn adoptable(
    api: &dyn Ec2Api,
    uid: &str,
    zone: &str,
) -> banlieue_aws::Result<Option<Instance>> {
    let found = api.find_instances_by_tag(TAG_OWNER_UID, uid).await?;
    let alive: Vec<&Instance> = owned_instances(&found, uid)
        .into_iter()
        .filter(|i| !i.state.is_gone())
        .collect();
    if alive.len() > 1 {
        warn!(
            count = alive.len(),
            "more than one live instance is tagged with this machine; adopting one, the finalizer terminates all"
        );
    }
    let chosen = alive
        .iter()
        .find(|i| i.zone_name == zone)
        .or_else(|| alive.first())
        .map(|i| (*i).clone());
    Ok(chosen)
}

/// Brings EC2 in line with `spec` and reports what was observed.
///
/// Never fails: an EC2 error becomes a condition and a requeue interval, so
/// the status always says why the machine is where it is.
pub async fn converge(
    api: &dyn Ec2Api,
    target: &Target,
    m: &MachineRef<'_>,
    spec: &EC2MachineSpec,
    prior: Option<&EC2MachineStatus>,
    generation: i64,
    now: jiff::Timestamp,
) -> Pass {
    let mut status = prior.cloned().unwrap_or_default();
    status.failure_domain.clone_from(&spec.failure_domain);

    if let Err(why) = spec.validate() {
        not_ready(&mut status, reasons::INVALID_SPEC, why, generation);
        status.observed_generation = Some(generation);
        return Pass {
            status,
            provider_id: None,
            requeue: STEADY_REQUEUE,
        };
    }
    // Status is user-editable: an ID that is not one is not sent to EC2.
    if let Some(bad) = status
        .instance_id
        .as_deref()
        .filter(|id| !is_instance_id(id))
    {
        warn!(
            instance_id = bad,
            "dropping a stored instance ID that is not one"
        );
        status.instance_id = None;
    }

    let instance = match status.instance_id.clone() {
        Some(id) => match api.describe_instance(&id).await {
            Ok(Some(instance)) => instance,
            Ok(None) => return unseen(status, spec, &id, generation),
            Err(e) => {
                let provider_id = stored_provider_id(&status, spec);
                return aws_error(status, &e, provider_id, generation);
            }
        },
        None => match adoptable(api, m.uid, &spec.zone).await {
            Ok(Some(instance)) => {
                info!(instance_id = %instance.instance_id, "adopting the instance tagged with this machine");
                status.instance_id = Some(instance.instance_id.clone());
                instance
            }
            Ok(None) => return create(api, target, m, spec, status, generation, now).await,
            Err(e) => return aws_error(status, &e, None, generation),
        },
    };

    // Adopt only what is tagged as ours: an ID alone is not ownership.
    if !owned_by(&instance.tags, m.uid) {
        let owner = instance.tags.get(TAG_OWNER_UID).map_or_else(
            || format!("carries no {TAG_OWNER_UID} tag"),
            |uid| format!("is tagged {TAG_OWNER_UID}={uid}"),
        );
        let message = format!(
            "instance {} {owner}; it is not this machine's and is left alone",
            instance.instance_id
        );
        warn!(instance_id = %instance.instance_id, "the stored instance is not tagged as ours");
        status.instance_id = None;
        status.instance_state = None;
        status.addresses.clear();
        record_create_failure(
            &mut status,
            reasons::FOREIGN_RESOURCE,
            &message,
            generation,
            now,
        );
        let requeue = create_backoff(status.create_failures);
        return Pass {
            status,
            provider_id: None,
            requeue,
        };
    }

    cond(
        &mut status,
        VM_CREATED,
        condition_status::TRUE,
        reasons::INSTANCE_EXISTS,
        format!("instance {} exists", instance.instance_id),
        generation,
    );
    status.create_failures = 0;
    observe(api, spec, instance, status, generation, now).await
}

/// The stored instance describes as not found: not visible yet when it was
/// never seen, gone when it was.
fn unseen(mut status: EC2MachineStatus, spec: &EC2MachineSpec, id: &str, generation: i64) -> Pass {
    let provider_id = Some(ec2_provider_id(&spec.zone, id));
    status.observed_generation = Some(generation);
    if status.instance_state.is_none() {
        not_ready(
            &mut status,
            reasons::CREATING,
            format!("instance {id} was created; EC2 does not show it yet"),
            generation,
        );
        return Pass {
            status,
            provider_id,
            requeue: FAST_POLL,
        };
    }
    instance_terminated(&mut status, id, generation);
    Pass {
        status,
        provider_id,
        requeue: STEADY_REQUEUE,
    }
}

/// Reports an instance that ended outside banlieue.
fn instance_terminated(status: &mut EC2MachineStatus, id: &str, generation: i64) {
    warn!(
        instance_id = id,
        "the instance was terminated outside banlieue"
    );
    status.instance_state = Some(InstanceState::Terminated.as_str().to_string());
    status.addresses.clear();
    status.observed_power_state = None;
    not_ready(
        status,
        reasons::INSTANCE_TERMINATED,
        format!(
            "instance {id} was terminated outside banlieue; it is not replaced, delete this machine to recover"
        ),
        generation,
    );
}

/// Launches the instance. Called only when none is stored or tagged.
async fn create(
    api: &dyn Ec2Api,
    target: &Target,
    m: &MachineRef<'_>,
    spec: &EC2MachineSpec,
    mut status: EC2MachineStatus,
    generation: i64,
    now: jiff::Timestamp,
) -> Pass {
    status.observed_generation = Some(generation);
    if let Some(wait) = create_backoff_remaining(&status, generation, now) {
        return Pass {
            status,
            provider_id: None,
            requeue: wait,
        };
    }
    let request = match run_request(target, m, spec) {
        Ok(request) => request,
        Err(e) => {
            record_create_failure(
                &mut status,
                reasons::CREATE_REFUSED,
                &e.to_string(),
                generation,
                now,
            );
            let requeue = create_backoff(status.create_failures);
            return Pass {
                status,
                provider_id: None,
                requeue,
            };
        }
    };
    info!(zone = %spec.zone, instance_type = %spec.instance_type, "launching instance");
    match api.run_instance(&request).await {
        Ok(id) => {
            info!(instance_id = %id, "RunInstances returned the instance; observing it on the next pass");
            status.instance_id = Some(id.clone());
            status.instance_state = None;
            status.create_failures = 0;
            cond(
                &mut status,
                VM_CREATED,
                condition_status::TRUE,
                reasons::INSTANCE_EXISTS,
                format!("instance {id} created"),
                generation,
            );
            // The create time: the finalizer waits this long for a fresh ID
            // to become visible before calling it gone.
            stamp_vm_created(&mut status, now);
            not_ready(
                &mut status,
                reasons::CREATING,
                format!("instance {id} created; waiting for it to run"),
                generation,
            );
            Pass {
                status,
                provider_id: Some(ec2_provider_id(&spec.zone, &id)),
                requeue: FAST_POLL,
            }
        }
        Err(e) => match create_failure_reason(&e) {
            Some(reason) => {
                record_create_failure(&mut status, reason, &e.to_string(), generation, now);
                let requeue = create_backoff(status.create_failures);
                Pass {
                    status,
                    provider_id: None,
                    requeue,
                }
            }
            None => aws_error(status, &e, None, generation),
        },
    }
}

/// The zone ID of zone `name` in this account, `None` when it cannot be
/// read this pass (retried on the next).
async fn zone_id(api: &dyn Ec2Api, name: &str) -> Option<String> {
    match api.availability_zones().await {
        Ok(zones) => zones
            .into_iter()
            .find(|z| z.name == name)
            .map(|z| z.id)
            .filter(|id| !id.is_empty()),
        Err(e) => {
            debug!(error = %e, "zone IDs not readable this pass");
            None
        }
    }
}

/// The NitroTPM's EK public keys, RSA-2048 then ECC P-384, as PEM
/// `SubjectPublicKeyInfo` (ADR-0167 Decision 1).
///
/// # Errors
/// The first read's EC2 error; not found means "not yet".
async fn endorsement_keys(api: &dyn Ec2Api, id: &str) -> banlieue_aws::Result<Vec<String>> {
    let mut keys = Vec::new();
    for key_type in [EkKeyType::Rsa2048, EkKeyType::EccSecP384] {
        let ek = api.tpm_ek_pub(id, key_type).await?;
        keys.push(to_pem_spki(&ek.key_value));
    }
    Ok(keys)
}

/// Folds an owned instance's state, address, guest and power into status,
/// changing power state when it differs from `spec`.
async fn observe(
    api: &dyn Ec2Api,
    spec: &EC2MachineSpec,
    instance: Instance,
    mut status: EC2MachineStatus,
    generation: i64,
    now: jiff::Timestamp,
) -> Pass {
    let id = instance.instance_id.clone();
    let zone = if instance.zone_name.is_empty() {
        spec.zone.clone()
    } else {
        instance.zone_name.clone()
    };
    let provider_id = Some(ec2_provider_id(&zone, &id));
    status.observed_generation = Some(generation);

    if instance.state.is_gone() {
        instance_terminated(&mut status, &id, generation);
        return Pass {
            status,
            provider_id,
            requeue: STEADY_REQUEUE,
        };
    }

    status.instance_state = Some(instance.state.as_str().to_string());
    status.addresses = addresses(&instance);
    status.observed_power_state = observed_power(&instance.state);
    if status.zone_id.is_none() {
        status.zone_id = zone_id(api, &zone).await;
    }
    // Non-negotiable 6: provisioned once EC2 shows it running; a stop later
    // does not un-provision it.
    let provisioned =
        status.initialization.provisioned == Some(true) || instance.state == InstanceState::Running;
    status.initialization.provisioned = Some(provisioned);

    let running = instance.state == InstanceState::Running;
    let mut poll_soon = !running && instance.state != InstanceState::Stopped;

    // GuestReady from the console (ADR-0175), sticky once seen.
    if running && !status.guest_installed {
        poll_soon |= read_guest(api, &id, &mut status, generation).await;
    }
    if status.guest_installed {
        cond(
            &mut status,
            condition_types::GUEST_READY,
            condition_status::TRUE,
            reasons::GUEST_REPORTED_READY,
            "the guest reported ready on the serial console",
            generation,
        );
    }

    // IMDS off once the guest is ready (ADR-0174 Decision 3), once.
    if status.guest_installed && !status.imds_disabled {
        match api.disable_instance_metadata(&id).await {
            Ok(()) => {
                info!(instance_id = %id, "turned the instance metadata endpoint off");
                status.imds_disabled = true;
            }
            Err(e) => {
                warn!(instance_id = %id, error = %e, "IMDS not turned off this pass");
                poll_soon = true;
            }
        }
    }

    // The NitroTPM's EK public keys, host-side (ADR-0167).
    if spec.tpm_enabled && running && status.tpm_endorsement_public_keys.is_empty() {
        match endorsement_keys(api, &id).await {
            Ok(keys) => {
                info!(instance_id = %id, "read the NitroTPM endorsement public keys");
                status.tpm_endorsement_public_keys = keys;
            }
            Err(e) if e.is_not_found() || e.is_throttled() => {
                debug!(instance_id = %id, error = %e, "EK public key not readable yet");
                poll_soon = true;
            }
            Err(e) => warn!(instance_id = %id, error = %e, "EK public key read failed"),
        }
    }

    // Power: stop or start, never waiting for the change.
    let mut state = instance.state.clone();
    if let Some(change) = power_change(&spec.desired_power_state, &state) {
        info!(instance_id = %id, ?change, "changing power state");
        let changed = match change {
            PowerChange::Start => api.start_instance(&id).await,
            PowerChange::Stop => api.stop_instance(&id).await,
        };
        match changed {
            Ok(after) => state = after,
            Err(e) => return aws_error(status, &e, provider_id, generation),
        }
        status.instance_state = Some(state.as_str().to_string());
        status.observed_power_state = observed_power(&state);
        poll_soon = true;
    }

    if !provisioned {
        not_ready(
            &mut status,
            reasons::INSTANCE_PENDING,
            format!("instance {id} is {}", state.as_str()),
            generation,
        );
        return Pass {
            status,
            provider_id,
            requeue: guest_poll_interval(instance.launch_time, now),
        };
    }
    if !at_desired_power(&spec.desired_power_state, &state) {
        not_ready(
            &mut status,
            reasons::AWAITING_POWER_STATE,
            format!(
                "instance {id} is {}; want {:?}",
                state.as_str(),
                spec.desired_power_state
            ),
            generation,
        );
        return Pass {
            status,
            provider_id,
            requeue: guest_poll_interval(instance.launch_time, now),
        };
    }
    cond(
        &mut status,
        condition_types::READY,
        condition_status::TRUE,
        reasons::INSTANCE_READY,
        "instance provisioned and at its desired power state",
        generation,
    );
    let requeue = if poll_soon {
        guest_poll_interval(instance.launch_time, now)
    } else {
        STEADY_REQUEUE
    };
    Pass {
        status,
        provider_id,
        requeue,
    }
}

/// Reads the console for this instance's ready line, setting
/// `guestInstalled` and `GuestReady`. Returns whether to look again soon.
async fn read_guest(
    api: &dyn Ec2Api,
    id: &str,
    status: &mut EC2MachineStatus,
    generation: i64,
) -> bool {
    let text = match api.console_output(id).await {
        Ok(text) => text,
        Err(e) => {
            debug!(instance_id = id, error = %e, "console not readable this pass");
            cond(
                status,
                condition_types::GUEST_READY,
                condition_status::FALSE,
                reasons::AWAITING_GUEST,
                "the serial console could not be read this pass",
                generation,
            );
            return true;
        }
    };
    match ready_state(&text, id) {
        ReadyState::Ready => {
            info!(instance_id = id, "the guest reported ready");
            status.guest_installed = true;
            false
        }
        ReadyState::NotYet => {
            cond(
                status,
                condition_types::GUEST_READY,
                condition_status::FALSE,
                reasons::AWAITING_GUEST,
                "no ready line on the serial console yet",
                generation,
            );
            true
        }
        ReadyState::OtherInstance(other) => {
            warn!(instance_id = id, other = %other, "a ready line names another instance; not taken");
            cond(
                status,
                condition_types::GUEST_READY,
                condition_status::FALSE,
                reasons::READY_LINE_MISMATCH,
                format!("the console's ready line names instance {other}, not {id}"),
                generation,
            );
            true
        }
    }
}

/// Where a deletion stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Finalized {
    /// Every instance this machine owned is terminated: remove the
    /// finalizer.
    Done,
    /// Waiting on EC2; apply the status and come back after the interval.
    Waiting(Box<EC2MachineStatus>, Duration),
}

/// Whether a create stamped on `VMCreated` is recent enough that an unseen
/// instance is still "not visible yet".
fn within_visibility_grace(status: &EC2MachineStatus, now: jiff::Timestamp) -> bool {
    find_condition(&status.conditions, VM_CREATED)
        .filter(|c| c.status == condition_status::TRUE)
        .and_then(|c| Duration::try_from(now.duration_since(c.last_transition_time.0)).ok())
        .is_some_and(|age| age < VISIBILITY_GRACE)
}

/// Terminates `instance` unless it already is; returns whether it is still
/// on its way out.
///
/// # Errors
/// EC2's error for the terminate.
async fn terminate(api: &dyn Ec2Api, instance: &Instance) -> banlieue_aws::Result<bool> {
    match instance.state {
        InstanceState::Terminated => return Ok(false),
        InstanceState::ShuttingDown => return Ok(true),
        _ => {}
    }
    info!(instance_id = %instance.instance_id, "terminating instance");
    let after = api.terminate_instance(&instance.instance_id).await?;
    Ok(after.is_some_and(|state| state != InstanceState::Terminated))
}

/// Terminates every instance tagged with this machine's UID (the stored one
/// and any a lost create response left), and finishes once each is
/// terminated or gone (ADR-0158 Decision 6). Already gone is success.
/// Nothing untagged is ever terminated: a stored ID whose instance is not
/// tagged as ours is left alone, with a warning.
///
/// # Errors
/// An EC2 error other than throttling (which waits); the finalizer must stay
/// until the instances are really gone.
pub async fn finalize(
    api: &dyn Ec2Api,
    m: &MachineRef<'_>,
    prior: Option<&EC2MachineStatus>,
    now: jiff::Timestamp,
) -> banlieue_aws::Result<Finalized> {
    match finalize_pass(api, m, prior, now).await {
        Err(e) if e.is_throttled() => Ok(Finalized::Waiting(
            Box::new(prior.cloned().unwrap_or_default()),
            THROTTLED_REQUEUE,
        )),
        other => other,
    }
}

async fn finalize_pass(
    api: &dyn Ec2Api,
    m: &MachineRef<'_>,
    prior: Option<&EC2MachineStatus>,
    now: jiff::Timestamp,
) -> banlieue_aws::Result<Finalized> {
    let mut status = prior.cloned().unwrap_or_default();
    let mut waiting = false;
    // The stored instance, once described (owned or not): the tag search
    // below leaves it alone then. One not visible yet is left to that search.
    let mut handled: Option<String> = None;
    let stored = status.instance_id.clone().filter(|id| is_instance_id(id));

    if let Some(id) = &stored {
        match api.describe_instance(id).await? {
            Some(instance) if owned_by(&instance.tags, m.uid) => {
                waiting |= terminate(api, &instance).await?;
                status.instance_state = Some(instance.state.as_str().to_string());
                handled = Some(id.clone());
            }
            Some(_) => {
                handled = Some(id.clone());
                warn!(
                instance_id = %id,
                    "NOT terminating the stored instance: it is not tagged as this machine's"
                );
            }
            None if status.instance_state.is_none() && within_visibility_grace(&status, now) => {
                debug!(instance_id = %id, "the created instance is not visible yet; waiting to terminate it");
                waiting = true;
            }
            None => debug!(instance_id = %id, "the stored instance is gone"),
        }
    }

    // A create whose response was lost leaves an instance only the tag
    // names (a create in flight is caught here on a later pass).
    let tagged = api.find_instances_by_tag(TAG_OWNER_UID, m.uid).await?;
    for instance in owned_instances(&tagged, m.uid)
        .into_iter()
        .filter(|i| handled.as_deref() != Some(i.instance_id.as_str()))
    {
        waiting |= terminate(api, instance).await?;
    }

    if waiting {
        return Ok(Finalized::Waiting(Box::new(status), DELETE_POLL));
    }
    Ok(Finalized::Done)
}

/// Reconcile one `EC2Machine`.
///
/// # Errors
/// [`Error`] on Kubernetes API failure, a missing or invalid `Provider`, or
/// an EC2 failure during deletion.
pub async fn reconcile(machine: Arc<EC2Machine>, ctx: Arc<Context>) -> Result<Action> {
    let namespace = machine
        .namespace()
        .ok_or(Error::Missing("EC2Machine.metadata.namespace"))?;
    let name = machine.name_any();
    let uid = machine
        .uid()
        .ok_or(Error::Missing("EC2Machine.metadata.uid"))?;
    let generation = machine.metadata.generation.unwrap_or(0);
    let deleting = machine.metadata.deletion_timestamp.is_some();
    let span = tracing::info_span!("reconcile", kind = "EC2Machine", %namespace, %name, generation);
    let _enter = span.enter();

    // A namespace the selector does not pick is not ours (ADR-0133); a
    // deleting machine is never skipped, or its instance would be stranded.
    if let Some(action) = ctx.skip_unserved(&namespace, deleting) {
        return Ok(action);
    }
    // Another Provider's machine is its own workload's (bug-178).
    if !ctx.serves_machine(&namespace, &machine.spec.provider_ref) {
        return Ok(Action::await_change());
    }

    // The Provider is read from this provider's HOME namespace, never the
    // machine's (ADR-0133 Decision 5).
    let home = ctx.home_namespace(&namespace);
    let api: Api<EC2Machine> = Api::namespaced(ctx.client.clone(), &namespace);
    let providers: Api<Provider> = Api::namespaced(ctx.client.clone(), home);
    let provider = providers.get(&machine.spec.provider_ref.name).await?;
    let target = Target::from_provider(&provider)?;
    let ec2 = ctx.ec2.build(&provider).await?;
    let m = MachineRef {
        name: &name,
        uid: &uid,
        namespace: &namespace,
    };

    if deleting {
        info!("finalizing EC2Machine");
        return match finalize(
            ec2.as_ref(),
            &m,
            machine.status.as_ref(),
            jiff::Timestamp::now(),
        )
        .await?
        {
            Finalized::Done => {
                remove_finalizer(&api, machine.as_ref(), MACHINE_FINALIZER).await?;
                Ok(Action::await_change())
            }
            Finalized::Waiting(status, after) => {
                patch_status(&api, &name, &status).await?;
                Ok(Action::requeue(after))
            }
        };
    }
    // Before any create: an instance must never exist without the finalizer
    // that terminates it.
    ensure_finalizer(&api, machine.as_ref(), MACHINE_FINALIZER).await?;

    let pass = converge(
        ec2.as_ref(),
        &target,
        &m,
        &machine.spec,
        machine.status.as_ref(),
        generation,
        jiff::Timestamp::now(),
    )
    .await;
    // The status first: it holds the instance ID, the one record of a create
    // EC2 accepted. The client token makes a lost one harmless anyway.
    patch_status(&api, &name, &pass.status).await?;
    if let Some(id) = pass.provider_id.as_deref()
        && machine.spec.provider_id.as_deref() != Some(id)
    {
        apply_provider_id(&api, &name, FIELD_MANAGER_PROVIDER_EC2, id).await?;
    }
    Ok(Action::requeue(pass.requeue))
}

async fn patch_status(api: &Api<EC2Machine>, name: &str, status: &EC2MachineStatus) -> Result<()> {
    apply_status(api, name, FIELD_MANAGER_PROVIDER_EC2, status, true).await?;
    Ok(())
}

#[cfg(test)]
#[path = "ec2machine_tests.rs"]
mod ec2machine_tests;
