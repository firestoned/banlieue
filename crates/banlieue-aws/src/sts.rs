// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! STS `AssumeRoleWithWebIdentity`, as pure pieces (ADR-0159, ADR-0174
//! Decision 6).
//!
//! The provider's projected ServiceAccount token (audience
//! `sts.amazonaws.com`) is posted to the regional STS endpoint, unsigned:
//! the token is the credential. STS answers with a role session (an access
//! key, its secret and a session token) that lives
//! [`SESSION_DURATION`], STS's floor, so a copied session is good for at
//! most fifteen minutes. [`crate::auth::WebIdentitySource`] makes the call
//! and caches the session; this module builds the request body and reads
//! the answer.
//!
//! STS speaks the Query protocol, as EC2 does, with its own pinned
//! [`STS_API_VERSION`].

use std::fmt;
use std::time::Duration;

use jiff::Timestamp;
use serde::Deserialize;

use crate::client::validate_region;
use crate::error::{Error, Result};
use crate::query::QueryParams;
use crate::sigv4::Credentials;

/// STS's Query API version, pinned.
pub const STS_API_VERSION: &str = "2011-06-15";
/// `AssumeRoleWithWebIdentity`.
pub const ACTION_ASSUME_ROLE_WITH_WEB_IDENTITY: &str = "AssumeRoleWithWebIdentity";
/// The session length asked for: STS's floor, and the default (ADR-0174
/// Decision 6). Nothing shorter is accepted.
pub const SESSION_DURATION: Duration = Duration::from_secs(900);
/// STS's ceiling on `DurationSeconds` (a role's own maximum may be lower).
pub const MAX_SESSION_DURATION: Duration = Duration::from_secs(43_200);
/// STS's shortest `RoleSessionName`.
pub const MIN_ROLE_SESSION_NAME_LEN: usize = 2;
/// STS's longest `RoleSessionName`.
pub const MAX_ROLE_SESSION_NAME_LEN: usize = 64;

/// STS's shortest `WebIdentityToken`.
const MIN_WEB_IDENTITY_TOKEN_LEN: usize = 4;
/// STS's longest `WebIdentityToken`.
const MAX_WEB_IDENTITY_TOKEN_LEN: usize = 20_000;
/// STS's shortest `RoleArn`.
const MIN_ROLE_ARN_LEN: usize = 20;
/// STS's longest `RoleArn`.
const MAX_ROLE_ARN_LEN: usize = 2048;
/// What `[\w+=,.@-]` allows besides ASCII letters and digits.
const ROLE_SESSION_NAME_SYMBOLS: [char; 7] = ['_', '+', '=', ',', '.', '@', '-'];
/// Joins a `Provider`'s namespace and name in its session name, as the
/// ownership tags join them.
const SESSION_NAME_SEPARATOR: char = '.';

const P_ROLE_ARN: &str = "RoleArn";
const P_ROLE_SESSION_NAME: &str = "RoleSessionName";
const P_WEB_IDENTITY_TOKEN: &str = "WebIdentityToken";
const P_DURATION_SECONDS: &str = "DurationSeconds";
const REDACTED: &str = "<redacted>";

/// The regional STS endpoint, `https://sts.<region>.amazonaws.com/`.
///
/// # Errors
/// [`Error::Invalid`] when `region` is not an AWS region name.
pub fn sts_endpoint(region: &str) -> Result<String> {
    validate_region(region)?;
    Ok(format!("https://sts.{region}.amazonaws.com/"))
}

/// Checks a `RoleSessionName` against STS's rules: 2 to 64 characters of
/// `[\w+=,.@-]` (ASCII letters, digits, `_`, `+`, `=`, `,`, `.`, `@`, `-`).
///
/// # Errors
/// [`Error::Invalid`] naming the rule broken.
pub fn validate_role_session_name(name: &str) -> Result<()> {
    let len = name.chars().count();
    if !(MIN_ROLE_SESSION_NAME_LEN..=MAX_ROLE_SESSION_NAME_LEN).contains(&len) {
        return Err(Error::Invalid(format!(
            "role session name {name:?} is {len} characters; STS takes \
             {MIN_ROLE_SESSION_NAME_LEN} to {MAX_ROLE_SESSION_NAME_LEN}"
        )));
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || ROLE_SESSION_NAME_SYMBOLS.contains(&c);
    if !name.chars().all(allowed) {
        return Err(Error::Invalid(format!(
            "role session name {name:?} holds a character outside [\\w+=,.@-]"
        )));
    }
    Ok(())
}

/// The session name for the `Provider` `provider_namespace/provider_name`:
/// `<namespace>.<name>`, so CloudTrail names the `Provider` that acted.
///
/// # Errors
/// [`Error::Invalid`] when the result breaks STS's rules
/// ([`validate_role_session_name`]), for example a name over 64
/// characters.
pub fn role_session_name(provider_namespace: &str, provider_name: &str) -> Result<String> {
    let name = format!("{provider_namespace}{SESSION_NAME_SEPARATOR}{provider_name}");
    validate_role_session_name(&name)?;
    Ok(name)
}

/// One `AssumeRoleWithWebIdentity` call. `Debug` redacts the token.
#[derive(Clone, Copy)]
pub struct AssumeRoleWithWebIdentity<'a> {
    /// The role to assume.
    pub role_arn: &'a str,
    /// The session name ([`role_session_name`]).
    pub role_session_name: &'a str,
    /// The projected ServiceAccount token. Never logged.
    pub web_identity_token: &'a str,
    /// The session length; at least [`SESSION_DURATION`].
    pub duration: Duration,
}

impl fmt::Debug for AssumeRoleWithWebIdentity<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AssumeRoleWithWebIdentity")
            .field("role_session_name", &self.role_session_name)
            .field("web_identity_token", &REDACTED)
            .field("duration", &self.duration)
            .finish_non_exhaustive()
    }
}

impl AssumeRoleWithWebIdentity<'_> {
    /// Refuses a call STS or ADR-0174 would refuse.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a duration under [`SESSION_DURATION`] or over
    /// [`MAX_SESSION_DURATION`], a role ARN or token outside STS's length
    /// limits, or a session name [`validate_role_session_name`] refuses.
    /// No message carries the token.
    pub fn validate(&self) -> Result<()> {
        if self.duration < SESSION_DURATION {
            return Err(Error::Invalid(format!(
                "session duration {}s is under STS's floor of {}s (ADR-0174 Decision 6)",
                self.duration.as_secs(),
                SESSION_DURATION.as_secs()
            )));
        }
        if self.duration > MAX_SESSION_DURATION {
            return Err(Error::Invalid(format!(
                "session duration {}s is over STS's ceiling of {}s",
                self.duration.as_secs(),
                MAX_SESSION_DURATION.as_secs()
            )));
        }
        let arn_len = self.role_arn.chars().count();
        if !(MIN_ROLE_ARN_LEN..=MAX_ROLE_ARN_LEN).contains(&arn_len) {
            return Err(Error::Invalid(format!(
                "role ARN is {arn_len} characters; STS takes {MIN_ROLE_ARN_LEN} to {MAX_ROLE_ARN_LEN}"
            )));
        }
        let token_len = self.web_identity_token.len();
        if !(MIN_WEB_IDENTITY_TOKEN_LEN..=MAX_WEB_IDENTITY_TOKEN_LEN).contains(&token_len) {
            return Err(Error::Invalid(format!(
                "web identity token is {token_len} bytes; STS takes \
                 {MIN_WEB_IDENTITY_TOKEN_LEN} to {MAX_WEB_IDENTITY_TOKEN_LEN}"
            )));
        }
        validate_role_session_name(self.role_session_name)
    }

    /// The form body: `Action`, `Version`, `RoleArn`, `RoleSessionName`,
    /// `WebIdentityToken` and `DurationSeconds`, encoded as every Query
    /// body is ([`QueryParams::encode`]).
    ///
    /// # Errors
    /// As [`Self::validate`].
    pub fn form(&self) -> Result<String> {
        self.validate()?;
        let mut params =
            QueryParams::with_version(ACTION_ASSUME_ROLE_WITH_WEB_IDENTITY, STS_API_VERSION);
        params.extend([
            (P_ROLE_ARN.to_string(), self.role_arn.to_string()),
            (
                P_ROLE_SESSION_NAME.to_string(),
                self.role_session_name.to_string(),
            ),
            (
                P_WEB_IDENTITY_TOKEN.to_string(),
                self.web_identity_token.to_string(),
            ),
            (
                P_DURATION_SECONDS.to_string(),
                self.duration.as_secs().to_string(),
            ),
        ])?;
        Ok(params.encode())
    }
}

/// A role session: the credentials to sign with, and when they expire.
#[derive(Clone, Debug)]
pub struct SessionCredentials {
    /// Access key, secret and session token; `Debug` redacts the last two.
    pub credentials: Credentials,
    /// When STS stops honouring them.
    pub expiration: Timestamp,
}

#[derive(Deserialize, Default)]
struct RawCredentials {
    #[serde(rename = "AccessKeyId", default)]
    access_key_id: Option<String>,
    #[serde(rename = "SecretAccessKey", default)]
    secret_access_key: Option<String>,
    #[serde(rename = "SessionToken", default)]
    session_token: Option<String>,
    #[serde(rename = "Expiration", default)]
    expiration: Option<String>,
}

#[derive(Deserialize)]
struct RawResult {
    #[serde(rename = "Credentials", default)]
    credentials: Option<RawCredentials>,
}

/// `<AssumeRoleWithWebIdentityResponse>`; the root's name is not read.
#[derive(Deserialize)]
struct RawResponse {
    #[serde(rename = "AssumeRoleWithWebIdentityResult", default)]
    result: Option<RawResult>,
}

/// Reads the session from a successful `AssumeRoleWithWebIdentity`
/// response (`AssumeRoleWithWebIdentityResult/Credentials`).
///
/// # Errors
/// [`Error::Decode`] for malformed XML, a missing or empty `AccessKeyId`,
/// `SecretAccessKey`, `SessionToken` or `Expiration`, or an `Expiration`
/// that is not RFC 3339. No message carries the secret or the token.
pub fn parse_assume_role_response(body: &str) -> Result<SessionCredentials> {
    // The parser's own message may quote the document: never pass it on.
    let raw: RawResponse = quick_xml::de::from_str(body).map_err(|_| {
        Error::Decode("AssumeRoleWithWebIdentity: the response is not well-formed".to_string())
    })?;
    let credentials = raw
        .result
        .and_then(|r| r.credentials)
        .ok_or_else(|| missing("Credentials"))?;
    let access_key_id = required(credentials.access_key_id, "AccessKeyId")?;
    let secret_access_key = required(credentials.secret_access_key, "SecretAccessKey")?;
    let session_token = required(credentials.session_token, "SessionToken")?;
    let expiration_text = required(credentials.expiration, "Expiration")?;
    let expiration = expiration_text.parse::<Timestamp>().map_err(|e| {
        Error::Decode(format!(
            "AssumeRoleWithWebIdentity: Expiration {expiration_text:?}: {e}"
        ))
    })?;
    Ok(SessionCredentials {
        credentials: Credentials::new(access_key_id, secret_access_key, Some(session_token)),
        expiration,
    })
}

fn required(value: Option<String>, element: &str) -> Result<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| missing(element))
}

fn missing(element: &str) -> Error {
    Error::Decode(format!(
        "AssumeRoleWithWebIdentity: no {element} in the response"
    ))
}

#[cfg(test)]
#[path = "sts_tests.rs"]
mod sts_tests;
