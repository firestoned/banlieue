// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `AzureVirtualMachine` reconciler: a scheduled VM becomes an Azure VM
//! (ADR-0164 Decision 3).
//!
//! ```text
//! poll a stored operation  →  look the VM up  →  (adopt or create NIC, then VM)
//!   →  observe provisioning, guest, address  →  power  →  patch status
//! ```
//!
//! The decisions live in [`converge`] and [`finalize`], which take
//! `&dyn ArmApi` and no Kubernetes client, so the whole lifecycle is
//! unit-tested against `FakeArm`. [`reconcile`] only wires them to the API
//! server.
//!
//! # Never blocking on ARM
//!
//! A write ARM runs asynchronously answers with an operation handle. It is
//! stored in `status.pendingOperation` and the reconcile returns; the next
//! one polls it once (ADR-0164 Decision 2). A restarted provider resumes
//! from the stored handle instead of starting a second operation.
//!
//! # Ownership and adoption
//!
//! The VM and NIC are named from the object's UID ([`vm_name`],
//! [`nic_name`]) and tagged with it (ADR-0158 Decision 4). Before any
//! create the VM is looked up by name, and the NIC by name in a tagged
//! listing: a create whose response was lost is adopted, never repeated
//! into a second VM.
//!
//! A name is never evidence of ownership; the `banlieue-owner-uid` tag is.
//! An existing VM is adopted (observed, powered, reported) only after a
//! direct read of its tags ([`ArmApi::vm_tags`]) shows this object's UID.
//! A VM or NIC with our name but another owner's tag, or no tag at all, is
//! left alone and reported (`ForeignResource`).
//!
//! Deletion touches only resources whose `banlieue-owner-uid` tag is this
//! object's UID. A VM with our name that is not tagged as ours is never
//! deleted: the machine finalizes as if its VM were already gone, with a
//! warning, the convention the proxmox provider's `find_ours` and ADR-0101
//! (vSphere) follow, so a foreign VM neither blocks the finalizer forever
//! nor gets destroyed. A NIC of ours that such a VM holds is left with it
//! (ARM refuses to delete a NIC in use).
//!
//! # Conditions
//!
//! - `Ready`: the VM exists, provisioning succeeded and it is at its desired
//!   power state.
//! - `GuestReady`: the VM's provisioning state, which on a banlieue image is
//!   the guest's own ready report (ADR-0163 Decision 1).
//!   `status.guestInstalled` is sticky.
//! - `VMCreated`: `True` once the VM exists. `False` after a refused or
//!   failed create; its `lastTransitionTime` is the time of the latest
//!   failure and the create is not retried for [`CREATE_RETRY_BACKOFF`]
//!   unless the spec changes (ADR-0161 Decision 2). Placing the machine in
//!   another zone is the controller's decision; the provider reports
//!   `ZoneUnavailable` for it to act on.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use banlieue_api::banlieue::Provider;
use banlieue_api::common::{MachineAddress, MachineAddressType, PowerState, condition_types};
use banlieue_api::infrastructure::{
    AzureOperation, AzureOperationAction, AzureVirtualMachine, AzureVirtualMachineSpec,
    AzureVirtualMachineStatus, azure_provider_id,
};
use banlieue_azure::error::operation_failure;
use banlieue_azure::tags::TAG_OWNER_UID;
use banlieue_azure::types::{InstanceView, Power, Provisioning};
use banlieue_azure::vm::{DEFAULT_ADMIN_USERNAME, nic_name, vm_name};
use banlieue_azure::{
    ArmApi, NicRequest, OperationHandle, OperationKind, OperationStatus, OwnershipTags,
    PowerAction, VmRequest,
};
use banlieue_provider_sdk::finalizer::{ensure_finalizer, remove_finalizer};
use banlieue_provider_sdk::reconciler::REQUEUE_ON_ERROR_SECS;
use banlieue_provider_sdk::ssa::{FIELD_MANAGER_PROVIDER_AZURE, apply_provider_id, apply_status};
use banlieue_provider_sdk::status::{condition_status, find_condition, set_condition};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube::{ResourceExt, api::Api, runtime::controller::Action};
use tracing::{debug, info, warn};

use crate::client::azure_connection;
use crate::context::Context;
use crate::error::{Error, Result};
use crate::reconciler::resources::{OWNER_KIND, OwnedKind, delete_owned, owned_by};
use crate::sshkey::throwaway_public_key;

/// Finalizer holding an `AzureVirtualMachine` until its VM, NIC and any
/// tagged disk are gone.
pub const MACHINE_FINALIZER: &str = "banlieue.io/azurevirtualmachine";
/// Condition type: the VM exists (or why creating it failed).
pub const VM_CREATED: &str = "VMCreated";
/// How soon a stored operation is polled when ARM gave no hint.
pub const OPERATION_POLL: Duration = Duration::from_secs(10);
/// How soon a VM still provisioning, or changing power state, is read again.
pub const OBSERVE_POLL: Duration = Duration::from_secs(15);
/// How long a refused or failed create waits before it is tried again
/// (ADR-0161 Decision 2 starts its back-off at 5 minutes).
pub const CREATE_RETRY_BACKOFF: Duration = Duration::from_secs(300);
/// How often a settled VM is observed again.
pub const STEADY_REQUEUE: Duration = Duration::from_secs(300);
/// After an ARM error that may pass.
pub const ERROR_REQUEUE: Duration = Duration::from_secs(REQUEUE_ON_ERROR_SECS);
/// Azure's limit on a Linux computer name.
pub const MAX_COMPUTER_NAME_LEN: usize = 64;

/// ARM's error code for deleting a NIC a VM still holds.
pub const CODE_NIC_IN_USE: &str = "NicInUse";

/// Stable `reason` strings on `Ready`, `GuestReady` and `VMCreated`.
pub mod reasons {
    /// The VM exists, provisioned, at its desired power state.
    pub const VM_READY: &str = "VMReady";
    /// The VM exists.
    pub const VM_EXISTS: &str = "VMExists";
    /// `spec.validate()` refused the spec; nothing was sent to ARM.
    pub const INVALID_SPEC: &str = "InvalidSpec";
    /// A stored ARM operation has not finished.
    pub const OPERATION_IN_PROGRESS: &str = "OperationInProgress";
    /// A stored ARM operation ended `Failed` or `Canceled`.
    pub const OPERATION_FAILED: &str = "OperationFailed";
    /// The NIC or VM create was sent; the VM is not visible yet.
    pub const CREATING: &str = "Creating";
    /// The size is not available or has no capacity in this zone
    /// (ADR-0161 Decision 2): the controller may place it elsewhere.
    pub const ZONE_UNAVAILABLE: &str = "ZoneUnavailable";
    /// The subscription's vCPU quota would be exceeded.
    pub const QUOTA_EXCEEDED: &str = "QuotaExceeded";
    /// ARM refused the create for another reason (image, identity, ...).
    pub const CREATE_REFUSED: &str = "CreateRefused";
    /// A VM or NIC with this machine's name carries another owner's tag,
    /// or none.
    pub const FOREIGN_RESOURCE: &str = "ForeignResource";
    /// ARM throttled the call; retried after its hint.
    pub const THROTTLED: &str = "Throttled";
    /// ARM failed or was unreachable.
    pub const ARM_ERROR: &str = "ArmError";
    /// The VM's provisioning has not succeeded yet.
    pub const PROVISIONING: &str = "Provisioning";
    /// The VM's provisioning failed; ARM refuses every change to it.
    pub const PROVISIONING_FAILED: &str = "ProvisioningFailed";
    /// Provisioned, not yet at the desired power state.
    pub const AWAITING_POWER_STATE: &str = "AwaitingPowerState";
    /// `GuestReady=True`: provisioning succeeded.
    pub const GUEST_REPORTED_READY: &str = "GuestReportedReady";
    /// `GuestReady=False`: the guest has not reported yet.
    pub const AWAITING_GUEST: &str = "AwaitingGuest";
}

/// Reasons after which a create waits for [`CREATE_RETRY_BACKOFF`].
const BACKOFF_REASONS: [&str; 5] = [
    reasons::ZONE_UNAVAILABLE,
    reasons::QUOTA_EXCEEDED,
    reasons::CREATE_REFUSED,
    reasons::FOREIGN_RESOURCE,
    reasons::OPERATION_FAILED,
];

/// Where a `Provider` creates VMs: from its `spec.connection.azure`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Subscription ID.
    pub subscription_id: String,
    /// Resource group every resource is created in.
    pub resource_group: String,
    /// Region.
    pub location: String,
    /// The `Provider`'s namespace.
    pub provider_namespace: String,
    /// The `Provider`'s name.
    pub provider_name: String,
}

impl Target {
    /// The target `provider` declares.
    ///
    /// # Errors
    /// [`Error::Invalid`] without `spec.connection.azure`.
    pub fn from_provider(provider: &Provider) -> Result<Self> {
        let azure = azure_connection(&provider.spec.connection)?;
        Ok(Self {
            subscription_id: azure.subscription_id.clone(),
            resource_group: azure.resource_group.clone(),
            location: azure.location.clone(),
            provider_namespace: provider.namespace().unwrap_or_default(),
            provider_name: provider.name_any(),
        })
    }

    /// The resource ID of NIC `name`.
    #[must_use]
    pub fn nic_id(&self, name: &str) -> String {
        format!(
            "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Network/networkInterfaces/{name}",
            self.subscription_id, self.resource_group
        )
    }

    /// The `providerID` of VM `name`.
    #[must_use]
    pub fn provider_id(&self, name: &str) -> String {
        azure_provider_id(&self.subscription_id, &self.resource_group, name)
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
    /// `metadata.name`: the guest's computer name, for humans.
    pub name: &'a str,
    /// `metadata.uid`: names and tags every resource.
    pub uid: &'a str,
    /// `metadata.namespace`: tagged, so a home-only sweep can tell its own
    /// machines' resources from a tenant namespace's.
    pub namespace: &'a str,
}

/// What one convergence pass decided.
#[derive(Clone, Debug, PartialEq)]
pub struct Pass {
    /// The status to apply.
    pub status: AzureVirtualMachineStatus,
    /// `spec.providerID` to set, once the VM create was accepted.
    pub provider_id: Option<String>,
    /// When to reconcile again.
    pub requeue: Duration,
}

/// `handle` as stored on status.
#[must_use]
pub fn to_operation(action: AzureOperationAction, handle: &OperationHandle) -> AzureOperation {
    AzureOperation {
        action,
        url: handle.url.clone(),
        kind: handle.kind.as_str().to_string(),
    }
}

/// A stored operation as a handle to poll; `None` for a kind this provider
/// never writes (status is user-editable).
#[must_use]
pub fn to_handle(operation: &AzureOperation) -> Option<OperationHandle> {
    Some(OperationHandle {
        url: operation.url.clone(),
        kind: OperationKind::from_name(&operation.kind)?,
        retry_after: None,
    })
}

/// A Linux computer name from the object's name: letters, digits and `-`,
/// at most [`MAX_COMPUTER_NAME_LEN`], not all digits; `fallback` otherwise.
#[must_use]
pub fn computer_name(name: &str, fallback: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(MAX_COMPUTER_NAME_LEN)
        .collect();
    let trimmed = mapped.trim_matches('-');
    if trimmed.is_empty() || trimmed.chars().all(|c| c.is_ascii_digit()) {
        return fallback.chars().take(MAX_COMPUTER_NAME_LEN).collect();
    }
    trimmed.to_string()
}

/// `provisioningState` as status reports it.
#[must_use]
pub fn provisioning_label(provisioning: &Provisioning) -> String {
    match provisioning {
        Provisioning::Creating => "Creating".to_string(),
        Provisioning::Updating => "Updating".to_string(),
        Provisioning::Deleting => "Deleting".to_string(),
        Provisioning::Succeeded => "Succeeded".to_string(),
        Provisioning::Failed { .. } => "Failed".to_string(),
        Provisioning::Other(state) => state.clone(),
        Provisioning::Unknown => "Unknown".to_string(),
    }
}

/// The power state a VM reads as: deallocated and stopped are `PoweredOff`
/// (ADR-0164 Decision 3); `None` while ARM reports none.
#[must_use]
pub fn observed_power(power: Power) -> Option<PowerState> {
    match power {
        Power::Running | Power::Starting => Some(PowerState::PoweredOn),
        Power::Stopped | Power::Stopping | Power::Deallocated | Power::Deallocating => {
            Some(PowerState::PoweredOff)
        }
        Power::Unknown => None,
    }
}

/// Whether `power` is `desired`: `PoweredOff` means **deallocated**, which
/// stops compute billing; a stopped but allocated VM is not there yet.
#[must_use]
pub fn at_desired_power(desired: &PowerState, power: Power) -> bool {
    match desired {
        PowerState::PoweredOn => power == Power::Running,
        PowerState::PoweredOff => power == Power::Deallocated,
        PowerState::Suspended => false,
    }
}

/// The power change that moves `power` toward `desired`, if any. A VM in
/// transition is left to finish.
#[must_use]
pub fn power_action(
    desired: &PowerState,
    power: Power,
) -> Option<(PowerAction, AzureOperationAction)> {
    match (desired, power) {
        (PowerState::PoweredOn, Power::Deallocated | Power::Stopped) => {
            Some((PowerAction::Start, AzureOperationAction::Start))
        }
        (PowerState::PoweredOff, Power::Running | Power::Stopped) => {
            Some((PowerAction::Deallocate, AzureOperationAction::Deallocate))
        }
        _ => None,
    }
}

/// A condition reason from ARM's provisioning failure reason: CamelCase
/// letters and digits only, as Kubernetes requires.
fn condition_reason(reason: &str) -> String {
    let cleaned: String = reason.chars().filter(char::is_ascii_alphanumeric).collect();
    if cleaned.is_empty() || !cleaned.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return reasons::PROVISIONING_FAILED.to_string();
    }
    cleaned
}

fn cond(
    status: &mut AzureVirtualMachineStatus,
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
    status: &mut AzureVirtualMachineStatus,
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

/// Time left before a failed create may be tried again: `Some` only while
/// `VMCreated` is `False` for a back-off reason, for this generation, and
/// younger than [`CREATE_RETRY_BACKOFF`].
#[must_use]
pub fn create_backoff_remaining(
    status: &AzureVirtualMachineStatus,
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
    CREATE_RETRY_BACKOFF
        .checked_sub(elapsed)
        .filter(|d| !d.is_zero())
}

/// Records a failed create on `VMCreated` and `Ready`. `lastTransitionTime`
/// becomes `now` on every failure, so the back-off counts from the latest.
fn record_create_failure(
    status: &mut AzureVirtualMachineStatus,
    reason: &str,
    message: &str,
    generation: i64,
    now: jiff::Timestamp,
) {
    warn!(reason, message, "VM create failed");
    cond(
        status,
        VM_CREATED,
        condition_status::FALSE,
        reason,
        message,
        generation,
    );
    if let Some(c) = status.conditions.iter_mut().find(|c| c.type_ == VM_CREATED) {
        c.last_transition_time = Time(now);
    }
    not_ready(status, reason, message, generation);
}

/// The `VMCreated` reason for a create ARM refused or failed.
fn create_failure_reason(error: &banlieue_azure::Error) -> &'static str {
    if error.is_zone_specific() {
        return reasons::ZONE_UNAVAILABLE;
    }
    if matches!(error, banlieue_azure::Error::QuotaExceeded { .. }) {
        return reasons::QUOTA_EXCEEDED;
    }
    if matches!(error, banlieue_azure::Error::OperationFailed { .. }) {
        return reasons::OPERATION_FAILED;
    }
    reasons::CREATE_REFUSED
}

/// The pass after an ARM error: throttling waits for ARM's hint, anything
/// else for the error interval. The status keeps everything known.
fn arm_error(
    mut status: AzureVirtualMachineStatus,
    error: &banlieue_azure::Error,
    provider_id: Option<String>,
    generation: i64,
) -> Pass {
    if let banlieue_azure::Error::Throttled { retry_after } = error {
        not_ready(
            &mut status,
            reasons::THROTTLED,
            format!(
                "Azure Resource Manager throttled the call; retrying after {}s",
                retry_after.as_secs()
            ),
            generation,
        );
        return Pass {
            status,
            provider_id,
            requeue: *retry_after,
        };
    }
    warn!(error = %error, "ARM call failed");
    not_ready(
        &mut status,
        reasons::ARM_ERROR,
        error.to_string(),
        generation,
    );
    Pass {
        status,
        provider_id,
        requeue: ERROR_REQUEUE,
    }
}

/// What polling a stored operation found.
enum Polled {
    InProgress,
    Done,
    Failed { code: String, message: String },
    Unpollable(String),
    Error(banlieue_azure::Error),
}

async fn poll(api: &dyn ArmApi, operation: &AzureOperation) -> Polled {
    let Some(handle) = to_handle(operation) else {
        return Polled::Unpollable(format!("unknown operation kind {:?}", operation.kind));
    };
    match api.operation_status(&handle).await {
        Ok(OperationStatus::InProgress) => Polled::InProgress,
        Ok(OperationStatus::Succeeded) => Polled::Done,
        Ok(OperationStatus::Failed { code, message }) => Polled::Failed { code, message },
        Ok(OperationStatus::Canceled) => Polled::Failed {
            code: "Canceled".to_string(),
            message: "the operation was canceled".to_string(),
        },
        Err(banlieue_azure::Error::UntrustedUrl(url)) => {
            Polled::Unpollable(format!("{url} is not under the ARM endpoint"))
        }
        Err(e) => Polled::Error(e),
    }
}

/// What a create attempt did.
enum Created {
    /// ARM is running the write; store the handle.
    Pending(AzureOperationAction, OperationHandle),
    /// The VM create completed synchronously.
    Accepted,
    /// ARM refused for a reason a retry will not change soon.
    Refused {
        reason: &'static str,
        message: String,
    },
    /// A transient failure (throttling, 5xx, transport).
    Error(banlieue_azure::Error),
}

fn refused_or_error(error: banlieue_azure::Error) -> Created {
    if error.is_retryable() {
        return Created::Error(error);
    }
    Created::Refused {
        reason: create_failure_reason(&error),
        message: error.to_string(),
    }
}

/// Creates (or adopts) the NIC, then creates the VM. Called only when the
/// VM does not exist.
async fn create(
    api: &dyn ArmApi,
    target: &Target,
    m: &MachineRef<'_>,
    spec: &AzureVirtualMachineSpec,
) -> Created {
    let owner = target.ownership(m.uid, m.namespace);
    let nic = nic_name(m.uid);
    let vm = vm_name(m.uid);
    let resources = match api.list_resources(&target.resource_group).await {
        Ok(r) => r,
        Err(e) => return Created::Error(e),
    };
    let existing = resources
        .iter()
        .find(|r| OwnedKind::of(r) == Some(OwnedKind::Nic) && r.name.eq_ignore_ascii_case(&nic));
    match existing {
        Some(found) if !owner.owns(&found.tags) => {
            return Created::Refused {
                reason: reasons::FOREIGN_RESOURCE,
                message: format!(
                    "NIC {nic} exists but is not tagged as this machine's; it is left alone"
                ),
            };
        }
        Some(_) => debug!(%nic, "adopting the NIC a previous pass created"),
        None => {
            let request = NicRequest {
                location: target.location.clone(),
                subnet_id: spec.subnet_id.clone(),
                accelerated_networking: spec.accelerated_networking,
                tags: owner.to_map(),
            };
            info!(%nic, "creating NIC");
            match api.put_nic(&target.resource_group, &nic, &request).await {
                Ok(Some(handle)) => {
                    return Created::Pending(AzureOperationAction::CreateNic, handle);
                }
                Ok(None) => {}
                Err(e) => return refused_or_error(e),
            }
        }
    }

    // Azure requires a key; nothing keeps the private half.
    let ssh_public_key = match throwaway_public_key() {
        Ok(k) => k,
        Err(e) => {
            return Created::Refused {
                reason: reasons::CREATE_REFUSED,
                message: e.to_string(),
            };
        }
    };
    let request = VmRequest {
        location: target.location.clone(),
        zone: spec.zone.clone(),
        size: spec.size.clone(),
        image_version_id: spec.image_version_id.clone(),
        os_disk_size_gb: spec.os_disk_size_gi_b,
        os_disk_sku: spec.os_disk_sku.clone(),
        nic_id: target.nic_id(&nic),
        computer_name: computer_name(m.name, &vm),
        admin_username: DEFAULT_ADMIN_USERNAME.to_string(),
        ssh_public_key,
        custom_data: spec.user_data.clone().unwrap_or_default().into_bytes(),
        trusted_launch: spec.trusted_launch,
        tags: owner.to_map(),
    };
    info!(%vm, zone = %spec.zone, size = %spec.size, "creating VM");
    match api.put_vm(&target.resource_group, &vm, &request).await {
        Ok(Some(handle)) => Created::Pending(AzureOperationAction::CreateVm, handle),
        Ok(None) => Created::Accepted,
        Err(e) => refused_or_error(e),
    }
}

/// The instance view of `vm`, `None` when it does not exist.
async fn instance_view(
    api: &dyn ArmApi,
    resource_group: &str,
    vm: &str,
) -> banlieue_azure::Result<Option<InstanceView>> {
    match api.vm_instance_view(resource_group, vm).await {
        Ok(view) => Ok(Some(view)),
        Err(e) if e.is_not_found() => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whom an existing VM with this machine's name belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VmOwnership {
    /// Tagged with this machine's UID.
    Ours,
    /// Exists, but its `banlieue-owner-uid` tag is another UID (`Some`) or
    /// absent (`None`).
    Foreign(Option<String>),
    /// No VM by that name.
    Absent,
}

impl VmOwnership {
    /// Classifies a VM read: `tags` is `None` when there is no VM.
    #[must_use]
    pub fn of(tags: Option<&BTreeMap<String, String>>, owner: &OwnershipTags) -> Self {
        let Some(tags) = tags else {
            return Self::Absent;
        };
        if owner.owns(tags) {
            return Self::Ours;
        }
        Self::Foreign(tags.get(TAG_OWNER_UID).cloned())
    }

    /// A human description of a foreign owner, for conditions and logs.
    fn describe(owner: Option<&str>) -> String {
        owner.map_or_else(
            || format!("carries no {TAG_OWNER_UID} tag"),
            |uid| format!("is tagged {TAG_OWNER_UID}={uid}"),
        )
    }
}

/// Reads `vm`'s tags and classifies its owner against `m`.
async fn vm_ownership(
    api: &dyn ArmApi,
    target: &Target,
    m: &MachineRef<'_>,
    vm: &str,
) -> banlieue_azure::Result<VmOwnership> {
    let tags = api.vm_tags(&target.resource_group, vm).await?;
    Ok(VmOwnership::of(
        tags.as_ref(),
        &target.ownership(m.uid, m.namespace),
    ))
}

/// Brings Azure in line with `spec` and reports what was observed.
///
/// Never fails: an ARM error becomes a condition and a requeue interval, so
/// the status always says why the machine is where it is.
pub async fn converge(
    api: &dyn ArmApi,
    target: &Target,
    m: &MachineRef<'_>,
    spec: &AzureVirtualMachineSpec,
    prior: Option<&AzureVirtualMachineStatus>,
    generation: i64,
    now: jiff::Timestamp,
) -> Pass {
    let mut status = prior.cloned().unwrap_or_default();
    let vm = vm_name(m.uid);
    let rg = target.resource_group.as_str();
    status.vm_name = Some(vm.clone());
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

    let vm_requested = spec.provider_id.is_some();
    if let Some(operation) = status.pending_operation.clone() {
        let provider_id = (vm_requested || operation.action == AzureOperationAction::CreateVm)
            .then(|| target.provider_id(&vm));
        match poll(api, &operation).await {
            Polled::InProgress => {
                not_ready(
                    &mut status,
                    reasons::OPERATION_IN_PROGRESS,
                    format!("{:?} in progress", operation.action),
                    generation,
                );
                return Pass {
                    status,
                    provider_id,
                    requeue: OPERATION_POLL,
                };
            }
            Polled::Done => {
                info!(action = ?operation.action, "ARM operation succeeded");
                status.pending_operation = None;
            }
            Polled::Failed { code, message } => {
                status.pending_operation = None;
                let error = operation_failure(&code, &message);
                let detail = format!("{:?} failed: {error}", operation.action);
                if matches!(
                    operation.action,
                    AzureOperationAction::CreateNic | AzureOperationAction::CreateVm
                ) {
                    record_create_failure(
                        &mut status,
                        create_failure_reason(&error),
                        &detail,
                        generation,
                        now,
                    );
                    return Pass {
                        status,
                        provider_id,
                        requeue: CREATE_RETRY_BACKOFF,
                    };
                }
                warn!(%detail, "ARM operation failed");
                not_ready(&mut status, reasons::OPERATION_FAILED, detail, generation);
                return Pass {
                    status,
                    provider_id,
                    requeue: ERROR_REQUEUE,
                };
            }
            Polled::Unpollable(why) => {
                warn!(%why, "dropping a stored operation that cannot be polled; observing instead");
                status.pending_operation = None;
            }
            Polled::Error(e) => return arm_error(status, &e, provider_id, generation),
        }
    }

    let view = match instance_view(api, rg, &vm).await {
        Ok(view) => view,
        Err(e) => return arm_error(status, &e, None, generation),
    };
    let view = match view {
        Some(view) => view,
        None => {
            if let Some(wait) = create_backoff_remaining(&status, generation, now) {
                return Pass {
                    status,
                    provider_id: None,
                    requeue: wait,
                };
            }
            match create(api, target, m, spec).await {
                Created::Pending(action, handle) => {
                    status.pending_operation = Some(to_operation(action, &handle));
                    not_ready(
                        &mut status,
                        reasons::CREATING,
                        format!("{action:?} accepted; waiting for ARM"),
                        generation,
                    );
                    return Pass {
                        status,
                        provider_id: (action == AzureOperationAction::CreateVm)
                            .then(|| target.provider_id(&vm)),
                        requeue: handle.retry_after.unwrap_or(OPERATION_POLL),
                    };
                }
                Created::Accepted => {}
                Created::Refused { reason, message } => {
                    record_create_failure(&mut status, reason, &message, generation, now);
                    return Pass {
                        status,
                        provider_id: None,
                        requeue: CREATE_RETRY_BACKOFF,
                    };
                }
                Created::Error(e) => return arm_error(status, &e, None, generation),
            }
            let provider_id = Some(target.provider_id(&vm));
            match instance_view(api, rg, &vm).await {
                Ok(Some(view)) => view,
                Ok(None) => {
                    not_ready(
                        &mut status,
                        reasons::CREATING,
                        "the VM create was accepted; the VM is not visible yet",
                        generation,
                    );
                    return Pass {
                        status,
                        provider_id,
                        requeue: OBSERVE_POLL,
                    };
                }
                Err(e) => return arm_error(status, &e, provider_id, generation),
            }
        }
    };

    // Adopt only what is tagged as ours: the name alone is not ownership.
    match vm_ownership(api, target, m, &vm).await {
        Ok(VmOwnership::Ours) => {}
        Ok(VmOwnership::Foreign(owner)) => {
            let message = format!(
                "VM {vm} exists but {}; it is not adopted and is left alone",
                VmOwnership::describe(owner.as_deref())
            );
            warn!(%vm, owner = ?owner, "a VM with this machine's name is not tagged as ours");
            status.initialization.provisioned = Some(false);
            record_create_failure(
                &mut status,
                reasons::FOREIGN_RESOURCE,
                &message,
                generation,
                now,
            );
            return Pass {
                status,
                provider_id: None,
                requeue: CREATE_RETRY_BACKOFF,
            };
        }
        Ok(VmOwnership::Absent) => {
            not_ready(
                &mut status,
                reasons::CREATING,
                format!("VM {vm} vanished between reads; observing again"),
                generation,
            );
            return Pass {
                status,
                provider_id: None,
                requeue: OBSERVE_POLL,
            };
        }
        Err(e) => return arm_error(status, &e, None, generation),
    }

    cond(
        &mut status,
        VM_CREATED,
        condition_status::TRUE,
        reasons::VM_EXISTS,
        format!("VM {vm} exists"),
        generation,
    );
    observe(api, target, m, spec, view, status, generation).await
}

/// Folds an existing VM's instance view, address and power into status,
/// changing power state when it differs from `spec`.
async fn observe(
    api: &dyn ArmApi,
    target: &Target,
    m: &MachineRef<'_>,
    spec: &AzureVirtualMachineSpec,
    mut view: InstanceView,
    mut status: AzureVirtualMachineStatus,
    generation: i64,
) -> Pass {
    let vm = vm_name(m.uid);
    let rg = target.resource_group.as_str();
    let provider_id = Some(target.provider_id(&vm));

    // GuestReady from provisioning state (ADR-0163 Decision 1).
    match &view.provisioning {
        Provisioning::Succeeded => {
            status.guest_installed = true;
            cond(
                &mut status,
                condition_types::GUEST_READY,
                condition_status::TRUE,
                reasons::GUEST_REPORTED_READY,
                "the guest reported ready (provisioningState Succeeded)",
                generation,
            );
        }
        Provisioning::Failed { reason } => cond(
            &mut status,
            condition_types::GUEST_READY,
            condition_status::FALSE,
            &condition_reason(reason),
            view.provisioning_message
                .clone()
                .unwrap_or_else(|| format!("provisioning failed: {reason}")),
            generation,
        ),
        other if !status.guest_installed => cond(
            &mut status,
            condition_types::GUEST_READY,
            condition_status::FALSE,
            reasons::AWAITING_GUEST,
            format!("provisioningState is {}", provisioning_label(other)),
            generation,
        ),
        _ => {}
    }

    // The NIC's address; an error keeps what was known.
    match api.nic_private_ip(rg, &nic_name(m.uid)).await {
        Ok(ip) => {
            status.addresses = ip
                .into_iter()
                .map(|address| MachineAddress {
                    address_type: MachineAddressType::InternalIP,
                    address,
                })
                .collect();
        }
        Err(e) if e.is_not_found() => status.addresses.clear(),
        Err(e) => debug!(error = %e, "NIC address not readable this pass"),
    }

    // Power, only on a VM whose provisioning finished: a creating VM is
    // busy and a failed one refuses every change.
    if view.provisioning == Provisioning::Succeeded
        && let Some((action, tracked)) = power_action(&spec.desired_power_state, view.power)
    {
        info!(?action, %vm, "changing power state");
        match api.power(rg, &vm, action).await {
            Ok(Some(handle)) => {
                status.pending_operation = Some(to_operation(tracked, &handle));
                status.provisioning_state = Some(provisioning_label(&view.provisioning));
                status.observed_power_state = observed_power(view.power);
                not_ready(
                    &mut status,
                    reasons::AWAITING_POWER_STATE,
                    format!("{tracked:?} accepted; waiting for ARM"),
                    generation,
                );
                return Pass {
                    status,
                    provider_id,
                    requeue: handle.retry_after.unwrap_or(OPERATION_POLL),
                };
            }
            Ok(None) => match instance_view(api, rg, &vm).await {
                Ok(Some(after)) => view = after,
                Ok(None) => {}
                Err(e) => return arm_error(status, &e, provider_id, generation),
            },
            Err(e) => return arm_error(status, &e, provider_id, generation),
        }
    }

    status.provisioning_state = Some(provisioning_label(&view.provisioning));
    status.observed_power_state = observed_power(view.power);
    let previously = status.initialization.provisioned == Some(true);
    // Non-negotiable 6: provisioned only once ARM says Succeeded. A power
    // change reads Updating for a moment; that does not un-provision.
    let provisioned = view.provisioning == Provisioning::Succeeded
        || (previously && view.provisioning == Provisioning::Updating);
    status.initialization.provisioned = Some(provisioned);
    status.observed_generation = Some(generation);

    if let Provisioning::Failed { reason } = &view.provisioning {
        not_ready(
            &mut status,
            reasons::PROVISIONING_FAILED,
            format!(
                "provisioning failed ({reason}); ARM refuses changes to this VM, delete it to retry"
            ),
            generation,
        );
        return Pass {
            status,
            provider_id,
            requeue: STEADY_REQUEUE,
        };
    }
    if !provisioned {
        not_ready(
            &mut status,
            reasons::PROVISIONING,
            format!(
                "provisioningState is {}; waiting for the guest to report ready",
                provisioning_label(&view.provisioning)
            ),
            generation,
        );
        return Pass {
            status,
            provider_id,
            requeue: OBSERVE_POLL,
        };
    }
    if !at_desired_power(&spec.desired_power_state, view.power) {
        not_ready(
            &mut status,
            reasons::AWAITING_POWER_STATE,
            format!(
                "power is {:?}; want {:?}",
                view.power, spec.desired_power_state
            ),
            generation,
        );
        return Pass {
            status,
            provider_id,
            requeue: OBSERVE_POLL,
        };
    }
    cond(
        &mut status,
        condition_types::READY,
        condition_status::TRUE,
        reasons::VM_READY,
        "VM provisioned and at its desired power state",
        generation,
    );
    Pass {
        status,
        provider_id,
        requeue: STEADY_REQUEUE,
    }
}

/// Where a deletion stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Finalized {
    /// Everything this machine owned is gone: remove the finalizer.
    Done,
    /// Waiting on ARM; apply the status and come back after the interval.
    Waiting(Box<AzureVirtualMachineStatus>, Duration),
}

fn waiting(
    mut status: AzureVirtualMachineStatus,
    action: AzureOperationAction,
    handle: &OperationHandle,
) -> Finalized {
    status.pending_operation = Some(to_operation(action, handle));
    let after = handle.retry_after.unwrap_or(OPERATION_POLL);
    Finalized::Waiting(Box::new(status), after)
}

/// Deletes the VM (its NIC and OS disk go with it), then every remaining
/// resource tagged with this machine's UID (ADR-0158 Decision 6). Already
/// gone is success at every step. Nothing untagged is ever deleted: a VM
/// with this machine's name but not its UID tag is left alone and the
/// machine finalizes as if its VM were gone.
///
/// # Errors
/// An ARM error other than "already gone" or throttling (which waits); the
/// finalizer must stay until the resources are really gone.
pub async fn finalize(
    api: &dyn ArmApi,
    target: &Target,
    m: &MachineRef<'_>,
    prior: Option<&AzureVirtualMachineStatus>,
) -> banlieue_azure::Result<Finalized> {
    let mut status = prior.cloned().unwrap_or_default();
    let rg = target.resource_group.as_str();
    let vm = vm_name(m.uid);

    if let Some(operation) = status.pending_operation.clone() {
        match poll(api, &operation).await {
            Polled::InProgress => return Ok(Finalized::Waiting(Box::new(status), OPERATION_POLL)),
            Polled::Done | Polled::Unpollable(_) => status.pending_operation = None,
            Polled::Failed { code, message } => {
                warn!(action = ?operation.action, %code, %message, "operation failed during deletion; retrying the delete");
                status.pending_operation = None;
            }
            Polled::Error(banlieue_azure::Error::Throttled { retry_after }) => {
                return Ok(Finalized::Waiting(Box::new(status), retry_after));
            }
            Polled::Error(e) => return Err(e),
        }
    }

    // Delete the VM only if its tag says it is ours. A foreign one is left
    // alone and this machine's VM is treated as gone (module docs).
    let ownership = match vm_ownership(api, target, m, &vm).await {
        Ok(o) => o,
        Err(banlieue_azure::Error::Throttled { retry_after }) => {
            return Ok(Finalized::Waiting(Box::new(status), retry_after));
        }
        Err(e) => return Err(e),
    };
    let foreign_vm = matches!(ownership, VmOwnership::Foreign(_));
    match ownership {
        VmOwnership::Ours => {
            info!(%vm, "deleting VM");
            match delete_owned(api, rg, OwnedKind::Vm, &vm).await {
                Ok(Some(handle)) => {
                    return Ok(waiting(status, AzureOperationAction::DeleteVm, &handle));
                }
                Ok(None) => {}
                Err(banlieue_azure::Error::Throttled { retry_after }) => {
                    return Ok(Finalized::Waiting(Box::new(status), retry_after));
                }
                Err(e) => return Err(e),
            }
        }
        VmOwnership::Foreign(owner) => warn!(
            %vm,
            owner = ?owner,
            "NOT deleting a VM with this machine's name: it {}; finalizing as if this machine's VM were gone",
            VmOwnership::describe(owner.as_deref())
        ),
        VmOwnership::Absent => debug!(%vm, "no VM to delete"),
    }

    // Confirm by tag that nothing is left (a refused VM create leaves its
    // NIC behind, as the spike saw).
    let resources = api.list_resources(rg).await?;
    for (kind, resource) in owned_by(&resources, m.uid) {
        info!(resource = %resource.id, "deleting a resource tagged with this machine");
        match delete_owned(api, rg, kind, &resource.name).await {
            Ok(Some(handle)) => return Ok(waiting(status, kind.delete_action(), &handle)),
            Ok(None) => {}
            // Our NIC, held by the foreign VM left alone above: deleting it
            // is impossible without touching that VM.
            Err(e) if foreign_vm && kind == OwnedKind::Nic && e.code() == Some(CODE_NIC_IN_USE) => {
                warn!(resource = %resource.id, error = %e, "leaving this machine's NIC: a VM it does not own holds it");
            }
            Err(banlieue_azure::Error::Throttled { retry_after }) => {
                return Ok(Finalized::Waiting(Box::new(status), retry_after));
            }
            Err(e) => return Err(e),
        }
    }
    // ARM does not copy the VM's tags onto the OS disk it creates; with
    // `deleteOption: Delete` it goes with the VM. One named like ours but
    // untagged is never deleted, only reported.
    for leftover in resources.iter().filter(|r| {
        OwnedKind::of(r) == Some(OwnedKind::Disk)
            && r.name
                .to_ascii_lowercase()
                .starts_with(&vm.to_ascii_lowercase())
            && !target.ownership(m.uid, m.namespace).owns(&r.tags)
    }) {
        warn!(resource = %leftover.id, "an untagged disk named like this machine's remains; not deleting it");
    }
    Ok(Finalized::Done)
}

/// Reconcile one `AzureVirtualMachine`.
///
/// # Errors
/// [`Error`] on Kubernetes API failure, a missing `Provider` or connection,
/// or an ARM failure during deletion.
pub async fn reconcile(machine: Arc<AzureVirtualMachine>, ctx: Arc<Context>) -> Result<Action> {
    let namespace = machine
        .namespace()
        .ok_or(Error::Missing("AzureVirtualMachine.metadata.namespace"))?;
    let name = machine.name_any();
    let uid = machine
        .uid()
        .ok_or(Error::Missing("AzureVirtualMachine.metadata.uid"))?;
    let generation = machine.metadata.generation.unwrap_or(0);
    let deleting = machine.metadata.deletion_timestamp.is_some();
    let span = tracing::info_span!("reconcile", kind = "AzureVirtualMachine", %namespace, %name, generation);
    let _enter = span.enter();

    // A namespace the selector does not pick is not ours (ADR-0133); a
    // deleting machine is never skipped, or its VM would be stranded.
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
    let api: Api<AzureVirtualMachine> = Api::namespaced(ctx.client.clone(), &namespace);
    let providers: Api<Provider> = Api::namespaced(ctx.client.clone(), home);
    let provider = providers.get(&machine.spec.provider_ref.name).await?;
    let target = Target::from_provider(&provider)?;
    let arm = ctx.arm.build(&provider.spec.connection).await?;
    let m = MachineRef {
        name: &name,
        uid: &uid,
        namespace: &namespace,
    };

    if deleting {
        info!("finalizing AzureVirtualMachine");
        return match finalize(arm.as_ref(), &target, &m, machine.status.as_ref()).await? {
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
    // Before any create: a VM must never exist without the finalizer that
    // removes it.
    ensure_finalizer(&api, machine.as_ref(), MACHINE_FINALIZER).await?;

    let pass = converge(
        arm.as_ref(),
        &target,
        &m,
        &machine.spec,
        machine.status.as_ref(),
        generation,
        jiff::Timestamp::now(),
    )
    .await;
    if let Some(id) = pass.provider_id.as_deref()
        && machine.spec.provider_id.as_deref() != Some(id)
    {
        apply_provider_id(&api, &name, FIELD_MANAGER_PROVIDER_AZURE, id).await?;
    }
    patch_status(&api, &name, &pass.status).await?;
    Ok(Action::requeue(pass.requeue))
}

async fn patch_status(
    api: &Api<AzureVirtualMachine>,
    name: &str,
    status: &AzureVirtualMachineStatus,
) -> Result<()> {
    apply_status(api, name, FIELD_MANAGER_PROVIDER_AZURE, status, true).await?;
    Ok(())
}

#[cfg(test)]
#[path = "azurevirtualmachine_tests.rs"]
mod azurevirtualmachine_tests;
