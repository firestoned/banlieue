// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VirtualMachineClaim` reconciler (roadmap 17, ADR-0047).
//!
//! Thin on purpose, like the pool: gather a snapshot, call
//! [`super::claim_plan::next_step`], apply it, publish status. Every
//! decision lives in `claim_plan.rs`.
//!
//! The one rule this file exists to enforce: **a member is bound at most
//! once in its life.** Binding is a merge patch carrying the member's
//! `resourceVersion`, so when two claims race for the same member exactly
//! one patch lands and the loser picks again. Release is always deletion.
//! There is no unbind.

use std::sync::Arc;
use std::time::Duration;

use banlieue_api::banlieue::{
    ANNOTATION_SUBJECT_ID, ANNOTATION_SUBJECT_ISSUER, CLAIM_FINALIZER, ClaimPhase, LABEL_CLAIM,
    LABEL_POOL, VirtualMachine, VirtualMachineClaim, VirtualMachinePool, pool_condition_reasons,
    pool_condition_types,
};
use banlieue_provider_sdk::{
    finalizer::{ensure_finalizer, remove_finalizer},
    reconciler::{requeue_default, requeue_long, requeue_on_error},
    status::{condition_status, set_condition},
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;
use kube::{
    Resource, ResourceExt,
    api::{Api, DeleteParams, ListParams, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::claim_plan::{
    BoundMember, ClaimInputs, ClaimStep, Pick, PoolMembers, PoolWaitState, PoolWarm, expiry,
    is_expired, mirrored_member_state, next_step, pick_across_pools, pool_selection, wait_reason,
};
use super::pool::member_view;
use super::pool_plan::MemberView;
use crate::context::Context;
use crate::error::{Error, Result};

/// Seconds between retries while no member is available. Short, because a
/// consumer is blocked on this — unlike every other requeue in the
/// controller, this one is a human waiting.
const PENDING_RETRY_SECS: u64 = 3;
/// Seconds before retrying a lost bind race. Shorter still: the snapshot is
/// already known to be stale and another member is probably free.
const BIND_RACE_RETRY_SECS: u64 = 1;
/// HTTP status the API server returns when a `resourceVersion` precondition
/// fails — i.e. somebody else wrote the member since we listed it.
const HTTP_CONFLICT: u16 = 409;
/// HTTP status for an object that is already gone.
const HTTP_NOT_FOUND: u16 = 404;

/// Reconcile one `VirtualMachineClaim`.
///
/// # Arguments
/// * `claim` - the claim to reconcile.
/// * `ctx` - controller context carrying the Kubernetes client.
///
/// # Errors
/// Returns [`Error::Kube`] if the API server rejects a read or write other
/// than the expected bind conflict, or [`Error::Missing`] if the claim has
/// no namespace or UID.
pub async fn reconcile(claim: Arc<VirtualMachineClaim>, ctx: Arc<Context>) -> Result<Action> {
    let namespace = claim.namespace().ok_or(Error::Missing("namespace"))?;
    if let Some(action) = ctx.skip_unserved(&namespace, claim.metadata.deletion_timestamp.is_some())
    {
        return Ok(action);
    }
    let name = claim.name_any();
    let generation = claim.metadata.generation.unwrap_or(0);

    let claim_api: Api<VirtualMachineClaim> = Api::namespaced(ctx.client.clone(), &namespace);
    let vm_api: Api<VirtualMachine> = Api::namespaced(ctx.client.clone(), &namespace);

    let bound_name = claim
        .status
        .as_ref()
        .and_then(|s| s.virtual_machine_ref.as_ref())
        .map(|r| r.name.clone());

    // One lookup answers both "does the bound member still exist" and
    // "what are its addresses", so Hold needs no second GET.
    let bound_vm = match &bound_name {
        Some(vm) => vm_api.get_opt(vm).await?,
        None => None,
    };

    let now = Timestamp::now();
    let inputs = ClaimInputs {
        deleting: claim.metadata.deletion_timestamp.is_some(),
        expired: claim
            .status
            .as_ref()
            .and_then(|s| s.expires_at.as_ref())
            .is_some_and(|t| is_expired(t.0, now)),
        bound: bound_name.clone().map(|name| BoundMember {
            name,
            exists: bound_vm.is_some(),
        }),
    };

    // Picking is only needed on the Bind path, and it costs a pool LIST
    // plus a member LIST, so it is deferred until the step is known to need
    // it.
    let mut pools = Vec::new();
    let mut picked: Option<Candidate> = None;
    let step = match next_step(&inputs, None) {
        ClaimStep::Wait => {
            let (candidate, matched) = pick(&ctx, &namespace, &claim).await?;
            pools = matched;
            let member = candidate.as_ref().map(|c| c.pick.member.clone());
            picked = candidate;
            next_step(&inputs, member)
        }
        other => other,
    };

    match step {
        ClaimStep::Release { member, deleting } => {
            release(
                &claim_api, &vm_api, &claim, &name, member, deleting, generation,
            )
            .await
        }
        ClaimStep::Hold { member } => {
            ensure_finalizer(&claim_api, claim.as_ref(), CLAIM_FINALIZER).await?;
            let vm = bound_vm.ok_or(Error::Missing("bound member"))?;
            hold(&claim_api, &name, &member, &vm, generation).await
        }
        ClaimStep::Fail { member } => {
            ensure_finalizer(&claim_api, claim.as_ref(), CLAIM_FINALIZER).await?;
            warn!(claim = %name, vm = %member, "bound member disappeared; claim is terminal");
            patch_status(
                &claim_api,
                &name,
                ClaimPhase::Failed,
                pool_condition_reasons::MEMBER_LOST,
                format!("bound member {member} no longer exists; claims are never rebound"),
                generation,
                json!({}),
            )
            .await?;
            Ok(Action::await_change())
        }
        ClaimStep::Bind { member } => {
            ensure_finalizer(&claim_api, claim.as_ref(), CLAIM_FINALIZER).await?;
            let picked = picked.ok_or(Error::Missing("picked member"))?;
            let target = BindTarget {
                member: &member,
                pool: &picked.pick.pool,
                resource_version: picked.resource_version.as_deref(),
            };
            bind(&claim_api, &vm_api, &claim, &name, target, now, generation).await
        }
        ClaimStep::Wait => {
            ensure_finalizer(&claim_api, claim.as_ref(), CLAIM_FINALIZER).await?;
            let warms: Vec<(String, Option<(String, String)>)> = pools
                .iter()
                .map(|p| (p.name_any(), warm_condition(p)))
                .collect();
            let pool_warms: Vec<PoolWarm> = warms
                .iter()
                .map(|(pool, warm)| PoolWarm {
                    name: pool,
                    warm: warm.as_ref().map(|(r, m)| (r.as_str(), m.as_str())),
                })
                .collect();
            let (reason, message) = wait_reason(&PoolWaitState {
                selection: pool_selection(&claim.spec),
                pools: &pool_warms,
            });
            patch_status(
                &claim_api,
                &name,
                ClaimPhase::Pending,
                reason,
                message,
                generation,
                json!({}),
            )
            .await?;
            Ok(Action::requeue(Duration::from_secs(PENDING_RETRY_SECS)))
        }
    }
}

/// Requeue policy for a failed claim reconcile.
pub fn error_policy(_claim: Arc<VirtualMachineClaim>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "VirtualMachineClaim reconcile failed");
    requeue_on_error()
}

/// Choose a member from the claim's pool or pools, or `None` if none is
/// claimable. Also returns every matched pool, sorted by name, so a waiting
/// claim can report on each.
///
/// Matching no pool is not an error: a claim may legitimately be created
/// before its pool exists, and a claim whose pool was deleted should say so
/// rather than crash-looping.
///
/// Two LISTs, both in the claim's namespace: the pools, filtered by the
/// claim's [`pool_selection`], and every pool member (ADR-0132).
async fn pick(
    ctx: &Context,
    namespace: &str,
    claim: &VirtualMachineClaim,
) -> Result<(Option<Candidate>, Vec<VirtualMachinePool>)> {
    let vm_api: Api<VirtualMachine> = Api::namespaced(ctx.client.clone(), namespace);
    let members = vm_api
        .list(&ListParams::default().labels(LABEL_POOL))
        .await?
        .items;

    // Finish a bind this claim already made before picking another. Every
    // pool member is searched, not only the matched pools': a pool
    // relabelled between the bind and the claim's own status patch must not
    // make the claim take a second member (ADR-0132 Decision 5).
    let labelled = members.iter().map(|vm| {
        (
            vm.metadata.name.as_deref().unwrap_or_default(),
            vm.labels().get(LABEL_CLAIM).map(String::as_str),
        )
    });
    let pool_api: Api<VirtualMachinePool> = Api::namespaced(ctx.client.clone(), namespace);
    let selection = pool_selection(&claim.spec);
    let mut pools: Vec<VirtualMachinePool> = pool_api
        .list(&ListParams::default())
        .await?
        .items
        .into_iter()
        .filter(|p| selection.matches(&p.name_any(), p.labels()))
        .collect();
    pools.sort_by_key(ResourceExt::name_any);

    if let Some(ours) = super::claim_plan::member_bound_to(labelled, &claim.name_any()) {
        let pool = members
            .iter()
            .find(|vm| vm.name_any() == ours)
            .and_then(|vm| vm.labels().get(LABEL_POOL).cloned())
            .unwrap_or_default();
        let pick = Pick { pool, member: ours };
        return Ok((Some(candidate(pick, &members)), pools));
    }

    let now = Timestamp::now();
    let per_pool: Vec<(String, String, Vec<MemberView>)> = pools
        .iter()
        .map(|pool| {
            let pool_name = pool.name_any();
            let views = members
                .iter()
                .filter(|vm| vm.labels().get(LABEL_POOL) == Some(&pool_name))
                .map(|vm| member_view(vm, pool.spec.readiness, now))
                .collect();
            let revision = pool
                .status
                .as_ref()
                .and_then(|s| s.image_revision.clone())
                .unwrap_or_default();
            (pool_name, revision, views)
        })
        .collect();
    let candidates: Vec<PoolMembers> = per_pool
        .iter()
        .map(|(pool, current_revision, views)| PoolMembers {
            pool,
            current_revision,
            views,
        })
        .collect();

    let chosen = pick_across_pools(&candidates).map(|pick| candidate(pick, &members));
    Ok((chosen, pools))
}

/// A picked member, with the `resourceVersion` it had in the list it was
/// picked from.
#[derive(Clone, Debug)]
struct Candidate {
    pick: Pick,
    resource_version: Option<String>,
}

/// Attach the listed `resourceVersion` to `pick`.
fn candidate(pick: Pick, members: &[VirtualMachine]) -> Candidate {
    let resource_version = members
        .iter()
        .find(|vm| vm.name_any() == pick.member)
        .and_then(ResourceExt::resource_version);
    Candidate {
        pick,
        resource_version,
    }
}

/// The pool's own `Warm` condition as `(reason, message)`, so a waiting
/// claim can repeat its diagnosis instead of only saying "waiting".
fn warm_condition(pool: &VirtualMachinePool) -> Option<(String, String)> {
    pool.status
        .as_ref()?
        .conditions
        .iter()
        .find(|c| c.type_ == pool_condition_types::WARM)
        .map(|c| (c.reason.clone(), c.message.clone()))
}

/// The member a bind takes, the pool it came from, and the
/// `resourceVersion` it had when it was picked.
#[derive(Clone, Copy, Debug)]
struct BindTarget<'a> {
    member: &'a str,
    pool: &'a str,
    resource_version: Option<&'a str>,
}

/// Take a member: label it, stamp the subject on it, and re-parent it from
/// the pool to this claim.
///
/// The `resourceVersion` precondition is the entire concurrency story. A
/// merge patch carrying it is rejected with 409 if *anyone* — including
/// another claim binding the same member — wrote that object since we
/// listed it. No lock, no lease, and the API server is already the
/// serialisation point.
///
/// The precondition is the version from the LIST the member was picked
/// from, never from a fresh GET here: a GET after a competing bind returns
/// that bind's version, the precondition then matches, and the second claim
/// overwrites the first one's label. Both claims went `Bound` on one member
/// in `live_claim.rs` `two_claims_cannot_bind_the_same_member` (ADR-0132).
async fn bind(
    claim_api: &Api<VirtualMachineClaim>,
    vm_api: &Api<VirtualMachine>,
    claim: &VirtualMachineClaim,
    name: &str,
    target: BindTarget<'_>,
    now: Timestamp,
    generation: i64,
) -> Result<Action> {
    let member = target.member;
    let resource_version = target
        .resource_version
        .ok_or(Error::Missing("member resourceVersion"))?;
    let owner = claim
        .controller_owner_ref(&())
        .ok_or(Error::Missing("metadata.uid"))?;

    let patch = json!({
        "metadata": {
            "resourceVersion": resource_version,
            "labels": { LABEL_CLAIM: name },
            "annotations": {
                ANNOTATION_SUBJECT_ISSUER: claim.spec.subject.issuer,
                ANNOTATION_SUBJECT_ID: claim.spec.subject.id,
            },
            // Re-parent: the member now lives and dies with the claim, not
            // with the pool. Deleting the pool must not destroy a sandbox
            // somebody is using (ADR-0047 Decision 3).
            "ownerReferences": [owner],
        }
    });

    match vm_api
        .patch(member, &PatchParams::default(), &Patch::Merge(&patch))
        .await
    {
        Ok(_) => {}
        // 409: somebody wrote the member since we listed it. 404: it was
        // deleted since. Either way the snapshot is stale; pick again.
        Err(kube::Error::Api(e)) if e.code == HTTP_CONFLICT || e.code == HTTP_NOT_FOUND => {
            info!(claim = %name, member = %member, "lost bind race; picking again");
            return Ok(Action::requeue(Duration::from_secs(BIND_RACE_RETRY_SECS)));
        }
        Err(e) => return Err(Error::Kube(e)),
    }

    let (bound_at, expires_at) = expiry(now, claim.spec.ttl_seconds);
    patch_status(
        claim_api,
        name,
        ClaimPhase::Bound,
        pool_condition_types::BOUND,
        format!("bound to {member}"),
        generation,
        bound_status(member, target.pool, bound_at, expires_at),
    )
    .await?;
    info!(claim = %name, member = %member, pool = %target.pool, ttl = claim.spec.ttl_seconds, "claim bound");
    Ok(requeue_default())
}

/// Bound and healthy: mirror the member's observable state onto the claim
/// so a consumer needs one GET rather than two.
async fn hold(
    claim_api: &Api<VirtualMachineClaim>,
    name: &str,
    member: &str,
    vm: &VirtualMachine,
    generation: i64,
) -> Result<Action> {
    // ADR-0045: the certificate travels with the addresses, so one GET of a
    // bound claim yields both the nonce and the anchor a quote must chain to
    // (ADR-0049). Empty for a member with no vTPM.
    let mirrored = mirrored_member_state(vm);
    let mut extra = json!({
        "addresses": mirrored.addresses,
        "tpmEndorsementCertificates": mirrored.tpm_endorsement_certificates,
    });
    // ADR-0132 Decision 6: backfills a claim bound before status.poolRef
    // existed; for any other claim it rewrites the value the bind wrote.
    if let Some(pool) = mirrored.pool {
        extra["poolRef"] = json!({ "name": pool });
    }
    patch_status(
        claim_api,
        name,
        ClaimPhase::Bound,
        pool_condition_types::BOUND,
        format!("bound to {member}"),
        generation,
        extra,
    )
    .await?;
    Ok(requeue_default())
}

/// Destroy the member, then let the claim go.
///
/// The order is the guarantee: the claim keeps its finalizer until the
/// member is gone from the API server, and the member's own finalizer keeps
/// *it* until the backend VM is destroyed (ADR-0026, ADR-0050). So "claim
/// gone" means "sandbox gone", transitively.
#[allow(clippy::too_many_arguments)]
async fn release(
    claim_api: &Api<VirtualMachineClaim>,
    vm_api: &Api<VirtualMachine>,
    claim: &VirtualMachineClaim,
    name: &str,
    member: Option<String>,
    deleting: bool,
    generation: i64,
) -> Result<Action> {
    if let Some(vm) = &member {
        match vm_api.delete(vm, &DeleteParams::background()).await {
            // Still there: the delete is issued (or already was) and the
            // member's finalizer is working. Come back rather than
            // releasing the claim while its VM still exists.
            Ok(_) => {
                patch_status(
                    claim_api,
                    name,
                    ClaimPhase::Releasing,
                    if deleting {
                        pool_condition_reasons::RELEASED
                    } else {
                        pool_condition_reasons::EXPIRED
                    },
                    format!("destroying member {vm}"),
                    generation,
                    json!({}),
                )
                .await?;
                return Ok(requeue_default());
            }
            Err(kube::Error::Api(e)) if e.code == HTTP_NOT_FOUND => {}
            Err(e) => return Err(Error::Kube(e)),
        }
    }

    if deleting {
        remove_finalizer(claim_api, claim, CLAIM_FINALIZER).await?;
        info!(claim = %name, "claim released; member destroyed");
        return Ok(Action::await_change());
    }

    // Expired but not deleted by anyone: remove the claim ourselves, so it
    // cannot be mistaken for a live hold on a VM that no longer exists.
    info!(claim = %name, "claim expired; member destroyed");
    match claim_api.delete(name, &DeleteParams::default()).await {
        Ok(_) => {}
        Err(kube::Error::Api(e)) if e.code == HTTP_NOT_FOUND => {}
        Err(e) => return Err(Error::Kube(e)),
    }
    Ok(requeue_long())
}

/// Patch `status`, merging `extra` over the phase and `Bound` condition.
async fn patch_status(
    api: &Api<VirtualMachineClaim>,
    name: &str,
    phase: ClaimPhase,
    reason: &str,
    message: String,
    generation: i64,
    extra: serde_json::Value,
) -> Result<()> {
    let status = if phase == ClaimPhase::Bound {
        condition_status::TRUE
    } else {
        condition_status::FALSE
    };
    let mut conditions: Vec<Condition> = Vec::new();
    set_condition(
        &mut conditions,
        pool_condition_types::BOUND,
        status,
        reason,
        message,
        generation,
    );

    let mut body = json!({
        "phase": phase,
        "conditions": conditions,
        "observedGeneration": generation,
    });
    if let (Some(base), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in extra {
            base.insert(k.clone(), v.clone());
        }
    }

    api.patch_status(
        name,
        &PatchParams::default(),
        &Patch::Merge(json!({ "status": body })),
    )
    .await?;
    Ok(())
}

/// The status fields a bind publishes: the member, the pool it came from
/// (ADR-0132 Decision 6), and when it was bound and expires.
///
/// No `nonce`: ADR-0049's quote-over-nonce handshake was superseded by SPIRE's
/// TPM node attestation, whose challenge is its own (ADR-0049, amended
/// 2026-11-05), so the field is deprecated and never set.
#[must_use]
pub fn bound_status(member: &str, pool: &str, bound_at: Timestamp, expires_at: Timestamp) -> Value {
    json!({
        "virtualMachineRef": { "name": member },
        "poolRef": { "name": pool },
        "boundAt": Time(bound_at),
        "expiresAt": Time(expires_at),
    })
}

#[cfg(test)]
#[path = "claim_tests.rs"]
mod claim_tests;
