// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
use super::*;

const REV: &str = "rev-b";
const OLD: &str = "rev-a";

fn inputs() -> PoolInputs {
    PoolInputs {
        warm_replicas: 3,
        max_replicas: 10,
        max_surge: 2,
        provisioning_timeout_secs: 1800,
        max_idle_secs: None,
        recycle_on_image_change: true,
        current_image_revision: REV.to_string(),
        address_range: None,
    }
}

fn member(name: &str, phase: MemberPhase, age: u64, rev: &str) -> MemberView {
    MemberView {
        name: name.to_string(),
        phase,
        age_secs: age,
        ready_for_secs: if phase == MemberPhase::Ready { age } else { 0 },
        image_revision: rev.to_string(),
        address: None,
    }
}

fn deleted(p: &PoolPlan) -> Vec<&str> {
    p.delete.iter().map(|(n, _)| n.as_str()).collect()
}

#[test]
fn empty_pool_fills_up_to_max_surge_only() {
    let p = plan(&inputs(), &[]);
    assert_eq!(p.create.len(), 2, "warm=3 but max_surge=2");
    assert!(p.delete.is_empty());
}

#[test]
fn in_flight_provisioning_counts_toward_target_and_surge() {
    let m = vec![
        member("a", MemberPhase::Provisioning, 60, REV),
        member("b", MemberPhase::Ready, 900, REV),
    ];
    let p = plan(&inputs(), &m);
    // want 3, have 1 ready + 1 provisioning, surge room is 2 - 1 = 1.
    assert_eq!(p.create.len(), 1);
}

#[test]
fn steady_state_is_a_no_op() {
    let m = vec![
        member("a", MemberPhase::Ready, 900, REV),
        member("b", MemberPhase::Ready, 800, REV),
        member("c", MemberPhase::Ready, 700, REV),
        member("d", MemberPhase::Claimed, 5000, OLD),
    ];
    assert_eq!(plan(&inputs(), &m), PoolPlan::default());
}

#[test]
fn claimed_members_are_never_deleted_even_when_stale_and_ancient() {
    let mut i = inputs();
    i.max_idle_secs = Some(10);
    let m = vec![member("c", MemberPhase::Claimed, 999_999, OLD)];
    let p = plan(&i, &m);
    assert!(p.delete.is_empty());
}

#[test]
fn provisioning_timeout_deletes_and_replaces_in_same_pass() {
    let m = vec![
        member("stuck", MemberPhase::Provisioning, 1801, REV),
        member("a", MemberPhase::Ready, 900, REV),
        member("b", MemberPhase::Ready, 900, REV),
    ];
    let p = plan(&inputs(), &m);
    assert_eq!(
        p.delete,
        vec![("stuck".to_string(), DeleteReason::ProvisioningTimeout)]
    );
    assert_eq!(p.create.len(), 1);
}

#[test]
fn stale_image_members_are_kept_until_fresh_ones_are_ready() {
    let m = vec![
        member("s1", MemberPhase::Ready, 3000, OLD),
        member("s2", MemberPhase::Ready, 2000, OLD),
        member("s3", MemberPhase::Ready, 1000, OLD),
    ];
    let p = plan(&inputs(), &m);
    assert!(p.delete.is_empty(), "nothing fresh yet, keep all stale");
    assert_eq!(p.create.len(), 2, "start building fresh, bounded by surge");
}

#[test]
fn stale_members_drain_oldest_first_as_fresh_ones_arrive() {
    let m = vec![
        member("s1", MemberPhase::Ready, 3000, OLD),
        member("s2", MemberPhase::Ready, 2000, OLD),
        member("s3", MemberPhase::Ready, 1000, OLD),
        member("f1", MemberPhase::Ready, 100, REV),
        member("f2", MemberPhase::Ready, 90, REV),
    ];
    let p = plan(&inputs(), &m);
    // 2 fresh ready, so only 1 stale is still needed: keep the newest (s3).
    assert_eq!(deleted(&p), vec!["s1", "s2"]);
    assert_eq!(p.create.len(), 1);
}

#[test]
fn no_headroom_trades_stale_for_replacements_one_for_one() {
    let mut i = inputs();
    i.max_replicas = 3; // == warm_replicas, no room to surge
    let m = vec![
        member("s1", MemberPhase::Ready, 3000, OLD),
        member("s2", MemberPhase::Ready, 2000, OLD),
        member("s3", MemberPhase::Ready, 1000, OLD),
    ];
    let p = plan(&i, &m);
    assert_eq!(deleted(&p), vec!["s1", "s2"], "max_surge=2, oldest first");
    assert_eq!(p.create.len(), 2);
    assert_eq!(p.blocked_on_capacity, 0);
}

#[test]
fn max_replicas_blocks_refill_when_everything_is_claimed() {
    let mut i = inputs();
    i.max_replicas = 4;
    let m = vec![
        member("c1", MemberPhase::Claimed, 10, REV),
        member("c2", MemberPhase::Claimed, 10, REV),
        member("c3", MemberPhase::Claimed, 10, REV),
    ];
    let p = plan(&i, &m);
    assert_eq!(p.create.len(), 1);
    assert_eq!(p.blocked_on_capacity, 1, "wanted 2 (surge), room for 1");
}

#[test]
fn idle_expiry_uses_ready_time_not_age() {
    let mut i = inputs();
    i.max_idle_secs = Some(600);
    let mut slow = member("slow-install", MemberPhase::Ready, 5000, REV);
    slow.ready_for_secs = 30; // old VM, but only just became Ready
    let m = vec![
        slow,
        member("b", MemberPhase::Ready, 700, REV),
        member("c", MemberPhase::Ready, 100, REV),
        member("d", MemberPhase::Ready, 100, REV),
    ];
    let p = plan(&i, &m);
    // b is idle-expired; 3 fresh remain, so b is not needed as cover.
    assert_eq!(p.delete, vec![("b".to_string(), DeleteReason::IdleExpired)]);
    assert!(p.create.is_empty());
}

#[test]
fn scale_down_trims_oldest_fresh_surplus() {
    let mut i = inputs();
    i.warm_replicas = 1;
    let m = vec![
        member("a", MemberPhase::Ready, 300, REV),
        member("b", MemberPhase::Ready, 200, REV),
        member("c", MemberPhase::Ready, 100, REV),
    ];
    let p = plan(&i, &m);
    assert_eq!(deleted(&p), vec!["a", "b"]);
    assert!(p.create.is_empty());
}

#[test]
fn addresses_skip_every_existing_holder_including_deleting() {
    let mut i = inputs();
    i.address_range = Some(AddressRange {
        start: Ipv4Addr::new(10, 0, 0, 10),
        end: Ipv4Addr::new(10, 0, 0, 13),
    });
    let mut gone = member("gone", MemberPhase::Deleting, 50, REV);
    gone.address = Some(Ipv4Addr::new(10, 0, 0, 10));
    let mut held = member("held", MemberPhase::Claimed, 50, REV);
    held.address = Some(Ipv4Addr::new(10, 0, 0, 12));
    let p = plan(&i, &[gone, held]);
    let got: Vec<Ipv4Addr> = p.create.iter().filter_map(|c| c.address).collect();
    assert_eq!(
        got,
        vec![Ipv4Addr::new(10, 0, 0, 11), Ipv4Addr::new(10, 0, 0, 13)]
    );
    assert_eq!(p.blocked_on_addresses, 0);
}

#[test]
fn address_exhaustion_is_reported_not_silently_dropped() {
    let mut i = inputs();
    i.address_range = Some(AddressRange {
        start: Ipv4Addr::new(10, 0, 0, 10),
        end: Ipv4Addr::new(10, 0, 0, 10),
    });
    let p = plan(&i, &[]);
    assert_eq!(p.create.len(), 1);
    assert_eq!(p.blocked_on_addresses, 1);
}

#[test]
fn inverted_range_allocates_nothing() {
    let mut i = inputs();
    i.address_range = Some(AddressRange {
        start: Ipv4Addr::new(10, 0, 0, 20),
        end: Ipv4Addr::new(10, 0, 0, 10),
    });
    let p = plan(&i, &[]);
    assert!(p.create.is_empty());
    assert_eq!(p.blocked_on_addresses, 2);
}

#[test]
fn invariants_hold_across_a_sweep() {
    // Cheap exhaustive-ish sweep instead of a proptest dependency.
    for warm in 0..4u32 {
        for max in 0..6u32 {
            for surge in 0..3u32 {
                for claimed in 0..4usize {
                    for prov in 0..3usize {
                        for stale in 0..3usize {
                            let mut i = inputs();
                            i.warm_replicas = warm;
                            i.max_replicas = max;
                            i.max_surge = surge;
                            let mut m = Vec::new();
                            for n in 0..claimed {
                                m.push(member(&format!("c{n}"), MemberPhase::Claimed, 10, OLD));
                            }
                            for n in 0..prov {
                                m.push(member(
                                    &format!("p{n}"),
                                    MemberPhase::Provisioning,
                                    10,
                                    REV,
                                ));
                            }
                            for n in 0..stale {
                                m.push(member(
                                    &format!("s{n}"),
                                    MemberPhase::Ready,
                                    10 + n as u64,
                                    OLD,
                                ));
                            }
                            let p = plan(&i, &m);
                            assert!(
                                p.delete.iter().all(|(n, _)| !n.starts_with('c')),
                                "claimed deleted"
                            );
                            assert!(
                                prov as u32 + p.create.len() as u32 <= surge.max(prov as u32),
                                "surge exceeded"
                            );
                            let alive = m.len() - p.delete.len() + p.create.len();
                            assert!(
                                alive as u32 <= max.max(m.len() as u32 - p.delete.len() as u32),
                                "max_replicas exceeded by creates"
                            );
                        }
                    }
                }
            }
        }
    }
}
