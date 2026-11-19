// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `token.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::Error;

    #[test]
    fn a_well_formed_token_builds_the_authorization_header() {
        let t = ApiToken::new("banlieue@pve!provider", "abc-123").unwrap();
        assert_eq!(
            t.authorization(),
            "PVEAPIToken=banlieue@pve!provider=abc-123"
        );
    }

    #[test]
    fn surrounding_whitespace_from_a_secret_is_trimmed() {
        let t = ApiToken::new("  banlieue@pve!provider\n", "abc-123\n").unwrap();
        assert_eq!(
            t.authorization(),
            "PVEAPIToken=banlieue@pve!provider=abc-123"
        );
    }

    #[test]
    fn a_username_without_a_token_id_is_rejected() {
        let e = ApiToken::new("banlieue@pve", "s").unwrap_err();
        assert!(matches!(e, Error::InvalidToken(_)), "{e}");
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        for (case, username, secret) in [
            ("no realm", "banlieue!provider", "s"),
            ("empty user", "@pve!provider", "s"),
            ("empty realm", "banlieue@!provider", "s"),
            ("empty token id", "banlieue@pve!", "s"),
            ("empty username", "", "s"),
            ("only a bang", "!", "s"),
            ("blank secret", "banlieue@pve!provider", "  "),
            (
                "whitespace inside the secret",
                "banlieue@pve!provider",
                "abc def",
            ),
            ("second bang", "banlieue@pve!a!b", "s"),
        ] {
            assert!(
                ApiToken::new(username, secret).is_err(),
                "{case}: {username:?} / {secret:?} should be rejected"
            );
        }
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let t = ApiToken::new("banlieue@pve!provider", "hunter2-secret").unwrap();
        let shown = format!("{t:?}");
        assert!(!shown.contains("hunter2-secret"), "{shown}");
        assert!(shown.contains("banlieue@pve!provider"), "{shown}");
    }

    #[test]
    fn token_id_is_exposed_for_logging() {
        let t = ApiToken::new("banlieue@pve!provider", "s").unwrap();
        assert_eq!(t.id(), "banlieue@pve!provider");
    }
}
