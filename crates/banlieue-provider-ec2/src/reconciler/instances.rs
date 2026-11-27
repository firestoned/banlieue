// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The EC2 instances banlieue owns, and the orphan sweep (ADR-0158
//! Decisions 4 to 6).
//!
//! Ownership is the `banlieue-owner-uid` tag and nothing else: an instance
//! ID in a status, a `Name` tag or a zone is never evidence. The machine
//! finalizer and the sweep both terminate through
//! [`Ec2Api::terminate_instance`], which reads "already gone" as success.
//!
//! The sweep is a reconcile over a listed snapshot, run from the `Provider`
//! reconcile on a slow cadence ([`SWEEP_INTERVAL`]). It terminates an
//! instance only when every one of these holds:
//!
//! - it is tagged for **this** `Provider` (`banlieue-provider`);
//! - its `banlieue-owner-kind` is `EC2Machine` (images and snapshots a
//!   `VMImage` owns are another reconciler's);
//! - its `banlieue-owner-uid` names no `EC2Machine` in the listing;
//! - when the listing covers one namespace only (a home-only provider),
//!   its `banlieue-owner-namespace` is that namespace (ADR-0158 Decision 4,
//!   amended 2026-11-25);
//! - it is not already terminating or terminated;
//! - it was launched (or last started) at least [`SWEEP_GRACE`] ago, so a
//!   create in flight is never swept.
//!
//! At most [`MAX_SWEEP_DELETES_PER_PASS`] terminates per pass, and a
//! throttled pass stops. The root volume goes with its instance
//! (`DeleteOnTermination`, ADR-0174 Decision 3).

use std::collections::BTreeSet;
use std::time::Duration;

use banlieue_aws::Ec2Api;
use banlieue_aws::tags::{
    TAG_OWNER_KIND, TAG_OWNER_NAMESPACE, TAG_OWNER_UID, TAG_PROVIDER, owned_by,
};
use banlieue_aws::types::Instance;
use tracing::{info, warn};

/// The kind every instance this provider creates is tagged with.
pub const OWNER_KIND: &str = "EC2Machine";
/// How often a `Provider`'s orphan sweep runs (ADR-0158 Decision 5).
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(600);
/// How long after its launch (or last start) an orphan waits before it is
/// swept: longer than any create, so one in flight is never judged.
pub const SWEEP_GRACE: Duration = Duration::from_secs(900);
/// Terminates per sweep pass, so one pass cannot spend the account's
/// mutating-call budget the machine reconciles share.
pub const MAX_SWEEP_DELETES_PER_PASS: usize = 3;

/// The instances in `instances` tagged as owned by the object with `uid`,
/// in instance ID order.
#[must_use]
pub fn owned_instances<'a>(instances: &'a [Instance], uid: &str) -> Vec<&'a Instance> {
    let mut owned: Vec<&Instance> = instances
        .iter()
        .filter(|i| owned_by(&i.tags, uid))
        .collect();
    owned.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
    owned
}

/// Whether `instance` was launched (or last started) at least `grace` before
/// `now`. An instance with no launch time is never old enough.
fn past_grace(instance: &Instance, now: jiff::Timestamp, grace: Duration) -> bool {
    let Some(launched) = instance.launch_time else {
        return false;
    };
    Duration::try_from(now.duration_since(launched)).is_ok_and(|age| age >= grace)
}

/// What the sweep may terminate from `instances` (see the module docs), in
/// instance ID order.
///
/// `only_namespace` is `Some(home)` when `live_uids` was listed from one
/// namespace only (a home-only provider): then an instance is a candidate
/// only if its [`TAG_OWNER_NAMESPACE`] is that namespace, because a machine
/// in any other namespace is invisible to the listing and its UID would read
/// as an orphan. An instance without the tag is never a candidate then.
#[must_use]
pub fn sweep_candidates<'a>(
    instances: &'a [Instance],
    provider: &str,
    live_uids: &BTreeSet<String>,
    only_namespace: Option<&str>,
    now: jiff::Timestamp,
    grace: Duration,
) -> Vec<&'a Instance> {
    let tag = |i: &'a Instance, key: &str| i.tags.get(key).map(String::as_str);
    let mut candidates: Vec<&Instance> = instances
        .iter()
        .filter(|i| tag(i, TAG_PROVIDER) == Some(provider))
        .filter(|i| tag(i, TAG_OWNER_KIND) == Some(OWNER_KIND))
        .filter(|i| {
            tag(i, TAG_OWNER_UID).is_some_and(|uid| !uid.is_empty() && !live_uids.contains(uid))
        })
        .filter(|i| only_namespace.is_none_or(|ns| tag(i, TAG_OWNER_NAMESPACE) == Some(ns)))
        .filter(|i| !i.state.is_gone())
        .filter(|i| past_grace(i, now, grace))
        .collect();
    candidates.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
    candidates
}

/// What one sweep pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Orphans past their grace period, deferred ones included.
    pub found: usize,
    /// Instance IDs whose terminate EC2 accepted (or which were already
    /// gone).
    pub deleted: Vec<String>,
    /// Instance IDs whose terminate failed, with why; retried next pass.
    pub failed: Vec<(String, String)>,
    /// EC2 throttled the pass, which then stopped.
    pub throttled: bool,
    /// Candidates left for a later pass by the per-pass limit.
    pub deferred: usize,
}

/// One orphan sweep for `provider` (`<namespace>.<name>`).
///
/// `live_uids` must be the UID of every `EC2Machine` this provider can see,
/// deleting ones included: a UID missing from it reads as an orphan.
/// `only_namespace` limits the sweep to one namespace's instances when
/// `live_uids` came from that namespace alone (see [`sweep_candidates`]).
///
/// # Errors
/// The listing's EC2 error; nothing is terminated then.
pub async fn sweep(
    api: &dyn Ec2Api,
    provider: &str,
    live_uids: &BTreeSet<String>,
    only_namespace: Option<&str>,
    now: jiff::Timestamp,
) -> banlieue_aws::Result<SweepReport> {
    let instances = api.find_instances_by_tag(TAG_PROVIDER, provider).await?;
    let candidates = sweep_candidates(
        &instances,
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
    for instance in candidates.into_iter().take(MAX_SWEEP_DELETES_PER_PASS) {
        let id = &instance.instance_id;
        let owner = instance
            .tags
            .get(TAG_OWNER_UID)
            .cloned()
            .unwrap_or_default();
        match api.terminate_instance(id).await {
            Ok(_) => {
                info!(
                    instance_id = %id,
                    owner_uid = %owner,
                    owner_kind = OWNER_KIND,
                    provider,
                    "orphan sweep: terminated an instance whose owner is gone"
                );
                report.deleted.push(id.clone());
            }
            Err(e) if e.is_throttled() => {
                warn!("orphan sweep throttled; resuming next pass");
                report.throttled = true;
                break;
            }
            Err(e) => {
                warn!(instance_id = %id, error = %e, "orphan sweep: terminate failed");
                report.failed.push((id.clone(), e.to_string()));
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
#[path = "instances_tests.rs"]
mod instances_tests;
