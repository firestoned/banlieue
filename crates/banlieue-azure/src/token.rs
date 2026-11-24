// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Workload identity federation (ADR-0159).
//!
//! The provider's projected ServiceAccount token (audience
//! [`TOKEN_EXCHANGE_AUDIENCE`]) is posted to Entra ID as a client assertion
//! in an OAuth 2.0 client-credentials grant; Entra ID answers with an access
//! token for the managed identity. Verified live: about 0.6 s per exchange;
//! another ServiceAccount (`AADSTS700213`) or audience (`AADSTS700212`) is
//! refused. The access token lived 24 hours, so it is held in memory only,
//! never logged, and refreshed [`REFRESH_MARGIN`] before it expires.

use std::fmt;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::error::{Error, Result, truncate};

/// Public-cloud Entra ID authority.
pub const DEFAULT_AUTHORITY: &str = "https://login.microsoftonline.com";
/// The audience a ServiceAccount token must carry for Entra ID to accept it.
pub const TOKEN_EXCHANGE_AUDIENCE: &str = "api://AzureADTokenExchange";
/// Scope for Azure Resource Manager.
pub const ARM_SCOPE: &str = "https://management.azure.com/.default";
/// Refresh this long before the access token expires.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(300);

const GRANT_TYPE: &str = "client_credentials";
const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";
const AADSTS_PREFIX: &str = "AADSTS";

/// The token endpoint for `tenant` under `authority`.
///
/// # Errors
/// [`Error::InvalidRequest`] if `tenant` is empty or holds anything but
/// letters, digits, `-` and `.`, so it cannot change the URL's path.
pub fn token_endpoint(authority: &str, tenant: &str) -> Result<String> {
    let valid = !tenant.is_empty()
        && !tenant.starts_with('.')
        && tenant
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && !tenant.contains("..");
    if !valid {
        return Err(Error::InvalidRequest {
            code: "InvalidTenant".to_string(),
            message: format!("tenant {tenant:?} is not a tenant ID or domain"),
        });
    }
    Ok(format!(
        "{}/{tenant}/oauth2/v2.0/token",
        authority.trim_end_matches('/')
    ))
}

/// The `application/x-www-form-urlencoded` body of the exchange.
#[must_use]
pub fn exchange_form(client_id: &str, assertion: &str, scope: &str) -> String {
    form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", GRANT_TYPE)
        .append_pair("client_id", client_id)
        .append_pair("scope", scope)
        .append_pair("client_assertion_type", CLIENT_ASSERTION_TYPE)
        .append_pair("client_assertion", assertion)
        .finish()
}

/// An access token. Its value is never printed.
#[derive(Clone)]
pub struct AccessToken {
    secret: String,
    expires_at: Instant,
}

impl AccessToken {
    /// The bearer value, for the `Authorization` header only.
    #[must_use]
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// When it expires.
    #[must_use]
    pub fn expires_at(&self) -> Instant {
        self.expires_at
    }

    /// Whether to exchange again: within [`REFRESH_MARGIN`] of expiry, or past it.
    #[must_use]
    pub fn needs_refresh(&self, now: Instant) -> bool {
        now + REFRESH_MARGIN >= self.expires_at
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessToken")
            .field("secret", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    expires_in: Option<u64>,
}

/// Parses a successful token response received at `now`.
///
/// # Errors
/// [`Error::Decode`] if the body is not JSON or lacks `access_token` or
/// `expires_in`.
pub fn parse_token_response(body: &[u8], now: Instant) -> Result<AccessToken> {
    let parsed: TokenResponse =
        serde_json::from_slice(body).map_err(|e| Error::Decode(format!("token response: {e}")))?;
    let secret = parsed
        .access_token
        .ok_or_else(|| Error::Decode("token response without access_token".to_string()))?;
    let expires_in = parsed
        .expires_in
        .ok_or_else(|| Error::Decode("token response without expires_in".to_string()))?;
    Ok(AccessToken {
        secret,
        expires_at: now + Duration::from_secs(expires_in),
    })
}

#[derive(Deserialize)]
struct TokenError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
    #[serde(default)]
    error_codes: Vec<u64>,
}

/// Turns a refused exchange into [`Error::TokenExchange`], keeping Entra
/// ID's `AADSTSnnnnnn` code when it gives one.
#[must_use]
pub fn parse_token_error(body: &[u8]) -> Error {
    let Ok(parsed) = serde_json::from_slice::<TokenError>(body) else {
        return Error::TokenExchange {
            code: String::new(),
            message: "unparseable response".to_string(),
        };
    };
    let code = parsed
        .error_codes
        .first()
        .map(|c| format!("{AADSTS_PREFIX}{c}"))
        .unwrap_or(parsed.error);
    Error::TokenExchange {
        code,
        message: truncate(&parsed.error_description),
    }
}

#[cfg(test)]
#[path = "token_tests.rs"]
mod token_tests;
