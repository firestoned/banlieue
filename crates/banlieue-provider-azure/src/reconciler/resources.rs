// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The Azure resources banlieue owns, and the orphan sweep (ADR-0158
//! Decisions 4 to 6).
//!
//! Ownership is the `banlieue-owner-uid` tag and nothing else: a name, a
//! location or another tag is never evidence. Both the machine finalizer and
//! the sweep delete through [`delete_owned`], so "already gone" is success in
//! one place.
//!
//! The sweep is a reconcile over a listed snapshot, run from the `Provider`
//! reconcile on a slow cadence ([`SWEEP_INTERVAL`]). It deletes a resource
//! only when every one of these holds:
//!
//! - it is tagged for **this** `Provider` (`banlieue-provider`);
//! - its `banlieue-owner-kind` is `AzureVirtualMachine` (gallery resources
//!   a `VMImage` owns are another reconciler's);
//! - its `banlieue-owner-uid` names no `AzureVirtualMachine` in the listing;
//! - it was created at least [`SWEEP_GRACE`] ago, so a create in flight is
//!   never swept;
//! - it is a VM, NIC or managed disk.
//!
//! At most [`MAX_SWEEP_DELETES_PER_PASS`] deletes per pass, and a throttled
//! pass stops: ARM throttled the fourth delete of a burst in the spike.

use std::collections::BTreeSet;
use std::time::Duration;

use banlieue_api::infrastructure::AzureOperationAction;
use banlieue_azure::tags::{TAG_OWNER_KIND, TAG_OWNER_NAMESPACE, TAG_OWNER_UID, TAG_PROVIDER};
use banlieue_azure::types::{TaggedResource, orphans};
use banlieue_azure::{ArmApi, OperationHandle};
use tracing::{info, warn};

/// The kind every VM, NIC and disk this provider creates is tagged with.
pub const OWNER_KIND: &str = "AzureVirtualMachine";
/// ARM type of a VM.
pub const RESOURCE_TYPE_VM: &str = "Microsoft.Compute/virtualMachines";
/// ARM type of a NIC.
pub const RESOURCE_TYPE_NIC: &str = "Microsoft.Network/networkInterfaces";
/// ARM type of a managed disk.
pub const RESOURCE_TYPE_DISK: &str = "Microsoft.Compute/disks";
/// How often a `Provider`'s orphan sweep runs (ADR-0158 Decision 5).
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(600);
/// How old an orphan must be before it is swept: longer than any create.
pub const SWEEP_GRACE: Duration = Duration::from_secs(900);
/// Deletes per sweep pass; ARM throttled the fourth of a burst in the spike.
pub const MAX_SWEEP_DELETES_PER_PASS: usize = 3;

/// The resource types banlieue creates for a machine, in delete order: the
/// VM first, since deleting it takes its NIC and OS disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OwnedKind {
    /// A virtual machine.
    Vm,
    /// A network interface.
    Nic,
    /// A managed disk.
    Disk,
}

impl OwnedKind {
    /// The kind of `resource`, by its ARM type (any case); `None` for a type
    /// banlieue never creates for a machine.
    #[must_use]
    pub fn of(resource: &TaggedResource) -> Option<Self> {
        let t = resource.resource_type.as_str();
        if t.eq_ignore_ascii_case(RESOURCE_TYPE_VM) {
            return Some(Self::Vm);
        }
        if t.eq_ignore_ascii_case(RESOURCE_TYPE_NIC) {
            return Some(Self::Nic);
        }
        if t.eq_ignore_ascii_case(RESOURCE_TYPE_DISK) {
            return Some(Self::Disk);
        }
        None
    }

    /// The operation a delete of this kind is tracked as.
    #[must_use]
    pub fn delete_action(self) -> AzureOperationAction {
        match self {
            Self::Vm => AzureOperationAction::DeleteVm,
            Self::Nic => AzureOperationAction::DeleteNic,
            Self::Disk => AzureOperationAction::DeleteDisk,
        }
    }
}

/// Deletes `name` of `kind` in `resource_group`. A resource that is already
/// gone is success (`Ok(None)`), as it is in ARM.
///
/// # Errors
/// Any other ARM error, including [`banlieue_azure::Error::Throttled`].
pub async fn delete_owned(
    api: &dyn ArmApi,
    resource_group: &str,
    kind: OwnedKind,
    name: &str,
) -> banlieue_azure::Result<Option<OperationHandle>> {
    let result = match kind {
        OwnedKind::Vm => api.delete_vm(resource_group, name).await,
        OwnedKind::Nic => api.delete_nic(resource_group, name).await,
        OwnedKind::Disk => api.delete_disk(resource_group, name).await,
    };
    match result {
        Err(e) if e.is_not_found() => Ok(None),
        other => other,
    }
}

/// The resources a machine with `owner_uid` owns, by tag, in delete order.
#[must_use]
pub fn owned_by<'a>(
    resources: &'a [TaggedResource],
    owner_uid: &str,
) -> Vec<(OwnedKind, &'a TaggedResource)> {
    let mut owned: Vec<(OwnedKind, &TaggedResource)> = resources
        .iter()
        .filter(|r| r.tags.get(TAG_OWNER_UID).map(String::as_str) == Some(owner_uid))
        .filter_map(|r| OwnedKind::of(r).map(|k| (k, r)))
        .collect();
    owned.sort_by_key(|(k, r)| (*k, r.name.clone()));
    owned
}

/// What the sweep may delete from `resources`: [`orphans`] for `provider`
/// (`<namespace>.<name>`), narrowed to `AzureVirtualMachine`-owned VMs, NICs
/// and disks, in delete order.
///
/// `only_namespace` is `Some(home)` when `live_uids` was listed from one
/// namespace only (a home-only provider): then a resource is a candidate
/// only if its [`TAG_OWNER_NAMESPACE`] is that namespace, because a machine
/// in any other namespace is invisible to the listing and its UID would
/// read as an orphan. A resource without the tag is never a candidate then.
#[must_use]
pub fn sweep_candidates<'a>(
    resources: &'a [TaggedResource],
    provider: &str,
    live_uids: &BTreeSet<String>,
    only_namespace: Option<&str>,
    now: jiff::Timestamp,
    grace: Duration,
) -> Vec<(OwnedKind, &'a TaggedResource)> {
    let mut candidates: Vec<(OwnedKind, &TaggedResource)> =
        orphans(resources, provider, live_uids, now, grace)
            .into_iter()
            .filter(|r| r.tags.get(TAG_OWNER_KIND).map(String::as_str) == Some(OWNER_KIND))
            .filter(|r| {
                only_namespace.is_none_or(|ns| {
                    r.tags.get(TAG_OWNER_NAMESPACE).map(String::as_str) == Some(ns)
                })
            })
            .filter_map(|r| OwnedKind::of(r).map(|k| (k, r)))
            .collect();
    candidates.sort_by_key(|(k, r)| (*k, r.name.clone()));
    candidates
}

/// What one sweep pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Orphans past their grace period, deferred ones included.
    pub found: usize,
    /// Names whose delete ARM accepted.
    pub deleted: Vec<String>,
    /// Names whose delete failed, with why; retried next pass.
    pub failed: Vec<(String, String)>,
    /// Set when ARM throttled the pass, which then stopped.
    pub throttled: Option<Duration>,
    /// Candidates left for a later pass by the per-pass limit.
    pub deferred: usize,
}

/// One orphan sweep over `resource_group` for `provider`.
///
/// `live_uids` must be the UID of every `AzureVirtualMachine` this provider
/// can see, deleting ones included: a UID missing from it reads as an orphan.
/// `only_namespace` limits the sweep to one namespace's resources when
/// `live_uids` came from that namespace alone (see [`sweep_candidates`]).
///
/// # Errors
/// The listing's ARM error; nothing is deleted then.
pub async fn sweep(
    api: &dyn ArmApi,
    resource_group: &str,
    provider: &str,
    live_uids: &BTreeSet<String>,
    only_namespace: Option<&str>,
    now: jiff::Timestamp,
) -> banlieue_azure::Result<SweepReport> {
    let resources = api.list_resources(resource_group).await?;
    let candidates = sweep_candidates(
        &resources,
        provider,
        live_uids,
        only_namespace,
        now,
        SWEEP_GRACE,
    );
    let mut report = SweepReport {
        found: candidates.len(),
        deferred: candidates.len().saturating_sub(MAX_SWEEP_DELETES_PER_PASS),
        ..SweepReport::default()
    };
    for (kind, resource) in candidates.into_iter().take(MAX_SWEEP_DELETES_PER_PASS) {
        let owner = resource
            .tags
            .get(TAG_OWNER_UID)
            .cloned()
            .unwrap_or_default();
        let tagged_provider = resource.tags.get(TAG_PROVIDER).cloned().unwrap_or_default();
        match delete_owned(api, resource_group, kind, &resource.name).await {
            Ok(_) => {
                info!(
                    resource = %resource.id,
                    owner_uid = %owner,
                    owner_kind = OWNER_KIND,
                    provider = %tagged_provider,
                    "orphan sweep: deleted a resource whose owner is gone"
                );
                report.deleted.push(resource.name.clone());
            }
            Err(banlieue_azure::Error::Throttled { retry_after }) => {
                warn!(?retry_after, "orphan sweep throttled; resuming next pass");
                report.throttled = Some(retry_after);
                break;
            }
            Err(e) => {
                warn!(resource = %resource.id, error = %e, "orphan sweep: delete failed");
                report.failed.push((resource.name.clone(), e.to_string()));
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
#[path = "resources_tests.rs"]
mod resources_tests;
