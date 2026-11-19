// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The name alphabet shared by the CA common name and the subject.

use thiserror::Error;

/// Longest accepted name. RFC 5280's upper bound for a common name.
pub const MAX_NAME_LEN: usize = 64;

/// Why a name was refused. Refused, never replaced.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum NameError {
    /// The name is empty.
    #[error("name is empty")]
    Empty,
    /// The name is longer than [`MAX_NAME_LEN`].
    #[error("name is longer than {MAX_NAME_LEN} characters")]
    TooLong,
    /// The name contains a character outside letters, digits, dots, dashes, underscores.
    #[error("name contains a character outside letters, digits, dots, dashes and underscores")]
    BadCharacter,
}

/// Accept a name made only of ASCII letters, digits, dots, dashes and underscores.
///
/// # Errors
/// [`NameError`] when the name is empty, too long or outside the alphabet.
pub fn validate_name(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.len() > MAX_NAME_LEN {
        return Err(NameError::TooLong);
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_');
    if !name.chars().all(allowed) {
        return Err(NameError::BadCharacter);
    }
    Ok(())
}

#[cfg(test)]
#[path = "name_tests.rs"]
mod name_tests;
