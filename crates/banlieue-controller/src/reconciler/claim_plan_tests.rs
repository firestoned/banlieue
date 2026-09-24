// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `claim_plan.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::reconciler::pool_plan::{MemberPhase, MemberView};
    use banlieue_api::banlieue::pool_condition_reasons;

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
            pool_name: "sandbox-pool",
            pool_exists: false,
            warm: None,
        });
        assert_eq!(reason, pool_condition_reasons::POOL_NOT_FOUND);
        assert!(message.contains("sandbox-pool"), "got: {message}");
    }

    /// A pool that is merely filling is the normal state after a burst.
    #[test]
    fn a_filling_pool_reports_no_member_available() {
        let (reason, message) = wait_reason(&PoolWaitState {
            pool_name: "sandbox-pool",
            pool_exists: true,
            warm: Some(("Filling", "1 of 2 members ready")),
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
            pool_name: "sandbox-pool",
            pool_exists: true,
            warm: Some((
                "ReadinessSignalAbsent",
                "no member has published the \"GuestReady\" condition",
            )),
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
            pool_name: "sandbox-pool",
            pool_exists: true,
            warm: None,
        });
        assert_eq!(reason, pool_condition_reasons::NO_MEMBER_AVAILABLE);
        assert!(
            !message.contains("does not exist"),
            "an existing pool must never be described as missing: {message}"
        );
        assert!(message.contains("sandbox-pool"), "got: {message}");
    }

    // ------------------------------------------------------------------
    // ADR-0045 — the claim mirrors its member's attestation anchor
    // ------------------------------------------------------------------

    /// Without this the claim carries a nonce a verifier cannot use: ADR-0049
    /// checks a quote against the EK certificate, and one GET of the claim is
    /// supposed to yield both.
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
