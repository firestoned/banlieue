// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Name validation for the CA name and the certificate subject.

use thiserror::Error;

/// The longest name accepted, in bytes. Well inside the 64 byte limit RFC 5280
/// puts on a common name, so a valid name is always a valid CN.
pub const MAX_NAME_LEN: usize = 63;

/// Why a name was refused. A name is refused, never repaired.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum NameError {
    /// The name is empty.
    #[error("name is empty")]
    Empty,
    /// The name is longer than [`MAX_NAME_LEN`].
    #[error("name is {len} bytes, the limit is {max}")]
    TooLong {
        /// Length given.
        len: usize,
        /// Limit.
        max: usize,
    },
    /// The name has a character outside letters, digits, dot, dash, underscore.
    #[error("name has a character outside letters, digits, '.', '-' and '_'")]
    InvalidChar,
    /// The name is `.` or `..`, which a SPIFFE path segment may not be.
    #[error("name is a dot segment")]
    DotSegment,
}

/// One allowed character: ASCII letter, digit, dot, dash or underscore.
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')
}

/// Checks `name` against the agent-ID path-segment alphabet.
///
/// # Errors
/// [`NameError`] naming the first rule broken.
pub fn validate_name(name: &str) -> Result<&str, NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.len() > MAX_NAME_LEN {
        return Err(NameError::TooLong {
            len: name.len(),
            max: MAX_NAME_LEN,
        });
    }
    if !name.chars().all(is_name_char) {
        return Err(NameError::InvalidChar);
    }
    if name == "." || name == ".." {
        return Err(NameError::DotSegment);
    }
    Ok(name)
}

#[cfg(test)]
#[path = "name_tests.rs"]
mod name_tests;
