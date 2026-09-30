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
    fn a_username_without_a_realm_is_rejected() {
        assert!(ApiToken::new("banlieue!provider", "s").is_err());
    }

    #[test]
    fn empty_parts_are_rejected() {
        for bad in [
            "@pve!provider",
            "banlieue@!provider",
            "banlieue@pve!",
            "",
            "!",
        ] {
            assert!(
                ApiToken::new(bad, "s").is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn an_empty_secret_is_rejected() {
        assert!(ApiToken::new("banlieue@pve!provider", "  ").is_err());
    }

    #[test]
    fn whitespace_inside_a_secret_is_rejected() {
        assert!(ApiToken::new("banlieue@pve!provider", "abc def").is_err());
    }

    #[test]
    fn a_second_bang_is_rejected() {
        assert!(ApiToken::new("banlieue@pve!a!b", "s").is_err());
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
