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
        let status = bound_status("sandbox-pool-x7k2p", at(1_800_000_000), at(1_800_003_600));
        assert_eq!(status["virtualMachineRef"]["name"], "sandbox-pool-x7k2p");
        assert!(status.get("boundAt").is_some(), "{status}");
        assert!(status.get("expiresAt").is_some(), "{status}");
    }

    /// ADR-0049, amended 2026-11-05: the attestation is SPIRE's TPM node
    /// attestation, which carries its own challenge, so a bind mints no
    /// nonce. A nonce nobody verifies would read as a replay guarantee that
    /// does not exist.
    #[test]
    fn a_bind_mints_no_nonce() {
        let status = bound_status("m", at(1_800_000_000), at(1_800_003_600));
        assert!(status.get("nonce").is_none(), "{status}");
    }
}
