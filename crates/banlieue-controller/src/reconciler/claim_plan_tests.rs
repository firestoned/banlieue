// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `claim_plan.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::reconciler::pool_plan::{MemberPhase, MemberView};
    use banlieue_api::banlieue::{ClaimSubject, VirtualMachineClaimSpec, pool_condition_reasons};
    use banlieue_api::common::{LabelSelector, LocalObjectReference};
    use std::collections::BTreeMap;

    fn subject() -> ClaimSubject {
        ClaimSubject {
            issuer: "https://issuer.example.com".into(),
            id: "0c3b7f2e".into(),
        }
    }

    const CURRENT: &str = "sha256:current";
    const STALE: &str = "sha256:stale";

    fn member(name: &str, phase: MemberPhase, ready_for_secs: u64, revision: &str) -> MemberView {
        MemberView {
            name: name.to_string(),
            phase,
            age_secs: ready_for_secs + 60,
            ready_for_secs,
            image_revision: revision.to_string(),
            address: None,
        }
    }

    // ------------------------------------------------------------------
    // member_bound_to: a claim never binds a second member
    // ------------------------------------------------------------------

    #[test]
    fn finds_the_member_already_labelled_for_this_claim() {
        let members = [("m1", None), ("m2", Some("c1")), ("m3", Some("other"))];
        assert_eq!(member_bound_to(members, "c1").as_deref(), Some("m2"));
    }

    #[test]
    fn a_member_of_another_claim_is_not_ours() {
        let members = [("m1", Some("other")), ("m2", None)];
        assert_eq!(member_bound_to(members, "c1"), None);
    }

    #[test]
    fn an_unbound_claim_owns_no_member() {
        assert_eq!(member_bound_to([("m1", None), ("m2", None)], "c1"), None);
    }

    // ------------------------------------------------------------------
    // pick_member (ADR-0047 Decision 4)
    // ------------------------------------------------------------------

    #[test]
    fn picks_nothing_from_an_empty_pool() {
        assert_eq!(pick_member(&[], CURRENT), None);
    }

    /// The invariant the whole design rests on: a member that has been
    /// handed to one subject is never offered to another. `Claimed` is not
    /// a transient state a claim may race through — it is permanent.
    #[test]
    fn never_picks_a_claimed_member() {
        let views = [member("a", MemberPhase::Claimed, 900, CURRENT)];
        assert_eq!(pick_member(&views, CURRENT), None);
    }

    #[test]
    fn never_picks_a_provisioning_or_deleting_member() {
        let views = [
            member("a", MemberPhase::Provisioning, 0, CURRENT),
            member("b", MemberPhase::Deleting, 900, CURRENT),
        ];
        assert_eq!(pick_member(&views, CURRENT), None);
    }

    /// A fresh member wins even when a stale one has been Ready far longer.
    /// Handing out yesterday's build while today's sits idle is how an
    /// unpatched image survives a rebuild that was supposed to retire it.
    #[test]
    fn prefers_the_current_image_revision_over_a_longer_ready_stale_one() {
        let views = [
            member("stale-but-old", MemberPhase::Ready, 86_400, STALE),
            member("fresh", MemberPhase::Ready, 10, CURRENT),
        ];
        assert_eq!(pick_member(&views, CURRENT).as_deref(), Some("fresh"));
    }

    /// A stale sandbox beats no sandbox. Its lifetime is bounded by the
    /// claim TTL anyway, and refusing to bind would mean a consumer waits
    /// out a full install during exactly the rollout the pool was supposed
    /// to hide.
    #[test]
    fn falls_back_to_a_stale_member_when_nothing_fresh_is_ready() {
        let views = [
            member("stale", MemberPhase::Ready, 60, STALE),
            member(
                "fresh-but-provisioning",
                MemberPhase::Provisioning,
                0,
                CURRENT,
            ),
        ];
        assert_eq!(pick_member(&views, CURRENT).as_deref(), Some("stale"));
    }

    /// Longest-Ready first, so idle expiry rarely has to fire: the member
    /// closest to being reaped for staleness is the one handed out.
    #[test]
    fn among_equals_picks_the_one_ready_longest() {
        let views = [
            member("young", MemberPhase::Ready, 30, CURRENT),
            member("old", MemberPhase::Ready, 3_600, CURRENT),
        ];
        assert_eq!(pick_member(&views, CURRENT).as_deref(), Some("old"));
    }

    /// Name is the final tiebreak purely so the choice is deterministic —
    /// two controllers reading the same snapshot must pick the same member,
    /// or the 409 that resolves a race never happens because they never
    /// collide in the first place.
    #[test]
    fn breaks_a_full_tie_on_name() {
        let views = [
            member("bbb", MemberPhase::Ready, 100, CURRENT),
            member("aaa", MemberPhase::Ready, 100, CURRENT),
        ];
        assert_eq!(pick_member(&views, CURRENT).as_deref(), Some("aaa"));
    }

    /// A pool that has never published a revision (no member created yet,
    /// or an image without a build digest) must still be claimable rather
    /// than treating every member as stale and refusing to choose.
    #[test]
    fn picks_a_member_when_the_pool_has_no_revision_yet() {
        let views = [member("a", MemberPhase::Ready, 10, "")];
        assert_eq!(pick_member(&views, "").as_deref(), Some("a"));
    }

    // ------------------------------------------------------------------
    // expiry (ADR-0047 Decision 5)
    // ------------------------------------------------------------------

    /// The TTL is counted from binding, not from creation: a claim that
    /// waited ten minutes for capacity still gets its full lifetime.
    #[test]
    fn expiry_runs_from_the_bind_instant() {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let (bound_at, expires_at) = expiry(now, 900);
        assert_eq!(bound_at, now);
        assert_eq!(expires_at.as_second(), now.as_second() + 900);
    }

    /// A TTL large enough to overflow must saturate rather than wrap — a
    /// wrapped deadline lands in the past and deletes the member on the
    /// next reconcile, which is the exact opposite of what was asked for.
    #[test]
    fn a_huge_ttl_saturates_instead_of_wrapping() {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let (_, expires_at) = expiry(now, u64::MAX);
        assert!(expires_at > now, "expiry must not wrap into the past");
    }

    #[test]
    fn expiry_is_inclusive_of_the_deadline_instant() {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let (_, expires_at) = expiry(now, 60);
        assert!(!is_expired(expires_at, now));
        assert!(is_expired(expires_at, expires_at));
        assert!(is_expired(
            expires_at,
            Timestamp::from_second(expires_at.as_second() + 1).unwrap()
        ));
    }

    // ------------------------------------------------------------------
    // next_step: the whole state machine, as a pure function
    // ------------------------------------------------------------------

    fn bound(name: &str, exists: bool) -> ClaimInputs {
        ClaimInputs {
            deleting: false,
            expired: false,
            bound: Some(BoundMember {
                name: name.to_string(),
                exists,
            }),
        }
    }

    #[test]
    fn an_unbound_claim_binds_the_candidate() {
        let inputs = ClaimInputs {
            deleting: false,
            expired: false,
            bound: None,
        };
        assert_eq!(
            next_step(&inputs, Some("a".into())),
            ClaimStep::Bind { member: "a".into() }
        );
    }

    /// Pending is unbounded on purpose (ADR-0047 Decision 11): the pool may
    /// simply be filling, which is the normal state after a burst.
    #[test]
    fn an_unbound_claim_with_no_candidate_waits() {
        let inputs = ClaimInputs {
            deleting: false,
            expired: false,
            bound: None,
        };
        assert_eq!(next_step(&inputs, None), ClaimStep::Wait);
    }

    #[test]
    fn a_bound_claim_holds_its_member() {
        assert_eq!(
            next_step(&bound("a", true), None),
            ClaimStep::Hold { member: "a".into() }
        );
    }

    /// Terminal, and deliberately not a rebind. A consumer holding this
    /// claim believes it is talking to one specific VM; silently handing it
    /// a different one would be worse than failing.
    #[test]
    fn a_bound_claim_whose_member_vanished_fails_rather_than_rebinding() {
        assert_eq!(
            next_step(&bound("a", false), Some("b".into())),
            ClaimStep::Fail { member: "a".into() }
        );
    }

    /// Deletion outranks everything, including a healthy binding: once a
    /// claim is going away the only remaining job is to destroy the member.
    #[test]
    fn deletion_outranks_a_healthy_binding() {
        let mut inputs = bound("a", true);
        inputs.deleting = true;
        assert_eq!(
            next_step(&inputs, None),
            ClaimStep::Release {
                member: Some("a".into()),
                deleting: true,
            }
        );
    }

    /// Expiry is a hard deadline, not a grace period. The member goes
    /// whether or not the consumer is finished with it.
    #[test]
    fn expiry_releases_a_healthy_binding() {
        let mut inputs = bound("a", true);
        inputs.expired = true;
        assert_eq!(
            next_step(&inputs, None),
            ClaimStep::Release {
                member: Some("a".into()),
                deleting: false,
            }
        );
    }

    /// A claim deleted before it ever bound has nothing to destroy, but
    /// still has a finalizer to drop — so it must reach Release, not Wait.
    #[test]
    fn deleting_an_unbound_claim_releases_with_no_member() {
        let inputs = ClaimInputs {
            deleting: true,
            expired: false,
            bound: None,
        };
        assert_eq!(
            next_step(&inputs, Some("a".into())),
            ClaimStep::Release {
                member: None,
                deleting: true,
            }
        );
    }

    /// A Failed claim being deleted must still release: its member is
    /// already gone, but the finalizer is not, and a finalizer nothing
    /// removes leaves the object undeletable forever.
    #[test]
    fn deleting_a_failed_claim_still_releases() {
        let mut inputs = bound("a", false);
        inputs.deleting = true;
        assert_eq!(
            next_step(&inputs, None),
            ClaimStep::Release {
                member: Some("a".into()),
                deleting: true,
            }
        );
    }

    // ------------------------------------------------------------------
    // wait_reason: "no capacity" vs "pool misconfigured" (Decision 11)
    // ------------------------------------------------------------------

    /// A claim against a pool that does not exist must not report "no
    /// member available" — that reads as a full pool and sends whoever is
    /// debugging it to look at capacity instead of at the typo in poolRef.
    #[test]
    fn a_missing_pool_is_reported_as_such() {
        let (reason, message) = wait_reason(&PoolWaitState {
            selection: PoolSelection::Named("sandbox-pool"),
            pools: &[],
        });
        assert_eq!(reason, pool_condition_reasons::POOL_NOT_FOUND);
        assert!(message.contains("sandbox-pool"), "got: {message}");
    }

    /// A pool that is merely filling is the normal state after a burst.
    #[test]
    fn a_filling_pool_reports_no_member_available() {
        let (reason, message) = wait_reason(&PoolWaitState {
            selection: PoolSelection::Named("sandbox-pool"),
            pools: &[PoolWarm {
                name: "sandbox-pool",
                warm: Some(("Filling", "1 of 2 members ready")),
            }],
        });
        assert_eq!(reason, pool_condition_reasons::NO_MEMBER_AVAILABLE);
        assert!(message.contains("Filling"), "got: {message}");
    }

    /// The case Decision 11 exists for: a pool stuck forever because
    /// `spec.readiness` names a condition nothing publishes. The claim must
    /// carry that reason itself — otherwise a consumer sees only "waiting"
    /// and has to know to go read a second object to learn it is waiting
    /// for something that will never happen.
    #[test]
    fn a_stuck_pool_surfaces_its_own_reason_on_the_claim() {
        let (reason, message) = wait_reason(&PoolWaitState {
            selection: PoolSelection::Named("sandbox-pool"),
            pools: &[PoolWarm {
                name: "sandbox-pool",
                warm: Some((
                    "ReadinessSignalAbsent",
                    "no member has published the \"GuestReady\" condition",
                )),
            }],
        });
        assert_eq!(reason, pool_condition_reasons::NO_MEMBER_AVAILABLE);
        assert!(
            message.contains("ReadinessSignalAbsent") && message.contains("GuestReady"),
            "the claim must repeat the pool's own diagnosis, got: {message}"
        );
    }

    /// The case the live suite caught and the unit tests had missed,
    /// because they were written with the same wrong mental model as the
    /// code: a pool that **exists but has not been reconciled yet** has no
    /// `Warm` condition. Reporting that as `PoolNotFound` sends whoever is
    /// debugging it hunting for a typo in `poolRef` that is not there —
    /// the single most misleading thing a waiting claim could say.
    #[test]
    fn a_pool_that_has_not_reported_yet_is_not_a_missing_pool() {
        let (reason, message) = wait_reason(&PoolWaitState {
            selection: PoolSelection::Named("sandbox-pool"),
            pools: &[PoolWarm {
                name: "sandbox-pool",
                warm: None,
            }],
        });
        assert_eq!(reason, pool_condition_reasons::NO_MEMBER_AVAILABLE);
        assert!(
            !message.contains("does not exist"),
            "an existing pool must never be described as missing: {message}"
        );
        assert!(message.contains("sandbox-pool"), "got: {message}");
    }

    // ------------------------------------------------------------------
    // ADR-0132: waiting on a selector
    // ------------------------------------------------------------------

    /// A selector that matches nothing is a labelling problem, not a
    /// capacity one, and not a typo in a pool name either: its own reason.
    #[test]
    fn a_selector_matching_no_pool_says_so() {
        let selector = LabelSelector::default();
        let (reason, message) = wait_reason(&PoolWaitState {
            selection: PoolSelection::Selector(&selector),
            pools: &[],
        });
        assert_eq!(reason, pool_condition_reasons::NO_POOL_MATCHES);
        assert!(message.contains("poolSelector"), "got: {message}");
    }

    /// A claim the CEL rule should have refused (an older CRD, say) still
    /// gets a reason that names the fix rather than a silent wait.
    #[test]
    fn a_claim_naming_no_pool_says_which_field_is_missing() {
        let (reason, message) = wait_reason(&PoolWaitState {
            selection: PoolSelection::Unspecified,
            pools: &[],
        });
        assert_eq!(reason, pool_condition_reasons::NO_POOL_MATCHES);
        assert!(
            message.contains("poolRef") && message.contains("poolSelector"),
            "got: {message}"
        );
    }

    /// Every matched pool's own diagnosis is repeated, so a consumer waiting
    /// on three pools learns why each one has nothing to give.
    #[test]
    fn waiting_on_several_pools_repeats_each_pools_diagnosis() {
        let selector = LabelSelector::default();
        let (reason, message) = wait_reason(&PoolWaitState {
            selection: PoolSelection::Selector(&selector),
            pools: &[
                PoolWarm {
                    name: "pool-a",
                    warm: Some(("Filling", "0 of 2 members ready")),
                },
                PoolWarm {
                    name: "pool-b",
                    warm: None,
                },
            ],
        });
        assert_eq!(reason, pool_condition_reasons::NO_MEMBER_AVAILABLE);
        assert!(message.contains("2 matching pools"), "got: {message}");
        assert!(
            message.contains("pool-a") && message.contains("Filling"),
            "got: {message}"
        );
        assert!(message.contains("pool-b"), "got: {message}");
    }

    /// A condition message is bounded; a namespace with dozens of pools
    /// must not produce one line per pool.
    #[test]
    fn waiting_on_many_pools_lists_a_few_and_counts_the_rest() {
        let selector = LabelSelector::default();
        let names: Vec<String> = (0..8).map(|i| format!("pool-{i}")).collect();
        let pools: Vec<PoolWarm> = names
            .iter()
            .map(|n| PoolWarm {
                name: n,
                warm: None,
            })
            .collect();
        let (_, message) = wait_reason(&PoolWaitState {
            selection: PoolSelection::Selector(&selector),
            pools: &pools,
        });
        assert!(message.contains("pool-4"), "got: {message}");
        assert!(!message.contains("pool-5"), "got: {message}");
        assert!(message.contains("and 3 more"), "got: {message}");
    }

    // ------------------------------------------------------------------
    // ADR-0132: which pools a claim draws from
    // ------------------------------------------------------------------

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn selection_comes_from_whichever_field_is_set() {
        let named = VirtualMachineClaimSpec {
            pool_ref: Some(LocalObjectReference {
                name: "sandbox-pool".into(),
            }),
            pool_selector: None,
            subject: subject(),
            ttl_seconds: 60,
        };
        assert_eq!(pool_selection(&named), PoolSelection::Named("sandbox-pool"));

        let selected = VirtualMachineClaimSpec {
            pool_ref: None,
            pool_selector: Some(LabelSelector::default()),
            ..named.clone()
        };
        assert_eq!(
            pool_selection(&selected),
            PoolSelection::Selector(&LabelSelector::default())
        );

        let neither = VirtualMachineClaimSpec {
            pool_ref: None,
            pool_selector: None,
            ..named
        };
        assert_eq!(pool_selection(&neither), PoolSelection::Unspecified);
    }

    #[test]
    fn a_named_selection_matches_that_pool_only() {
        let sel = PoolSelection::Named("sandbox-pool");
        assert!(sel.matches("sandbox-pool", &labels(&[])));
        assert!(!sel.matches("other-pool", &labels(&[])));
    }

    /// `poolSelector: {}` is the written-down "any pool" (Decision 2).
    #[test]
    fn an_empty_selector_matches_every_pool() {
        let empty = LabelSelector::default();
        let sel = PoolSelection::Selector(&empty);
        assert!(sel.matches("a", &labels(&[])));
        assert!(sel.matches("b", &labels(&[("tier", "gold")])));
    }

    #[test]
    fn a_selector_matches_on_pool_labels_not_names() {
        let gold = LabelSelector {
            match_labels: labels(&[("tier", "gold")]),
            ..Default::default()
        };
        let sel = PoolSelection::Selector(&gold);
        assert!(sel.matches("anything", &labels(&[("tier", "gold")])));
        assert!(!sel.matches("gold", &labels(&[("tier", "silver")])));
    }

    /// Omitting both fields never widens to "every pool".
    #[test]
    fn an_unspecified_selection_matches_nothing() {
        assert!(!PoolSelection::Unspecified.matches("a", &labels(&[])));
    }

    // ------------------------------------------------------------------
    // ADR-0132 Decision 4: one ranking across every matched pool
    // ------------------------------------------------------------------

    #[test]
    fn picks_the_longest_ready_member_across_pools() {
        let a = [member("a-1", MemberPhase::Ready, 10, CURRENT)];
        let b = [member("b-1", MemberPhase::Ready, 500, CURRENT)];
        let pick = pick_across_pools(&[
            PoolMembers {
                pool: "pool-a",
                current_revision: CURRENT,
                views: &a,
            },
            PoolMembers {
                pool: "pool-b",
                current_revision: CURRENT,
                views: &b,
            },
        ]);
        assert_eq!(
            pick,
            Some(Pick {
                pool: "pool-b".into(),
                member: "b-1".into(),
            })
        );
    }

    /// An empty pool next to a warm one is the case this ADR exists for.
    #[test]
    fn an_empty_pool_does_not_block_a_warm_neighbour() {
        let a: [MemberView; 0] = [];
        let b = [member("b-1", MemberPhase::Ready, 5, CURRENT)];
        let pick = pick_across_pools(&[
            PoolMembers {
                pool: "pool-a",
                current_revision: CURRENT,
                views: &a,
            },
            PoolMembers {
                pool: "pool-b",
                current_revision: CURRENT,
                views: &b,
            },
        ]);
        assert_eq!(pick.map(|p| p.pool), Some("pool-b".to_string()));
    }

    /// Staleness is judged against each member's own pool: pool B's
    /// revision is not pool A's, and a member current in its own pool beats
    /// one that is stale in its own, however long the stale one was Ready.
    #[test]
    fn staleness_is_judged_against_the_members_own_pool() {
        let a = [member("a-1", MemberPhase::Ready, 900, "sha256:a-old")];
        let b = [member("b-1", MemberPhase::Ready, 5, "sha256:b-new")];
        let pick = pick_across_pools(&[
            PoolMembers {
                pool: "pool-a",
                current_revision: "sha256:a-new",
                views: &a,
            },
            PoolMembers {
                pool: "pool-b",
                current_revision: "sha256:b-new",
                views: &b,
            },
        ]);
        assert_eq!(pick.map(|p| p.member), Some("b-1".to_string()));
    }

    #[test]
    fn never_picks_a_claimed_member_in_any_pool() {
        let a = [member("a-1", MemberPhase::Claimed, 900, CURRENT)];
        let b = [member("b-1", MemberPhase::Provisioning, 0, CURRENT)];
        let pick = pick_across_pools(&[
            PoolMembers {
                pool: "pool-a",
                current_revision: CURRENT,
                views: &a,
            },
            PoolMembers {
                pool: "pool-b",
                current_revision: CURRENT,
                views: &b,
            },
        ]);
        assert_eq!(pick, None);
    }

    #[test]
    fn a_full_tie_across_pools_breaks_on_member_name() {
        let a = [member("zzz", MemberPhase::Ready, 60, CURRENT)];
        let b = [member("aaa", MemberPhase::Ready, 60, CURRENT)];
        let pick = pick_across_pools(&[
            PoolMembers {
                pool: "pool-a",
                current_revision: CURRENT,
                views: &a,
            },
            PoolMembers {
                pool: "pool-b",
                current_revision: CURRENT,
                views: &b,
            },
        ]);
        assert_eq!(pick.map(|p| p.member), Some("aaa".to_string()));
    }

    // ------------------------------------------------------------------
    // ADR-0045 — the claim mirrors its member's attestation anchor
    // ------------------------------------------------------------------

    /// The claim's attestation anchor: a consumer verifies the guest against
    /// this EK certificate (ADR-0045, ADR-0049), and one GET of the claim is
    /// supposed to yield it.
    #[test]
    fn a_bound_claim_mirrors_its_members_endorsement_certificate() {
        const PEM: &str = "-----BEGIN CERTIFICATE-----\nstub\n-----END CERTIFICATE-----";
        let mut vm = member_vm();
        vm.status = Some(banlieue_api::banlieue::VirtualMachineStatus {
            tpm_endorsement_certificates: vec![PEM.to_string()],
            ..Default::default()
        });

        let got = mirrored_member_state(&vm);
        assert_eq!(got.tpm_endorsement_certificates, vec![PEM.to_string()]);
    }

    /// A member with no vTPM mirrors nothing, and a member with no status at
    /// all must not panic the reconciler.
    #[test]
    fn a_member_without_a_vtpm_mirrors_no_certificate() {
        let vm = member_vm();
        let got = mirrored_member_state(&vm);
        assert!(got.tpm_endorsement_certificates.is_empty());
        assert!(got.addresses.is_empty());
    }

    /// ADR-0132 Decision 6: a claim bound before `status.poolRef` existed
    /// learns its pool on the next hold, from the member's own label, which
    /// is the record the bind left on the member.
    #[test]
    fn a_bound_claim_mirrors_the_pool_its_member_came_from() {
        let mut vm = member_vm();
        vm.metadata.labels = Some(BTreeMap::from([(
            banlieue_api::banlieue::LABEL_POOL.to_string(),
            "sandbox-pool".to_string(),
        )]));
        assert_eq!(
            mirrored_member_state(&vm).pool.as_deref(),
            Some("sandbox-pool")
        );
        assert_eq!(mirrored_member_state(&member_vm()).pool, None);
    }

    /// A pool member with no status yet.
    fn member_vm() -> banlieue_api::banlieue::VirtualMachine {
        use banlieue_api::banlieue::{
            MigrationPolicy, PlacementSpec, VirtualMachine, VirtualMachineSpec,
        };
        use banlieue_api::common::{LocalObjectReference, PowerState};
        VirtualMachine {
            metadata: kube::api::ObjectMeta {
                name: Some("member-1".to_string()),
                ..Default::default()
            },
            spec: VirtualMachineSpec {
                class_ref: LocalObjectReference { name: "c".into() },
                image_ref: LocalObjectReference { name: "i".into() },
                placement: PlacementSpec::default(),
                desired_power_state: PowerState::PoweredOn,
                user_data: None,
                migration_policy: MigrationPolicy::Automatic,
                paused: false,
                network_overrides: Vec::new(),
                hardware_override: None,
                folder: None,
            },
            status: None,
        }
    }
}
