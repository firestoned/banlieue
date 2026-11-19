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

use std::cmp::{Ordering, Reverse};
use std::collections::BTreeMap;
use std::fmt;

use banlieue_api::banlieue::{
    ANNOTATION_SUBJECT_ID, ANNOTATION_SUBJECT_ISSUER, ClaimSubject, LABEL_CLAIM, LABEL_POOL,
    Provider, SelectionStrategy, VirtualMachine, VirtualMachineClaimSpec,
    VirtualMachineClaimStatus, pool_condition_reasons,
};
use banlieue_api::common::{LabelSelector, MachineAddress};
use k8s_openapi::jiff::Timestamp;
use kube::ResourceExt;

use super::pool_plan::{MemberPhase, MemberView};
use super::scheduler::{is_stale, is_unschedulable, label_selector_matches};

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
/// Decision 4), with no placement information: [`rank_pick`] under
/// `LongestReady` with empty inputs.
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
    let placements = BTreeMap::new();
    let held = BTreeMap::new();
    rank_pick(
        pools,
        &SelectionInputs {
            strategy: SelectionStrategy::LongestReady,
            placements: &placements,
            subject_held: &held,
        },
    )
    .map(|r| r.pick)
}

/// A failure domain: the pair `(providerName, failureDomain)` from a
/// member's `status.scheduled` (ADR-0135 Decision 4).
///
/// The pair, not the name: failure-domain names are scoped to their
/// `Provider`, and two Cloud Hypervisor or libvirt hosts commonly carry the
/// same single name, which alone would merge two hosts into one bucket.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FailureDomainKey {
    pub provider: String,
    pub failure_domain: String,
}

impl fmt::Display for FailureDomainKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.failure_domain)
    }
}

/// Where a member runs, and whether that host is fit to hand out from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberPlacement {
    /// `None` for a member with no `status.scheduled`, which a Ready member
    /// should never be. Ranks after every placed member.
    pub domain: Option<FailureDomainKey>,
    /// `false` when the member's `Provider` is cordoned, its heartbeat is
    /// `Stale`, or it cannot be found (ADR-0135 Decision 3).
    pub host_healthy: bool,
}

impl MemberPlacement {
    /// What the ranking assumes for a member it has no placement for:
    /// nothing is known, so nothing is held against it.
    fn unknown() -> Self {
        Self {
            domain: None,
            host_healthy: true,
        }
    }
}

/// The failure domain `vm` was placed in, from its `status.scheduled`.
#[must_use]
pub fn member_domain(vm: &VirtualMachine) -> Option<FailureDomainKey> {
    let scheduled = vm.status.as_ref()?.scheduled.as_ref()?;
    Some(FailureDomainKey {
        provider: scheduled.provider_name.clone(),
        failure_domain: scheduled.failure_domain.clone(),
    })
}

/// Where `vm` runs and whether its host is healthy, judged with the
/// scheduler's own predicates so "unhealthy" means one thing in banlieue:
/// cordoned (ADR-0124), `Stale` heartbeat (ADR-0102), or a `Provider` that
/// is not among `providers` at all.
///
/// # Arguments
/// * `vm` - the member.
/// * `providers` - the candidate `Provider`s for the claim's namespace,
///   resolved as ADR-0133 Decision 1 resolves them.
#[must_use]
pub fn member_placement(vm: &VirtualMachine, providers: &[Provider]) -> MemberPlacement {
    let domain = member_domain(vm);
    let host_healthy = domain.as_ref().is_some_and(|d| {
        providers
            .iter()
            .find(|p| p.name_any() == d.provider)
            .is_some_and(|p| !is_unschedulable(p) && !is_stale(p))
    });
    MemberPlacement {
        domain,
        host_healthy,
    }
}

/// How many members `subject` already holds in each failure domain: the
/// count `Spread` minimises (ADR-0135 Decision 5).
///
/// A member is held when it carries a claim label, is not being deleted,
/// and both subject annotations equal `subject`'s. Issuer and id are
/// matched together, never the id alone: the same id at two issuers is two
/// people (ADR-0047).
///
/// # Arguments
/// * `members` - every pool member in the claim's namespace.
/// * `subject` - the claim's `spec.subject`.
#[must_use]
pub fn subject_held(
    members: &[VirtualMachine],
    subject: &ClaimSubject,
) -> BTreeMap<FailureDomainKey, u32> {
    let mut held = BTreeMap::new();
    for vm in members {
        if vm.metadata.deletion_timestamp.is_some() || !vm.labels().contains_key(LABEL_CLAIM) {
            continue;
        }
        let annotations = vm.annotations();
        let same_subject = annotations.get(ANNOTATION_SUBJECT_ISSUER) == Some(&subject.issuer)
            && annotations.get(ANNOTATION_SUBJECT_ID) == Some(&subject.id);
        if !same_subject {
            continue;
        }
        if let Some(domain) = member_domain(vm) {
            *held.entry(domain).or_insert(0) += 1;
        }
    }
    held
}

/// What [`rank_pick`] needs beyond the members themselves.
#[derive(Clone, Copy, Debug)]
pub struct SelectionInputs<'a> {
    pub strategy: SelectionStrategy,
    /// Each member's placement, by member name. A member absent here is
    /// ranked as [`MemberPlacement::unknown`].
    pub placements: &'a BTreeMap<String, MemberPlacement>,
    /// [`subject_held`] for the claim's subject. Read by `Spread` only.
    pub subject_held: &'a BTreeMap<FailureDomainKey, u32>,
}

/// Why the chosen member was chosen, for the `Bound` condition message
/// (ADR-0135 Decision 6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickDetail {
    pub domain: Option<FailureDomainKey>,
    pub host_healthy: bool,
    /// Members the subject already held in that domain.
    pub subject_held: u32,
}

/// The member a claim should bind, with why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankedPick {
    pub pick: Pick,
    pub detail: PickDetail,
}

/// One Ready candidate with everything its rank depends on.
struct Ranked<'a> {
    pool: &'a str,
    name: &'a str,
    stale: bool,
    ready_for_secs: u64,
    placement: MemberPlacement,
    available: u32,
    held: u32,
}

impl Ranked<'_> {
    /// The strategy's own key (ADR-0135 Decisions 4 and 5): unplaced last,
    /// then fewest held by the subject, then the fullest domain. Each part
    /// is constant for a strategy that does not read it.
    fn strategy_key(&self, strategy: SelectionStrategy) -> (bool, u32, Reverse<u32>) {
        let unplaced = self.placement.domain.is_none();
        match strategy {
            SelectionStrategy::LongestReady => (false, 0, Reverse(0)),
            SelectionStrategy::MostAvailable => (unplaced, 0, Reverse(self.available)),
            SelectionStrategy::Spread => (unplaced, self.held, Reverse(self.available)),
        }
    }

    /// The whole order (ADR-0135 Decision 2). Lower is better.
    fn cmp(&self, other: &Self, strategy: SelectionStrategy) -> Ordering {
        self.stale
            .cmp(&other.stale)
            .then_with(|| (!self.placement.host_healthy).cmp(&!other.placement.host_healthy))
            .then_with(|| {
                self.strategy_key(strategy)
                    .cmp(&other.strategy_key(strategy))
            })
            .then_with(|| other.ready_for_secs.cmp(&self.ready_for_secs))
            .then_with(|| self.name.cmp(other.name))
    }
}

/// Choose the member to bind, by the claim's strategy (ADR-0135).
///
/// The order, lowest first:
/// 1. Ready only (a `Claimed` member is never offered again);
/// 2. current image revision of the member's own pool before stale, so no
///    placement preference keeps an unpatched build in circulation;
/// 3. a healthy host before a cordoned, `Stale` or unknown one, under every
///    strategy;
/// 4. the strategy's key: none for `LongestReady`; for `MostAvailable` the
///    failure domain with the most Ready members; for `Spread` the domain
///    where the subject holds fewest, then `MostAvailable`'s key;
/// 5. the member Ready longest;
/// 6. member name, so two controllers reading one snapshot agree.
///
/// Only reorders: if any member is Ready, one is returned. Availability is
/// counted over every matched pool, from this snapshot rather than from a
/// pool's lagging `status.available`.
///
/// # Arguments
/// * `pools` - each matched pool with its members and current revision.
/// * `inputs` - the strategy, placements and the subject's holdings.
#[must_use]
pub fn rank_pick(pools: &[PoolMembers], inputs: &SelectionInputs) -> Option<RankedPick> {
    let ready: Vec<(&PoolMembers, &MemberView, MemberPlacement)> = pools
        .iter()
        .flat_map(|p| p.views.iter().map(move |m| (p, m)))
        .filter(|(_, m)| m.phase == MemberPhase::Ready)
        .map(|(p, m)| {
            let placement = inputs
                .placements
                .get(&m.name)
                .cloned()
                .unwrap_or_else(MemberPlacement::unknown);
            (p, m, placement)
        })
        .collect();

    let mut available: BTreeMap<&FailureDomainKey, u32> = BTreeMap::new();
    for (_, _, placement) in &ready {
        if let Some(domain) = &placement.domain {
            *available.entry(domain).or_insert(0) += 1;
        }
    }

    let ranked: Vec<Ranked> = ready
        .iter()
        .map(|(p, m, placement)| {
            let (avail, held) = placement.domain.as_ref().map_or((0, 0), |d| {
                (
                    available.get(d).copied().unwrap_or(0),
                    inputs.subject_held.get(d).copied().unwrap_or(0),
                )
            });
            Ranked {
                pool: p.pool,
                name: &m.name,
                stale: m.image_revision != p.current_revision,
                ready_for_secs: m.ready_for_secs,
                placement: placement.clone(),
                available: avail,
                held,
            }
        })
        .collect();

    ranked
        .into_iter()
        .min_by(|a, b| a.cmp(b, inputs.strategy))
        .map(|r| RankedPick {
            pick: Pick {
                pool: r.pool.to_string(),
                member: r.name.to_string(),
            },
            detail: PickDetail {
                domain: r.placement.domain,
                host_healthy: r.placement.host_healthy,
                subject_held: r.held,
            },
        })
}

/// The `Bound` condition message for a fresh bind (ADR-0135 Decision 6):
/// the strategy, the domain the member came from, and whether the choice
/// was a fallback (an unhealthy host, or a domain the subject already
/// occupied). `detail` is `None` when finishing a bind made on an earlier
/// pass, which knows only the member.
#[must_use]
pub fn bound_message(
    member: &str,
    strategy: SelectionStrategy,
    detail: Option<&PickDetail>,
) -> String {
    let Some(detail) = detail else {
        return format!("bound to {member}");
    };
    let mut msg = format!("bound to {member} by {strategy}");
    if let Some(domain) = &detail.domain {
        msg.push_str(&format!(" from {domain}"));
    }
    if !detail.host_healthy {
        msg.push_str(
            "; its host is cordoned or not reporting, and nothing better ranked was Ready",
        );
    }
    if detail.subject_held > 0 {
        msg.push_str(&format!(
            "; the subject already holds {} member(s) in that domain",
            detail.subject_held
        ));
    }
    msg
}

/// The `Bound` message a hold publishes: the bind's own explanation when
/// it is about this member, so a hold does not erase it on the next pass;
/// otherwise the plain "bound to {member}".
#[must_use]
pub fn hold_message(existing: Option<&str>, member: &str) -> String {
    let plain = format!("bound to {member}");
    match existing {
        Some(msg) if msg == plain || msg.starts_with(&format!("{plain} ")) => msg.to_string(),
        _ => plain,
    }
}

/// What a pass that finishes an earlier pass's bind (the member already
/// labelled ours, ADR-0132 Decision 5) does with the claim's status, given a
/// fresh read of the claim rather than the copy the pass was handed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishBind {
    /// The earlier pass published its bind, with the message that names the
    /// strategy and the domain (ADR-0135 Decision 6). Nothing is written:
    /// the plain message this pass could write would erase that one.
    AlreadyBound,
    /// The earlier pass stopped between labelling the member and publishing
    /// the bind. Publish it, saying only what this pass knows.
    Publish,
}

/// `AlreadyBound` when `fresh` already names `member` as the bound one.
#[must_use]
pub fn finish_bind(fresh: Option<&VirtualMachineClaimStatus>, member: &str) -> FinishBind {
    let published = fresh
        .and_then(|s| s.virtual_machine_ref.as_ref())
        .is_some_and(|r| r.name == member);
    if published {
        FinishBind::AlreadyBound
    } else {
        FinishBind::Publish
    }
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
    /// The certificate subject CN of the member's node-attestation identity
    /// (ADR-0143 Decision 3), for a member with no vTPM. `None` otherwise.
    pub attestation_subject: Option<String>,
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
        attestation_subject: status.attestation_subject.clone(),
        pool,
    }
}

#[cfg(test)]
#[path = "claim_plan_tests.rs"]
mod claim_plan_tests;
