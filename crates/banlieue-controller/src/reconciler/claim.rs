// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VirtualMachineClaim` reconciler (roadmap 70, ADR-0047).
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
    ANNOTATION_SUBJECT_ID, ANNOTATION_SUBJECT_ISSUER, CLAIM_FINALIZER, CLAIM_NONCE_BYTES,
    ClaimPhase, LABEL_CLAIM, LABEL_POOL, VirtualMachine, VirtualMachineClaim, VirtualMachinePool,
    pool_condition_reasons, pool_condition_types,
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
use serde_json::json;
use tracing::{info, warn};

use super::claim_plan::{
    BoundMember, ClaimInputs, ClaimStep, PoolWaitState, expiry, is_expired, next_step, wait_reason,
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

    // Picking is only needed on the Bind path, and it costs a pool GET plus
    // a member LIST — so it is deferred until the step is known to need it.
    let mut pool = None;
    let step = match next_step(&inputs, None) {
        ClaimStep::Wait => {
            let (candidate, found) = pick(&ctx, &namespace, &claim).await?;
            pool = found;
            next_step(&inputs, candidate)
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
            bind(&claim_api, &vm_api, &claim, &name, &member, now, generation).await
        }
        ClaimStep::Wait => {
            ensure_finalizer(&claim_api, claim.as_ref(), CLAIM_FINALIZER).await?;
            let warm = pool.as_ref().and_then(warm_condition);
            let (reason, message) = wait_reason(&PoolWaitState {
                pool_name: &claim.spec.pool_ref.name,
                pool_exists: pool.is_some(),
                warm: warm.as_ref().map(|(r, m)| (r.as_str(), m.as_str())),
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

/// Choose a member of the claim's pool, or `None` if none is claimable.
///
/// A missing pool is not an error: a claim may legitimately be created
/// before its pool exists, and a claim whose pool was deleted should say so
/// rather than crash-looping.
async fn pick(
    ctx: &Context,
    namespace: &str,
    claim: &VirtualMachineClaim,
) -> Result<(Option<String>, Option<VirtualMachinePool>)> {
    let pool_api: Api<VirtualMachinePool> = Api::namespaced(ctx.client.clone(), namespace);
    let Some(pool) = pool_api.get_opt(&claim.spec.pool_ref.name).await? else {
        return Ok((None, None));
    };
    let vm_api: Api<VirtualMachine> = Api::namespaced(ctx.client.clone(), namespace);
    let members = vm_api
        .list(&ListParams::default().labels(&format!("{LABEL_POOL}={}", pool.name_any())))
        .await?
        .items;

    let now = Timestamp::now();
    let views: Vec<MemberView> = members
        .iter()
        .map(|vm| member_view(vm, pool.spec.readiness, now))
        .collect();
    let current_revision = pool
        .status
        .as_ref()
        .and_then(|s| s.image_revision.clone())
        .unwrap_or_default();

    let choice = super::claim_plan::pick_member(&views, &current_revision);
    Ok((choice, Some(pool)))
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

/// Take a member: label it, stamp the subject on it, and re-parent it from
/// the pool to this claim.
///
/// The `resourceVersion` precondition is the entire concurrency story. A
/// merge patch carrying it is rejected with 409 if *anyone* — including
/// another claim binding the same member — wrote that object since we
/// listed it. No lock, no lease, and the API server is already the
/// serialisation point.
async fn bind(
    claim_api: &Api<VirtualMachineClaim>,
    vm_api: &Api<VirtualMachine>,
    claim: &VirtualMachineClaim,
    name: &str,
    member: &str,
    now: Timestamp,
    generation: i64,
) -> Result<Action> {
    let vm = vm_api
        .get_opt(member)
        .await?
        .ok_or(Error::Missing("member"))?;
    let owner = claim
        .controller_owner_ref(&())
        .ok_or(Error::Missing("metadata.uid"))?;

    let patch = json!({
        "metadata": {
            "resourceVersion": vm.resource_version(),
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
        Err(kube::Error::Api(e)) if e.code == HTTP_CONFLICT => {
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
        json!({
            "virtualMachineRef": { "name": member },
            "nonce": new_nonce(),
            "boundAt": Time(bound_at),
            "expiresAt": Time(expires_at),
        }),
    )
    .await?;
    info!(claim = %name, member = %member, ttl = claim.spec.ttl_seconds, "claim bound");
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
    let addresses = vm
        .status
        .as_ref()
        .map(|s| s.addresses.clone())
        .unwrap_or_default();
    // `tpmEndorsementCertificates` will be mirrored here from the member's
    // infra CR once ADR-0045 publishes them; the field exists so that
    // landing it needs no second CRD change.
    patch_status(
        claim_api,
        name,
        ClaimPhase::Bound,
        pool_condition_types::BOUND,
        format!("bound to {member}"),
        generation,
        json!({ "addresses": addresses }),
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

/// 128 random bits, hex-encoded.
///
/// Deliberately *not* derived from the claim UID, its name or the clock:
/// the whole point is that a guest which has not been told the nonce cannot
/// predict it, so an attestation quote cannot be replayed across claims.
fn new_nonce() -> String {
    let mut bytes = [0u8; CLAIM_NONCE_BYTES];
    getrandom::fill(&mut bytes).expect("OS RNG unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
#[path = "claim_tests.rs"]
mod claim_tests;
