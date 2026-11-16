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

use std::collections::BTreeMap;

use banlieue_api::banlieue::{
    LABEL_POOL, VirtualMachine, VirtualMachineClaimSpec, pool_condition_reasons,
};
use banlieue_api::common::{LabelSelector, MachineAddress};
use k8s_openapi::jiff::Timestamp;
use kube::ResourceExt;

use super::pool_plan::{MemberPhase, MemberView};
use super::scheduler::label_selector_matches;

/// How many pools a waiting claim's message lists before it only counts
/// the rest. A condition message is read by a person; one line per pool in
/// a namespace with dozens of them is not.
const MAX_POOLS_IN_WAIT_MESSAGE: usize = 5;

/// Which pools a claim draws from (ADR-0132 Decision 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolSelection<'a> {
    /// `spec.poolRef`: exactly this pool.
    Named(&'a str),
    /// `spec.poolSelector`: every pool in the namespace whose labels match.
    /// The empty selector matches every pool.
    Selector(&'a LabelSelector),
    /// Neither field is set. The CRD's CEL rule refuses this, so it is
    /// reachable only past an older CRD; it matches nothing, because an
    /// omitted pool never widens to "any pool" (Decision 2).
    Unspecified,
}

impl PoolSelection<'_> {
    /// Whether the pool `name`, carrying `labels`, is one this claim may
    /// draw from.
    #[must_use]
    pub fn matches(&self, name: &str, labels: &BTreeMap<String, String>) -> bool {
        match self {
            Self::Named(wanted) => *wanted == name,
            Self::Selector(selector) => label_selector_matches(selector, labels),
            Self::Unspecified => false,
        }
    }
}

/// The claim's [`PoolSelection`]. `poolRef` wins if, past the CEL rule,
/// both are somehow set: it is the narrower of the two.
#[must_use]
pub fn pool_selection(spec: &VirtualMachineClaimSpec) -> PoolSelection<'_> {
    if let Some(pool_ref) = &spec.pool_ref {
        return PoolSelection::Named(&pool_ref.name);
    }
    match &spec.pool_selector {
        Some(selector) => PoolSelection::Selector(selector),
        None => PoolSelection::Unspecified,
    }
}

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

/// The member already labelled for `claim`, if any: `(name, claim label)`
/// pairs, as listed fresh from the API server.
///
/// The member's label is written first, under a `resourceVersion`
/// precondition, and the claim's `virtualMachineRef` second. A reconcile
/// from a cached claim that predates its own status patch sees no ref and
/// would pick again; checking the members first makes it finish the bind it
/// already made instead of taking a second member (one claim held both warm
/// members of a pool in the roadmap 17 phase G run, 2026-09-28).
#[must_use]
pub fn member_bound_to<'a>(
    members: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    claim: &str,
) -> Option<String> {
    members
        .into_iter()
        .find(|(_, label)| *label == Some(claim))
        .map(|(name, _)| name.to_string())
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
    pick_across_pools(&[PoolMembers {
        pool: "",
        current_revision,
        views,
    }])
    .map(|p| p.member)
}

/// One matched pool's members, as [`pick_across_pools`] ranks them.
#[derive(Clone, Copy, Debug)]
pub struct PoolMembers<'a> {
    pub pool: &'a str,
    /// The pool's `status.imageRevision`: what "current" means for *these*
    /// members.
    pub current_revision: &'a str,
    pub views: &'a [MemberView],
}

/// The member a claim should bind, and the pool it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    pub pool: String,
    pub member: String,
}

/// Choose the member to bind from every matched pool at once (ADR-0132
/// Decision 4).
///
/// [`pick_member`]'s order over the union: Ready only, then current image
/// revision *of the member's own pool*, then the one Ready longest, then
/// member name. Names are unique in a namespace, so the order is total and
/// two controllers reading one snapshot agree. No pool is preferred over
/// another.
///
/// # Arguments
/// * `pools` - each matched pool with its members and current revision.
#[must_use]
pub fn pick_across_pools(pools: &[PoolMembers]) -> Option<Pick> {
    pools
        .iter()
        .flat_map(|p| p.views.iter().map(move |m| (p, m)))
        .filter(|(_, m)| m.phase == MemberPhase::Ready)
        .min_by(|(pa, a), (pb, b)| {
            let a_stale = a.image_revision != pa.current_revision;
            let b_stale = b.image_revision != pb.current_revision;
            a_stale
                .cmp(&b_stale)
                .then_with(|| b.ready_for_secs.cmp(&a.ready_for_secs))
                .then_with(|| a.name.cmp(&b.name))
        })
        .map(|(p, m)| Pick {
            pool: p.pool.to_string(),
            member: m.name.clone(),
        })
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

/// One matched pool, as a waiting claim reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolWarm<'a> {
    pub name: &'a str,
    /// The pool's own `Warm` condition as `(reason, message)`, when it has
    /// published one. `None` is a pool that exists but has not reported
    /// yet, a normal state for the first seconds of its life and
    /// emphatically not a missing pool.
    pub warm: Option<(&'a str, &'a str)>,
}

/// What a waiting claim knows about the pools it searched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolWaitState<'a> {
    pub selection: PoolSelection<'a>,
    /// Every pool the selection matched. Empty when none exists (for a
    /// `poolRef`) or none matches (for a `poolSelector`).
    pub pools: &'a [PoolWarm<'a>],
}

/// Why a claim could not bind, as `(reason, message)` for its `Bound=False`
/// condition.
///
/// The distinction is the point (ADR-0047 Decision 11, ADR-0132
/// Decision 7). "No member available" reads as a busy pool and sends
/// whoever is debugging it to look at capacity; a pool that does not exist,
/// a selector that matches nothing, or a pool stuck because
/// `spec.readiness` names a condition nothing publishes each needs a
/// different answer. So the claim repeats each pool's own diagnosis rather
/// than making a consumer go read a second object to discover it is waiting
/// for something that will never happen.
#[must_use]
pub fn wait_reason(state: &PoolWaitState) -> (&'static str, String) {
    if state.pools.is_empty() {
        return match state.selection {
            PoolSelection::Named(name) => (
                pool_condition_reasons::POOL_NOT_FOUND,
                format!("pool {name} does not exist"),
            ),
            PoolSelection::Selector(_) => (
                pool_condition_reasons::NO_POOL_MATCHES,
                "no pool in this namespace matches spec.poolSelector".to_string(),
            ),
            PoolSelection::Unspecified => (
                pool_condition_reasons::NO_POOL_MATCHES,
                "the claim sets neither spec.poolRef nor spec.poolSelector; \
                 use poolSelector: {} for any pool in the namespace"
                    .to_string(),
            ),
        };
    }
    if let [only] = state.pools {
        return (
            pool_condition_reasons::NO_MEMBER_AVAILABLE,
            format!(
                "no Ready, unclaimed member in pool {}{}",
                only.name,
                warm_detail(only.warm)
            ),
        );
    }
    let listed: Vec<String> = state
        .pools
        .iter()
        .take(MAX_POOLS_IN_WAIT_MESSAGE)
        .map(|p| format!("{}{}", p.name, warm_detail(p.warm)))
        .collect();
    let rest = state.pools.len().saturating_sub(MAX_POOLS_IN_WAIT_MESSAGE);
    let more = if rest > 0 {
        format!("; and {rest} more")
    } else {
        String::new()
    };
    (
        pool_condition_reasons::NO_MEMBER_AVAILABLE,
        format!(
            "no Ready, unclaimed member in any of {} matching pools: {}{more}",
            state.pools.len(),
            listed.join("; ")
        ),
    )
}

/// A pool's `Warm` diagnosis as a parenthesised suffix.
fn warm_detail(warm: Option<(&str, &str)>) -> String {
    match warm {
        Some((reason, message)) => format!(" (pool: {reason}: {message})"),
        None => " (pool has not published a Warm condition yet)".to_string(),
    }
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
    /// The pool the member came from, from its `banlieue.io/pool` label:
    /// `status.poolRef`, backfilled for a claim bound before that field
    /// existed (ADR-0132 Decision 6).
    pub pool: Option<String>,
}

/// Project a bound member's observable state onto its claim.
///
/// Pure so the mirror is testable without a cluster — `hold` in
/// [`super::claim`] does nothing but apply this.
#[must_use]
pub fn mirrored_member_state(vm: &VirtualMachine) -> MirroredMemberState {
    let pool = vm.labels().get(LABEL_POOL).cloned();
    let Some(status) = vm.status.as_ref() else {
        return MirroredMemberState {
            pool,
            ..MirroredMemberState::default()
        };
    };
    MirroredMemberState {
        addresses: status.addresses.clone(),
        tpm_endorsement_certificates: status.tpm_endorsement_certificates.clone(),
        pool,
    }
}

#[cfg(test)]
#[path = "claim_plan_tests.rs"]
mod claim_plan_tests;
