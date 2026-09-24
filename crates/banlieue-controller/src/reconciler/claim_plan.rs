// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Pure decisions behind the `VirtualMachineClaim` reconciler (ADR-0047).
//!
//! Everything a claim decides is a function of a snapshot: which member to
//! bind, when the hold expires, and which step the state machine takes
//! next. The reconciler in [`super::claim`] only gathers the snapshot,
//! applies the step and reports — the same split the pool uses
//! ([`super::pool_plan`]), and for the same reason: a state machine that
//! needs a cluster to exercise is a state machine nobody exercises.

use banlieue_api::banlieue::{VirtualMachine, pool_condition_reasons};
use banlieue_api::common::MachineAddress;
use k8s_openapi::jiff::Timestamp;

use super::pool_plan::{MemberPhase, MemberView};

/// The bound member as the planner needs to see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundMember {
    pub name: String,
    /// Whether the member still exists in the API server.
    pub exists: bool,
}

/// A claim's situation, gathered from its own status and one member lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimInputs {
    /// `metadata.deletionTimestamp` is set.
    pub deleting: bool,
    /// `status.expiresAt` has passed.
    pub expired: bool,
    /// `status.virtualMachineRef`, resolved.
    pub bound: Option<BoundMember>,
}

/// What the reconciler should do this pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimStep {
    /// Destroy the member (if there is one), then let the claim go.
    /// `deleting` distinguishes "the user deleted this" from "the TTL ran
    /// out", which differ only in who removes the claim object at the end.
    Release {
        member: Option<String>,
        deleting: bool,
    },
    /// Bound and healthy: mirror the member's status and sleep.
    Hold { member: String },
    /// The bound member is gone. Terminal — never a rebind.
    Fail { member: String },
    /// Not bound yet; take this member.
    Bind { member: String },
    /// Not bound and nothing available. Wait, unboundedly.
    Wait,
}

/// Decide this pass's step.
///
/// Precedence is the interesting part. Release outranks everything,
/// including a healthy binding: once a claim is going away, or its deadline
/// has passed, the only remaining job is to destroy the member. Below that,
/// a vanished member fails the claim even when a replacement is available,
/// because rebinding would hand the consumer a different VM than the one it
/// believes it holds (ADR-0047 Decision 7).
///
/// # Arguments
/// * `inputs` - the claim's own situation.
/// * `candidate` - the member [`pick_member`] chose, if any.
#[must_use]
pub fn next_step(inputs: &ClaimInputs, candidate: Option<String>) -> ClaimStep {
    if inputs.deleting || inputs.expired {
        return ClaimStep::Release {
            member: inputs.bound.as_ref().map(|b| b.name.clone()),
            deleting: inputs.deleting,
        };
    }
    if let Some(bound) = &inputs.bound {
        if !bound.exists {
            return ClaimStep::Fail {
                member: bound.name.clone(),
            };
        }
        return ClaimStep::Hold {
            member: bound.name.clone(),
        };
    }
    match candidate {
        Some(member) => ClaimStep::Bind { member },
        None => ClaimStep::Wait,
    }
}

/// Choose the member to bind.
///
/// Ready only — a `Claimed` member is never offered again, which is the
/// invariant the whole design rests on. Then current image revision first,
/// so a rebuild that was meant to retire an unpatched build actually does;
/// then the member that has been Ready longest, so idle expiry rarely has
/// to fire; then name, purely so two controllers reading one snapshot make
/// the same choice.
///
/// A stale-revision member is still handed out when nothing fresher is
/// Ready: an available sandbox on yesterday's image beats no sandbox, and
/// its life is bounded by the claim's TTL anyway.
///
/// # Arguments
/// * `views` - every member of the pool, as the pool planner sees them.
/// * `current_revision` - the pool's `status.imageRevision`.
#[must_use]
pub fn pick_member(views: &[MemberView], current_revision: &str) -> Option<String> {
    views
        .iter()
        .filter(|m| m.phase == MemberPhase::Ready)
        .min_by(|a, b| {
            let a_stale = a.image_revision != current_revision;
            let b_stale = b.image_revision != current_revision;
            a_stale
                .cmp(&b_stale)
                .then_with(|| b.ready_for_secs.cmp(&a.ready_for_secs))
                .then_with(|| a.name.cmp(&b.name))
        })
        .map(|m| m.name.clone())
}

/// `(bound_at, expires_at)` for a claim binding at `now`.
///
/// Counted from the bind instant rather than from creation: a claim that
/// waited ten minutes for capacity still gets its full TTL. Saturates
/// rather than wrapping — a wrapped deadline lands in the past and would
/// delete the member on the very next reconcile.
#[must_use]
pub fn expiry(now: Timestamp, ttl_seconds: u64) -> (Timestamp, Timestamp) {
    let ttl = i64::try_from(ttl_seconds).unwrap_or(i64::MAX);
    let end = Timestamp::from_second(now.as_second().saturating_add(ttl)).unwrap_or(Timestamp::MAX);
    (now, end)
}

/// What a waiting claim knows about its pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolWaitState<'a> {
    pub pool_name: &'a str,
    /// Whether the pool exists at all.
    ///
    /// Separate from `warm` on purpose. A pool that exists but has not been
    /// reconciled yet has no `Warm` condition, and folding the two together
    /// makes a brand-new pool report as a missing one.
    pub pool_exists: bool,
    /// The pool's own `Warm` condition as `(reason, message)`, when it has
    /// published one.
    pub warm: Option<(&'a str, &'a str)>,
}

/// Why a claim could not bind, as `(reason, message)` for its `Bound=False`
/// condition.
///
/// The distinction is the point (ADR-0047 Decision 11). "No member
/// available" reads as a busy pool and sends whoever is debugging it to
/// look at capacity; a pool that does not exist, or one stuck because
/// `spec.readiness` names a condition nothing publishes, needs a different
/// answer. So the claim repeats the pool's own diagnosis rather than making
/// a consumer go read a second object to discover it is waiting for
/// something that will never happen.
#[must_use]
pub fn wait_reason(state: &PoolWaitState) -> (&'static str, String) {
    if !state.pool_exists {
        return (
            pool_condition_reasons::POOL_NOT_FOUND,
            format!("pool {} does not exist", state.pool_name),
        );
    }
    let detail = match state.warm {
        Some((reason, message)) => format!(" (pool: {reason}: {message})"),
        // The pool exists but has not reported yet — a normal state for the
        // first seconds of its life, and emphatically not a missing pool.
        None => " (pool has not published a Warm condition yet)".to_string(),
    };
    (
        pool_condition_reasons::NO_MEMBER_AVAILABLE,
        format!(
            "no Ready, unclaimed member in pool {}{detail}",
            state.pool_name
        ),
    )
}

/// Whether a deadline has passed. Inclusive of the deadline instant.
#[must_use]
pub fn is_expired(expires_at: Timestamp, now: Timestamp) -> bool {
    now >= expires_at
}

/// What a bound claim mirrors from its member, so a consumer needs one GET
/// rather than two (ADR-0047, ADR-0045).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MirroredMemberState {
    /// The member's addresses.
    pub addresses: Vec<MachineAddress>,
    /// The member's vTPM EK certificate(s) — the anchor a verifier checks an
    /// attestation quote against (ADR-0045). Empty for a member with no
    /// vTPM, which is every member of a pool that does not need attestation.
    pub tpm_endorsement_certificates: Vec<String>,
}

/// Project a bound member's observable state onto its claim.
///
/// Pure so the mirror is testable without a cluster — `hold` in
/// [`super::claim`] does nothing but apply this.
#[must_use]
pub fn mirrored_member_state(vm: &VirtualMachine) -> MirroredMemberState {
    let Some(status) = vm.status.as_ref() else {
        return MirroredMemberState::default();
    };
    MirroredMemberState {
        addresses: status.addresses.clone(),
        tpm_endorsement_certificates: status.tpm_endorsement_certificates.clone(),
    }
}

#[cfg(test)]
#[path = "claim_plan_tests.rs"]
mod claim_plan_tests;
