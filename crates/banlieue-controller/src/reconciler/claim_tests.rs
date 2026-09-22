// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `claim.rs`.
//!
//! The reconcile path itself is exercised through `claim_plan.rs`, which
//! holds every decision as a pure function. What is left here is the nonce
//! generator — small, but the one piece whose failure mode is silent: a
//! nonce that is short, non-random or repeated still looks like a nonce.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::CLAIM_NONCE_BITS;
    use std::collections::HashSet;

    /// 128 bits, hex-encoded, is 32 characters. A nonce that silently came
    /// out shorter would still serialize, still round-trip, and still look
    /// like a nonce to every reader.
    #[test]
    fn a_nonce_is_128_bits_of_hex() {
        let nonce = new_nonce();
        assert_eq!(nonce.len(), CLAIM_NONCE_BITS / 4);
        assert!(
            nonce.chars().all(|c| c.is_ascii_hexdigit()),
            "expected lowercase hex, got {nonce}"
        );
    }

    /// The property the nonce exists for. Deriving it from the claim UID or
    /// the clock would pass every other test in this file while making a
    /// replayed attestation quote accepted (ADR-0047 Decision 8).
    #[test]
    fn nonces_do_not_repeat() {
        const DRAWS: usize = 256;
        let seen: HashSet<String> = (0..DRAWS).map(|_| new_nonce()).collect();
        assert_eq!(seen.len(), DRAWS, "nonce generator repeated a value");
    }

    /// A zero nonce is what a broken or absent RNG produces, and it would
    /// be indistinguishable from a real one at a glance.
    #[test]
    fn a_nonce_is_not_all_zeroes() {
        assert!(new_nonce().chars().any(|c| c != '0'));
    }
}
