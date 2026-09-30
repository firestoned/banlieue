// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! API-token credentials (ADR-0074 Decision 3): the only auth scheme.

use crate::error::{Error, Result};

/// Separates the user and realm in `user@realm!tokenid`.
const REALM_SEPARATOR: char = '@';
/// Separates the user part from the token id.
const TOKEN_SEPARATOR: char = '!';

/// A validated `PVEAPIToken` credential.
///
/// Built from the Provider credentials Secret's `username` (the full
/// `user@realm!tokenid`) and `tokenValue`. Validated before any request,
/// because a mis-pasted token otherwise surfaces as a bare 401.
#[derive(Clone)]
pub struct ApiToken {
    id: String,
    header: String,
}

impl ApiToken {
    /// Validate and build a token.
    ///
    /// Surrounding whitespace is trimmed (a Secret often ends in a newline).
    ///
    /// # Arguments
    /// * `token_id` - `user@realm!tokenid`
    /// * `secret` - the token's UUID secret
    ///
    /// # Errors
    /// [`Error::InvalidToken`] if the id is not exactly `user@realm!id` with
    /// every part non-empty, or the secret is empty or contains whitespace.
    pub fn new(token_id: &str, secret: &str) -> Result<Self> {
        let id = token_id.trim();
        let secret = secret.trim();
        let invalid = |why: &str| Err(Error::InvalidToken(why.to_string()));

        let Some((user_realm, name)) = id.split_once(TOKEN_SEPARATOR) else {
            return invalid("username must be user@realm!tokenid (missing '!')");
        };
        if name.is_empty() || name.contains(TOKEN_SEPARATOR) {
            return invalid("token id must be one non-empty name after '!'");
        }
        let Some((user, realm)) = user_realm.split_once(REALM_SEPARATOR) else {
            return invalid("username must be user@realm!tokenid (missing '@')");
        };
        if user.is_empty() || realm.is_empty() {
            return invalid("user and realm must be non-empty");
        }
        if secret.is_empty() {
            return invalid("token secret is empty");
        }
        if secret.chars().any(char::is_whitespace) {
            return invalid("token secret contains whitespace");
        }
        Ok(Self {
            id: id.to_string(),
            header: format!("PVEAPIToken={id}={secret}"),
        })
    }

    /// The full `user@realm!tokenid`, safe to log.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The `Authorization` header value. Contains the secret.
    #[must_use]
    pub fn authorization(&self) -> &str {
        &self.header
    }
}

impl std::fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiToken")
            .field("id", &self.id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
#[path = "token_tests.rs"]
mod token_tests;
