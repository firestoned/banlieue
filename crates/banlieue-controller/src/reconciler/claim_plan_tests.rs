// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `claim_plan.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::reconciler::pool_plan::{MemberPhase, MemberView};
    use banlieue_api::banlieue::{
        ClaimPhase, ClaimSubject, VirtualMachineClaimSpec, VirtualMachineClaimStatus,
        pool_condition_reasons,
    };
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
            selection: None,
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

    // ------------------------------------------------------------------
    // ADR-0135: selection strategies and host health
    // ------------------------------------------------------------------

    use banlieue_api::banlieue::{
        ANNOTATION_SUBJECT_ID, ANNOTATION_SUBJECT_ISSUER, LABEL_CLAIM, Provider,
        ScheduledPlacement, SelectionStrategy, VirtualMachineStatus,
    };

    fn domain(provider: &str, fd: &str) -> FailureDomainKey {
        FailureDomainKey {
            provider: provider.to_string(),
            failure_domain: fd.to_string(),
        }
    }

    fn placed(domain: FailureDomainKey, host_healthy: bool) -> MemberPlacement {
        MemberPlacement {
            domain: Some(domain),
            host_healthy,
        }
    }

    /// A member VM placed on `provider`/`fd`.
    fn placed_vm(name: &str, provider: &str, fd: &str) -> banlieue_api::banlieue::VirtualMachine {
        let mut vm = member_vm();
        vm.metadata.name = Some(name.to_string());
        vm.status = Some(VirtualMachineStatus {
            scheduled: Some(ScheduledPlacement {
                provider_name: provider.to_string(),
                provider_class: "cloud-hypervisor".to_string(),
                failure_domain: fd.to_string(),
                resolved_storage: Vec::new(),
                resolved_networks: Vec::new(),
                scheduled_at: None,
            }),
            ..Default::default()
        });
        vm
    }

    /// A member bound to `subject`, as the bind leaves it.
    fn held_vm(
        name: &str,
        provider: &str,
        fd: &str,
        issuer: &str,
        id: &str,
    ) -> banlieue_api::banlieue::VirtualMachine {
        let mut vm = placed_vm(name, provider, fd);
        vm.metadata.labels = Some(BTreeMap::from([(
            LABEL_CLAIM.to_string(),
            format!("claim-{name}"),
        )]));
        vm.metadata.annotations = Some(BTreeMap::from([
            (ANNOTATION_SUBJECT_ISSUER.to_string(), issuer.to_string()),
            (ANNOTATION_SUBJECT_ID.to_string(), id.to_string()),
        ]));
        vm
    }

    fn provider(name: &str, labels: serde_json::Value, heartbeat: Option<&str>) -> Provider {
        let mut json = serde_json::json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "Provider",
            "metadata": { "name": name, "labels": labels },
            "spec": {
                "providerClassRef": { "name": "cloud-hypervisor" },
                "connection": { "endpoint": "unix:///run/ch.sock" }
            }
        });
        if let Some(hb) = heartbeat {
            json["status"] = serde_json::json!({ "workload": { "namespace": "banlieue-system", "heartbeat": hb } });
        }
        serde_json::from_value(json).expect("provider fixture must parse")
    }

    fn one_pool<'a>(views: &'a [MemberView]) -> [PoolMembers<'a>; 1] {
        [PoolMembers {
            pool: "pool",
            current_revision: CURRENT,
            views,
        }]
    }

    fn inputs<'a>(
        strategy: SelectionStrategy,
        placements: &'a BTreeMap<String, MemberPlacement>,
        subject_held: &'a BTreeMap<FailureDomainKey, u32>,
    ) -> SelectionInputs<'a> {
        SelectionInputs {
            strategy,
            placements,
            subject_held,
        }
    }

    fn picked(pick: Option<RankedPick>) -> Option<String> {
        pick.map(|p| p.pick.member)
    }

    // --- placement: where a member runs ---------------------------------

    #[test]
    fn a_members_domain_is_its_provider_and_failure_domain() {
        let vm = placed_vm("m", "host-a", "default");
        assert_eq!(member_domain(&vm), Some(domain("host-a", "default")));
        assert_eq!(member_domain(&member_vm()), None);
    }

    #[test]
    fn a_member_on_a_cordoned_host_is_unhealthy() {
        let vm = placed_vm("m", "host-a", "default");
        let cordoned = provider(
            "host-a",
            serde_json::json!({ "banlieue.io/unschedulable": "true" }),
            None,
        );
        assert!(!member_placement(&vm, &[cordoned]).host_healthy);
    }

    #[test]
    fn a_member_on_a_stale_host_is_unhealthy() {
        let vm = placed_vm("m", "host-a", "default");
        let stale = provider("host-a", serde_json::json!({}), Some("Stale"));
        assert!(!member_placement(&vm, &[stale]).host_healthy);
    }

    /// `Absent` is not filtered by the scheduler (ADR-0102), so it is not
    /// unhealthy here either: one meaning of "unhealthy" in banlieue.
    #[test]
    fn a_member_on_a_live_or_absent_heartbeat_host_is_healthy() {
        let vm = placed_vm("m", "host-a", "default");
        for hb in [None, Some("Alive"), Some("Absent")] {
            let p = provider("host-a", serde_json::json!({}), hb);
            let got = member_placement(&vm, &[p]);
            assert!(got.host_healthy, "{hb:?}");
            assert_eq!(got.domain, Some(domain("host-a", "default")));
        }
    }

    #[test]
    fn a_member_whose_provider_cannot_be_found_is_unhealthy() {
        let vm = placed_vm("m", "host-a", "default");
        let other = provider("host-b", serde_json::json!({}), None);
        assert!(!member_placement(&vm, &[other]).host_healthy);
        assert!(!member_placement(&member_vm(), &[]).host_healthy);
    }

    // --- subject_held: what Spread counts -------------------------------

    #[test]
    fn counts_the_subjects_members_per_domain() {
        let s = subject();
        let members = [
            held_vm("a", "host-a", "default", &s.issuer, &s.id),
            held_vm("b", "host-a", "default", &s.issuer, &s.id),
            held_vm("c", "host-b", "default", &s.issuer, &s.id),
        ];
        let got = subject_held(&members, &s);
        assert_eq!(got.get(&domain("host-a", "default")), Some(&2));
        assert_eq!(got.get(&domain("host-b", "default")), Some(&1));
    }

    /// The same id at another issuer is another person (ADR-0047): the id
    /// is never matched alone.
    #[test]
    fn the_same_id_at_another_issuer_is_not_the_subject() {
        let s = subject();
        let members = [held_vm(
            "a",
            "host-a",
            "default",
            "https://other.example.com",
            &s.id,
        )];
        assert!(subject_held(&members, &s).is_empty());
    }

    #[test]
    fn unclaimed_and_deleting_members_are_not_held() {
        let s = subject();
        let mut unclaimed = held_vm("a", "host-a", "default", &s.issuer, &s.id);
        unclaimed.metadata.labels = None;
        let mut deleting = held_vm("b", "host-a", "default", &s.issuer, &s.id);
        deleting.metadata.deletion_timestamp =
            Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                k8s_openapi::jiff::Timestamp::UNIX_EPOCH,
            ));
        assert!(subject_held(&[unclaimed, deleting], &s).is_empty());
    }

    // --- rank_pick -------------------------------------------------------

    /// With nothing known about placement, the ranking is exactly
    /// ADR-0047's: `pick_across_pools` is `rank_pick` with empty inputs.
    #[test]
    fn empty_inputs_reproduce_the_longest_ready_order() {
        let views = [
            member("young", MemberPhase::Ready, 30, CURRENT),
            member("old", MemberPhase::Ready, 3_600, CURRENT),
        ];
        let none = BTreeMap::new();
        let held = BTreeMap::new();
        let got = rank_pick(
            &one_pool(&views),
            &inputs(SelectionStrategy::LongestReady, &none, &held),
        );
        assert_eq!(picked(got), Some("old".to_string()));
    }

    /// Decision 3: under every strategy, including the default, a member
    /// on a cordoned or silent host is taken last.
    #[test]
    fn a_healthy_host_beats_a_longer_ready_member_on_an_unhealthy_one() {
        let views = [
            member("on-cordoned", MemberPhase::Ready, 3_600, CURRENT),
            member("on-healthy", MemberPhase::Ready, 30, CURRENT),
        ];
        let placements = BTreeMap::from([
            ("on-cordoned".to_string(), placed(domain("a", "d"), false)),
            ("on-healthy".to_string(), placed(domain("b", "d"), true)),
        ]);
        let held = BTreeMap::new();
        for strategy in [
            SelectionStrategy::LongestReady,
            SelectionStrategy::MostAvailable,
            SelectionStrategy::Spread,
        ] {
            let got = rank_pick(&one_pool(&views), &inputs(strategy, &placements, &held));
            assert_eq!(picked(got), Some("on-healthy".to_string()), "{strategy}");
        }
    }

    /// Decision 2: a strategy and health only reorder. A cordoned host's
    /// member still beats no sandbox.
    #[test]
    fn an_unhealthy_host_is_still_used_when_it_is_all_there_is() {
        let views = [member("only", MemberPhase::Ready, 30, CURRENT)];
        let placements = BTreeMap::from([("only".to_string(), placed(domain("a", "d"), false))]);
        let held = BTreeMap::new();
        let got = rank_pick(
            &one_pool(&views),
            &inputs(SelectionStrategy::Spread, &placements, &held),
        )
        .expect("a Ready member must be bound");
        assert_eq!(got.pick.member, "only");
        assert!(!got.detail.host_healthy);
    }

    /// Decision 2: no placement preference outranks a security rebuild.
    #[test]
    fn current_revision_still_beats_host_health() {
        let views = [
            member("stale-healthy", MemberPhase::Ready, 30, STALE),
            member("fresh-cordoned", MemberPhase::Ready, 30, CURRENT),
        ];
        let placements = BTreeMap::from([
            ("stale-healthy".to_string(), placed(domain("a", "d"), true)),
            (
                "fresh-cordoned".to_string(),
                placed(domain("b", "d"), false),
            ),
        ]);
        let held = BTreeMap::new();
        let got = rank_pick(
            &one_pool(&views),
            &inputs(SelectionStrategy::LongestReady, &placements, &held),
        );
        assert_eq!(picked(got), Some("fresh-cordoned".to_string()));
    }

    #[test]
    fn most_available_takes_from_the_fullest_domain() {
        let views = [
            member("a-1", MemberPhase::Ready, 3_600, CURRENT),
            member("b-1", MemberPhase::Ready, 10, CURRENT),
            member("b-2", MemberPhase::Ready, 20, CURRENT),
        ];
        let placements = BTreeMap::from([
            ("a-1".to_string(), placed(domain("a", "d"), true)),
            ("b-1".to_string(), placed(domain("b", "d"), true)),
            ("b-2".to_string(), placed(domain("b", "d"), true)),
        ]);
        let held = BTreeMap::new();
        let got = rank_pick(
            &one_pool(&views),
            &inputs(SelectionStrategy::MostAvailable, &placements, &held),
        )
        .unwrap();
        // Fullest domain first, then the one Ready longest within it.
        assert_eq!(got.pick.member, "b-2");
        assert_eq!(got.detail.domain, Some(domain("b", "d")));
        // LongestReady ignores domains.
        let got = rank_pick(
            &one_pool(&views),
            &inputs(SelectionStrategy::LongestReady, &placements, &held),
        );
        assert_eq!(picked(got), Some("a-1".to_string()));
    }

    /// Only Ready members count as available: Claimed and Provisioning
    /// members in a domain do not make it look full.
    #[test]
    fn most_available_counts_ready_members_only() {
        let views = [
            member("a-1", MemberPhase::Ready, 10, CURRENT),
            member("a-2", MemberPhase::Ready, 10, CURRENT),
            member("b-1", MemberPhase::Ready, 10, CURRENT),
            member("b-2", MemberPhase::Claimed, 10, CURRENT),
            member("b-3", MemberPhase::Provisioning, 0, CURRENT),
        ];
        let placements: BTreeMap<String, MemberPlacement> = views
            .iter()
            .map(|v| {
                let host = &v.name[..1];
                (v.name.clone(), placed(domain(host, "d"), true))
            })
            .collect();
        let held = BTreeMap::new();
        let got = rank_pick(
            &one_pool(&views),
            &inputs(SelectionStrategy::MostAvailable, &placements, &held),
        );
        assert_eq!(picked(got), Some("a-1".to_string()));
    }

    /// Two hosts whose single failure domain carries the same name are two
    /// domains, not one (Decision 4).
    #[test]
    fn same_failure_domain_name_on_two_hosts_is_two_domains() {
        let views = [
            member("a-1", MemberPhase::Ready, 10, CURRENT),
            member("b-1", MemberPhase::Ready, 10, CURRENT),
            member("b-2", MemberPhase::Ready, 10, CURRENT),
        ];
        let placements = BTreeMap::from([
            ("a-1".to_string(), placed(domain("host-a", "default"), true)),
            ("b-1".to_string(), placed(domain("host-b", "default"), true)),
            ("b-2".to_string(), placed(domain("host-b", "default"), true)),
        ]);
        let held = BTreeMap::new();
        let got = rank_pick(
            &one_pool(&views),
            &inputs(SelectionStrategy::MostAvailable, &placements, &held),
        );
        assert_eq!(picked(got), Some("b-1".to_string()));
    }

    /// A member with no recorded placement is its own bucket, ranked after
    /// every member that has one, however many such members there are.
    #[test]
    fn an_unplaced_member_ranks_after_placed_ones() {
        let views = [
            member("u-1", MemberPhase::Ready, 3_600, CURRENT),
            member("u-2", MemberPhase::Ready, 3_600, CURRENT),
            member("p-1", MemberPhase::Ready, 10, CURRENT),
        ];
        let unplaced = MemberPlacement {
            domain: None,
            host_healthy: true,
        };
        let placements = BTreeMap::from([
            ("u-1".to_string(), unplaced.clone()),
            ("u-2".to_string(), unplaced),
            ("p-1".to_string(), placed(domain("a", "d"), true)),
        ]);
        let held = BTreeMap::new();
        for strategy in [SelectionStrategy::MostAvailable, SelectionStrategy::Spread] {
            let got = rank_pick(&one_pool(&views), &inputs(strategy, &placements, &held));
            assert_eq!(picked(got), Some("p-1".to_string()), "{strategy}");
        }
    }

    #[test]
    fn spread_avoids_a_domain_the_subject_already_holds() {
        let views = [
            member("a-1", MemberPhase::Ready, 10, CURRENT),
            member("a-2", MemberPhase::Ready, 10, CURRENT),
            member("b-1", MemberPhase::Ready, 10, CURRENT),
        ];
        let placements = BTreeMap::from([
            ("a-1".to_string(), placed(domain("a", "d"), true)),
            ("a-2".to_string(), placed(domain("a", "d"), true)),
            ("b-1".to_string(), placed(domain("b", "d"), true)),
        ]);
        let held = BTreeMap::from([(domain("a", "d"), 1)]);
        let got = rank_pick(
            &one_pool(&views),
            &inputs(SelectionStrategy::Spread, &placements, &held),
        )
        .unwrap();
        // MostAvailable alone would take from `a`; Spread takes `b`.
        assert_eq!(got.pick.member, "b-1");
        assert_eq!(got.detail.subject_held, 0);
    }

    /// A subject holding nothing yet: Spread falls through to
    /// MostAvailable's key.
    #[test]
    fn spread_with_nothing_held_behaves_as_most_available() {
        let views = [
            member("a-1", MemberPhase::Ready, 10, CURRENT),
            member("b-1", MemberPhase::Ready, 10, CURRENT),
            member("b-2", MemberPhase::Ready, 10, CURRENT),
        ];
        let placements = BTreeMap::from([
            ("a-1".to_string(), placed(domain("a", "d"), true)),
            ("b-1".to_string(), placed(domain("b", "d"), true)),
            ("b-2".to_string(), placed(domain("b", "d"), true)),
        ]);
        let held = BTreeMap::new();
        let got = rank_pick(
            &one_pool(&views),
            &inputs(SelectionStrategy::Spread, &placements, &held),
        );
        assert_eq!(picked(got), Some("b-1".to_string()));
    }

    /// Spread is soft: every domain occupied still binds, from the domain
    /// the subject holds least.
    #[test]
    fn spread_is_soft_when_every_domain_is_occupied() {
        let views = [
            member("a-1", MemberPhase::Ready, 10, CURRENT),
            member("b-1", MemberPhase::Ready, 10, CURRENT),
        ];
        let placements = BTreeMap::from([
            ("a-1".to_string(), placed(domain("a", "d"), true)),
            ("b-1".to_string(), placed(domain("b", "d"), true)),
        ]);
        let held = BTreeMap::from([(domain("a", "d"), 2), (domain("b", "d"), 1)]);
        let got = rank_pick(
            &one_pool(&views),
            &inputs(SelectionStrategy::Spread, &placements, &held),
        )
        .unwrap();
        assert_eq!(got.pick.member, "b-1");
        assert_eq!(got.detail.subject_held, 1);
    }

    /// Availability is counted over every matched pool at once, the same
    /// set the ranking runs over (ADR-0132 Decision 4).
    #[test]
    fn availability_is_counted_across_pools() {
        let a = [member("a-1", MemberPhase::Ready, 10, CURRENT)];
        let b = [
            member("b-1", MemberPhase::Ready, 10, CURRENT),
            member("z-1", MemberPhase::Ready, 10, CURRENT),
        ];
        // a-1 and z-1 share a domain, across two pools.
        let placements = BTreeMap::from([
            ("a-1".to_string(), placed(domain("x", "d"), true)),
            ("z-1".to_string(), placed(domain("x", "d"), true)),
            ("b-1".to_string(), placed(domain("y", "d"), true)),
        ]);
        let held = BTreeMap::new();
        let pools = [
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
        ];
        let got = rank_pick(
            &pools,
            &inputs(SelectionStrategy::MostAvailable, &placements, &held),
        )
        .unwrap();
        assert_eq!(
            got.pick,
            Pick {
                pool: "pool-a".into(),
                member: "a-1".into()
            }
        );
    }

    // --- the Bound condition message (Decision 6) ------------------------

    #[test]
    fn the_bound_message_names_strategy_and_domain() {
        let detail = PickDetail {
            domain: Some(domain("host-b", "default")),
            host_healthy: true,
            subject_held: 0,
        };
        assert_eq!(
            bound_message("m-1", SelectionStrategy::Spread, Some(&detail)),
            "bound to m-1 by Spread from host-b/default"
        );
    }

    #[test]
    fn the_bound_message_says_when_the_choice_was_a_fallback() {
        let detail = PickDetail {
            domain: Some(domain("host-a", "default")),
            host_healthy: false,
            subject_held: 2,
        };
        let msg = bound_message("m-1", SelectionStrategy::Spread, Some(&detail));
        assert!(
            msg.starts_with("bound to m-1 by Spread from host-a/default"),
            "{msg}"
        );
        assert!(msg.contains("cordoned or not reporting"), "{msg}");
        assert!(msg.contains("already holds 2"), "{msg}");
    }

    /// Finishing a bind made on an earlier pass knows nothing about why it
    /// was chosen, and says only what it knows.
    #[test]
    fn the_bound_message_without_detail_is_plain() {
        assert_eq!(
            bound_message("m-1", SelectionStrategy::MostAvailable, None),
            "bound to m-1"
        );
        let none = PickDetail {
            domain: None,
            host_healthy: true,
            subject_held: 0,
        };
        assert_eq!(
            bound_message("m-1", SelectionStrategy::LongestReady, Some(&none)),
            "bound to m-1 by LongestReady"
        );
    }

    /// Holding must not erase the explanation the bind wrote, or it would
    /// be gone on the very next reconcile.
    #[test]
    fn hold_keeps_the_binds_explanation_for_the_same_member() {
        let bound = "bound to m-1 by Spread from host-b/default";
        assert_eq!(hold_message(Some(bound), "m-1"), bound);
        assert_eq!(hold_message(None, "m-1"), "bound to m-1");
        assert_eq!(hold_message(Some("waiting"), "m-1"), "bound to m-1");
        // A prefix of another member's name is not this member.
        assert_eq!(
            hold_message(Some("bound to m-10 by Spread"), "m-1"),
            "bound to m-1"
        );
    }

    /// A pass that finishes an earlier pass's bind (the member already
    /// labelled ours, ADR-0132 Decision 5) re-reads the claim before it
    /// writes: when that bind is already published, nothing is written, so
    /// the message naming the strategy and the domain survives (ADR-0135
    /// Decision 6). Seen live 2026-11-19: three bind passes within 300 ms
    /// left a Spread claim saying only `bound to <member>`.
    #[test]
    fn finishing_a_published_bind_writes_nothing() {
        let published = VirtualMachineClaimStatus {
            phase: ClaimPhase::Bound,
            virtual_machine_ref: Some(LocalObjectReference { name: "m-1".into() }),
            ..VirtualMachineClaimStatus::default()
        };
        assert_eq!(
            finish_bind(Some(&published), "m-1"),
            FinishBind::AlreadyBound
        );
    }

    /// The earlier pass stopped between labelling the member and publishing
    /// the bind, the case the finish path exists for: publish it, with the
    /// plain message, since the pick's reasons are gone with that pass.
    #[test]
    fn finishing_an_unpublished_bind_publishes_it() {
        assert_eq!(finish_bind(None, "m-1"), FinishBind::Publish);
        let pending = VirtualMachineClaimStatus::default();
        assert_eq!(finish_bind(Some(&pending), "m-1"), FinishBind::Publish);
        // Another member in the status is not this bind, published or not.
        let other = VirtualMachineClaimStatus {
            phase: ClaimPhase::Bound,
            virtual_machine_ref: Some(LocalObjectReference {
                name: "m-10".into(),
            }),
            ..VirtualMachineClaimStatus::default()
        };
        assert_eq!(finish_bind(Some(&other), "m-1"), FinishBind::Publish);
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
