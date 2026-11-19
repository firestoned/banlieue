// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `name.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn accepts_the_allowed_alphabet() {
        for ok in ["a", "bar.foo.io", "Host_01-x", "0f8fad5b-d9cb", "A.B_c-9"] {
            assert_eq!(validate_name(ok), Ok(ok), "{ok}");
        }
    }

    #[test]
    fn refuses_empty() {
        assert_eq!(validate_name(""), Err(NameError::Empty));
    }

    #[test]
    fn length_limit_is_inclusive() {
        let at = "a".repeat(MAX_NAME_LEN);
        assert!(validate_name(&at).is_ok());
        let over = "a".repeat(MAX_NAME_LEN + 1);
        assert_eq!(
            validate_name(&over),
            Err(NameError::TooLong {
                len: MAX_NAME_LEN + 1,
                max: MAX_NAME_LEN
            })
        );
    }

    #[test]
    fn refuses_every_other_character_never_replacing_it() {
        for bad in [
            "a b",
            "a/b",
            "a:b",
            "a@b",
            "a$b",
            "a\nb",
            "a\0b",
            "caf\u{e9}",
            "a*b",
            "a\\b",
            " ",
            "a,b",
        ] {
            assert_eq!(validate_name(bad), Err(NameError::InvalidChar), "{bad:?}");
        }
    }

    #[test]
    fn refuses_dot_segments() {
        assert_eq!(validate_name("."), Err(NameError::DotSegment));
        assert_eq!(validate_name(".."), Err(NameError::DotSegment));
        assert!(validate_name("...").is_ok());
        assert!(validate_name(".a").is_ok());
    }
}
