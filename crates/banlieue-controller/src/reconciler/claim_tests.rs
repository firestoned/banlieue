// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `claim.rs`.
//!
//! The reconcile path itself is exercised through `claim_plan.rs`, which
//! holds every decision as a pure function. What is left here is the status a
//! bind publishes.

#[cfg(test)]
mod tests {
    use super::super::*;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("valid timestamp")
    }

    /// A bind publishes the member, when it was bound and when it expires:
    /// what a consumer reads to find its sandbox and its deadline.
    #[test]
    fn a_bind_publishes_the_member_and_its_deadline() {
        let status = bound_status(
            "sandbox-pool-x7k2p",
            "sandbox-pool",
            at(1_800_000_000),
            at(1_800_003_600),
        );
        assert_eq!(status["virtualMachineRef"]["name"], "sandbox-pool-x7k2p");
        // ADR-0132 Decision 6: a selector claim's only record of its pool.
        assert_eq!(status["poolRef"]["name"], "sandbox-pool");
        assert!(status.get("boundAt").is_some(), "{status}");
        assert!(status.get("expiresAt").is_some(), "{status}");
    }

    /// ADR-0049, amended 2026-11-05: the attestation is SPIRE's TPM node
    /// attestation, which carries its own challenge, so a bind mints no
    /// nonce. A nonce nobody verifies would read as a replay guarantee that
    /// does not exist.
    #[test]
    fn a_bind_mints_no_nonce() {
        let status = bound_status("m", "p", at(1_800_000_000), at(1_800_003_600));
        assert!(status.get("nonce").is_none(), "{status}");
    }
    /// What `hold` writes to the claim. The subject is written when present
    /// and sent as `null` when absent: the status is applied as a merge
    /// patch, so `null` is what removes a subject the member has withdrawn,
    /// as an empty list does for the certificates.
    #[test]
    fn a_hold_writes_the_members_attestation_subject() {
        let state = MirroredMemberState {
            attestation_subject: Some("0f3c9a1e-0000-4000-8000-000000000001".into()),
            ..MirroredMemberState::default()
        };
        let body = mirrored_status_fields(&state);
        assert_eq!(
            body["attestationSubject"],
            "0f3c9a1e-0000-4000-8000-000000000001"
        );
        assert_eq!(body["addresses"], serde_json::json!([]));
        assert_eq!(body["tpmEndorsementCertificates"], serde_json::json!([]));
    }

    #[test]
    fn a_hold_clears_a_withdrawn_attestation_subject() {
        let body = mirrored_status_fields(&MirroredMemberState::default());
        assert!(body["attestationSubject"].is_null(), "{body}");
        assert!(body.as_object().unwrap().contains_key("attestationSubject"));
    }
}
