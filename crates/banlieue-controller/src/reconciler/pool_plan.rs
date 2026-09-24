// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Pure planning logic for `VirtualMachinePool` (roadmap 17, ADR-0046).
//!
//! No Kubernetes I/O and no clock reads: the reconciler in
//! [`super::pool`] gathers a [`PoolInputs`] snapshot, calls [`plan`], and
//! applies the resulting [`PoolPlan`]. Everything that decides *what* a pool
//! does lives here so it is table-testable without a cluster, matching the
//! `scheduler.rs` / `migration.rs` split already used in this crate.
//!
//! # Invariants [`plan`] guarantees
//!
//! 1. A claimed member is never deleted by the pool. Only its
//!    `VirtualMachineClaim` ending (release, expiry, deletion) removes it.
//! 2. A member is never returned to the warm set after being claimed. Reuse
//!    across identities is the one thing this design must never do.
//! 3. `alive + creates <= max_replicas`, always.
//! 4. `provisioning + creates <= max_surge`, always. A Deferred-mode install
//!    is minutes of sustained datastore and CPU load per member, so an
//!    unbounded refill after a claim burst would hurt the hosts the claimed
//!    sandboxes are running on.
//! 5. Stale members (old image revision, or idle past `max_idle_secs`) are
//!    replaced surge-style: a stale Ready member is only deleted once enough
//!    fresh Ready members exist to keep `warm_replicas` claimable, unless
//!    `max_replicas` leaves no room to build the replacement first.
//! 6. An address is never handed to a new member while any existing member,
//!    including one still being finalized, holds it.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

/// Where a pool member is in its life. Derived by the reconciler from the
/// member `VirtualMachine`'s conditions, labels and deletion timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberPhase {
    /// Created, not yet reporting the pool's readiness condition. For a
    /// Deferred-mode image this covers the whole unattended install.
    Provisioning,
    /// Reporting ready and carrying no claim label. Claimable.
    Ready,
    /// Bound to a `VirtualMachineClaim`. Invisible to pool sizing except
    /// that it counts against `max_replicas`.
    Claimed,
    /// Has a deletion timestamp. Still holds its name and address until the
    /// provider's finalizer completes.
    Deleting,
}

/// One existing member, as the planner needs to see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberView {
    pub name: String,
    pub phase: MemberPhase,
    /// Seconds since `metadata.creationTimestamp`.
    pub age_secs: u64,
    /// Seconds since the member became Ready. `0` while Provisioning.
    pub ready_for_secs: u64,
    /// Value of the member's image-revision label. Compared for equality
    /// only, so any stable digest of the resolved `VMImage` build works.
    pub image_revision: String,
    /// Static address this member was stamped with, if the pool does
    /// static addressing.
    pub address: Option<Ipv4Addr>,
}

/// Inclusive IPv4 range a pool allocates member addresses from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressRange {
    pub start: Ipv4Addr,
    pub end: Ipv4Addr,
}

/// Sizing and hygiene knobs, copied out of `VirtualMachinePoolSpec`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolInputs {
    pub warm_replicas: u32,
    pub max_replicas: u32,
    pub max_surge: u32,
    pub provisioning_timeout_secs: u64,
    /// `None` disables idle recycling.
    pub max_idle_secs: Option<u64>,
    pub recycle_on_image_change: bool,
    pub current_image_revision: String,
    /// Candidate addresses, in the order to draw from them (ADR-0056). Empty
    /// means the pool does not stamp addresses (DHCP, or the class's own
    /// IPAM).
    pub address_ranges: Vec<AddressRange>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteReason {
    /// Provisioning longer than `provisioning_timeout_secs`. Treated as
    /// poisoned: never repaired, never handed out.
    ProvisioningTimeout,
    /// Ready but built from an image revision the pool no longer wants.
    StaleImage,
    /// Ready and unclaimed for longer than `max_idle_secs`.
    IdleExpired,
    /// More claimable members than `warm_replicas` (the pool was scaled
    /// down).
    ScaleDown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMember {
    /// Address to stamp into `spec.networkOverrides`, when the pool does
    /// static addressing.
    pub address: Option<Ipv4Addr>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PoolPlan {
    pub create: Vec<NewMember>,
    pub delete: Vec<(String, DeleteReason)>,
    /// Members the pool wanted to create but could not, because the address
    /// range is exhausted. Surfaced as a condition by the reconciler.
    pub blocked_on_addresses: u32,
    /// Members the pool wanted to create but could not, because of
    /// `max_replicas`. Surfaced as a condition by the reconciler.
    pub blocked_on_capacity: u32,
}

/// Decide what to create and delete for one pool, given a snapshot of its
/// members. Deterministic: same inputs, same plan.
#[must_use]
pub fn plan(inputs: &PoolInputs, members: &[MemberView]) -> PoolPlan {
    let mut out = PoolPlan::default();

    // Pass 1: poisoned provisioners go first. They free surge slots and
    // capacity for their own replacements in this same pass.
    let mut provisioning: u32 = 0;
    for m in members
        .iter()
        .filter(|m| m.phase == MemberPhase::Provisioning)
    {
        if m.age_secs > inputs.provisioning_timeout_secs {
            out.delete
                .push((m.name.clone(), DeleteReason::ProvisioningTimeout));
        } else {
            provisioning += 1;
        }
    }

    // Pass 2: split Ready members into fresh and stale.
    let mut fresh_ready: Vec<&MemberView> = Vec::new();
    let mut stale_ready: Vec<(&MemberView, DeleteReason)> = Vec::new();
    for m in members.iter().filter(|m| m.phase == MemberPhase::Ready) {
        let stale_image =
            inputs.recycle_on_image_change && m.image_revision != inputs.current_image_revision;
        let idle_expired = inputs
            .max_idle_secs
            .is_some_and(|max| m.ready_for_secs > max);
        if stale_image {
            stale_ready.push((m, DeleteReason::StaleImage));
        } else if idle_expired {
            stale_ready.push((m, DeleteReason::IdleExpired));
        } else {
            fresh_ready.push(m);
        }
    }
    // Oldest first, name as the tiebreak, so the plan is deterministic.
    stale_ready.sort_by(|a, b| {
        b.0.age_secs
            .cmp(&a.0.age_secs)
            .then_with(|| a.0.name.cmp(&b.0.name))
    });
    fresh_ready.sort_by(|a, b| {
        b.age_secs
            .cmp(&a.age_secs)
            .then_with(|| a.name.cmp(&b.name))
    });

    let fresh_ready_n = len_u32(fresh_ready.len());
    let claimed = count(members, MemberPhase::Claimed);

    // Pass 3: scale-down. Only ever trims *fresh* surplus; stale surplus is
    // handled by pass 4, and claimed members are untouchable.
    if fresh_ready_n > inputs.warm_replicas {
        let surplus = (fresh_ready_n - inputs.warm_replicas) as usize;
        for m in fresh_ready.iter().take(surplus) {
            out.delete.push((m.name.clone(), DeleteReason::ScaleDown));
        }
    }
    let fresh_ready_kept = fresh_ready_n.min(inputs.warm_replicas);

    // Pass 4: surge replacement of stale members. Keep exactly as many
    // stale members as are still needed to cover `warm_replicas`, newest
    // stale kept, oldest deleted.
    let stale_needed = inputs.warm_replicas.saturating_sub(fresh_ready_kept) as usize;
    let mut stale_kept: usize = stale_ready.len().min(stale_needed);
    for (m, reason) in stale_ready.iter().take(stale_ready.len() - stale_kept) {
        out.delete.push((m.name.clone(), *reason));
    }

    // Pass 5: how many fresh members to build. Stale members do not count
    // toward the target: they are being replaced, not relied on.
    let wanted = inputs
        .warm_replicas
        .saturating_sub(fresh_ready_kept)
        .saturating_sub(provisioning);
    let surge_room = inputs.max_surge.saturating_sub(provisioning);
    let mut to_create = wanted.min(surge_room);

    // Pass 6: `max_replicas`. Alive = everything that will still exist after
    // this plan's deletes, excluding members already Deleting.
    let alive = |stale_kept: usize| -> u32 {
        claimed + provisioning + fresh_ready_kept + len_u32(stale_kept)
    };
    let mut room = inputs.max_replicas.saturating_sub(alive(stale_kept));
    if to_create > room && stale_kept > 0 {
        // No room to surge. Trade kept stale members for replacements, one
        // for one, oldest first. This is the only path that lets claimable
        // capacity dip below `warm_replicas`, and only when the operator
        // left no headroom between `warm_replicas` and `max_replicas`.
        let trade = ((to_create - room) as usize).min(stale_kept);
        let already = stale_ready.len() - stale_kept;
        for (m, reason) in stale_ready.iter().skip(already).take(trade) {
            out.delete.push((m.name.clone(), *reason));
        }
        stale_kept -= trade;
        room = inputs.max_replicas.saturating_sub(alive(stale_kept));
    }
    if to_create > room {
        out.blocked_on_capacity = to_create - room;
        to_create = room;
    }

    // Pass 7: addresses. Every member that still exists holds its address,
    // including ones this plan deletes and ones already Deleting: the
    // backend VM is there until the provider finalizer finishes. Entries are
    // drawn in list order, each low to high (ADR-0056), so the plan stays
    // deterministic and an operator controls draw order by list order.
    if inputs.address_ranges.is_empty() {
        for _ in 0..to_create {
            out.create.push(NewMember { address: None });
        }
    } else {
        let used: BTreeSet<u32> = members
            .iter()
            .filter_map(|m| m.address)
            .map(u32::from)
            .collect();
        let mut granted: u32 = 0;
        'ranges: for range in &inputs.address_ranges {
            let (lo, hi) = (u32::from(range.start), u32::from(range.end));
            let mut next = lo;
            while lo <= hi && next <= hi {
                if granted >= to_create {
                    break 'ranges;
                }
                if !used.contains(&next) {
                    out.create.push(NewMember {
                        address: Some(Ipv4Addr::from(next)),
                    });
                    granted += 1;
                }
                if next == u32::MAX {
                    break;
                }
                next += 1;
            }
        }
        out.blocked_on_addresses = to_create - granted;
    }

    out
}

fn count(members: &[MemberView], phase: MemberPhase) -> u32 {
    len_u32(members.iter().filter(|m| m.phase == phase).count())
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "pool_plan_tests.rs"]
mod pool_plan_tests;
