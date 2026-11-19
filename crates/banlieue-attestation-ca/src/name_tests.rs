// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn accepts_the_alphabet_and_a_uuid() {
    for ok in [
        "host-01",
        "a.b_c-D9",
        "0b6f9f0e-2c64-4c61-9f43-0a1b2c3d4e5f",
        "x",
    ] {
        assert_eq!(validate_name(ok), Ok(()), "{ok}");
    }
}

#[test]
fn refuses_empty() {
    assert_eq!(validate_name(""), Err(NameError::Empty));
}

#[test]
fn refuses_characters_outside_the_alphabet() {
    for bad in [
        "a b", "a,b", "a=b", "a/b", "a\"b", "a+b", "é", "a\nb", "a\0b", "a*b", "a;b",
    ] {
        assert_eq!(validate_name(bad), Err(NameError::BadCharacter), "{bad:?}");
    }
}

#[test]
fn length_boundary() {
    assert_eq!(validate_name(&"a".repeat(MAX_NAME_LEN)), Ok(()));
    assert_eq!(
        validate_name(&"a".repeat(MAX_NAME_LEN + 1)),
        Err(NameError::TooLong)
    );
}
