// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Where the signing credentials come from (ADR-0159, ADR-0174 Decision 6).
//!
//! [`WebIdentitySource`] is the production source: it reads the provider's
//! projected ServiceAccount token from a file, exchanges it at STS with
//! `AssumeRoleWithWebIdentity` for a [`SESSION_DURATION`] session, and
//! caches the session until it has under [`REFRESH_BEFORE_EXPIRY`] left (at
//! ten minutes of a fifteen-minute session). The file is read again on
//! every exchange because the kubelet rotates it. The token and the session
//! live in memory only and are never logged: every `Debug` here redacts
//! them, and no error message carries them.
//!
//! [`StaticSource`] holds explicit credentials: ADR-0159 Decision 4's
//! documented fallback, and what the live tier uses with an operator's own
//! session.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use jiff::{SignedDuration, Timestamp};
use tokio::sync::Mutex;

use crate::client::{check_endpoint, read_capped, validate_region};
use crate::error::{Error, Result};
use crate::query::FORM_CONTENT_TYPE;
use crate::sigv4::Credentials;
use crate::sts::{
    AssumeRoleWithWebIdentity, SESSION_DURATION, SessionCredentials, parse_assume_role_response,
    sts_endpoint, validate_role_session_name,
};
use crate::types::parse_error;

/// The region, as the AWS SDKs and the EKS pod identity webhook name it.
pub const ENV_REGION: &str = "AWS_REGION";
/// The role to assume.
pub const ENV_ROLE_ARN: &str = "AWS_ROLE_ARN";
/// Path of the projected ServiceAccount token.
pub const ENV_WEB_IDENTITY_TOKEN_FILE: &str = "AWS_WEB_IDENTITY_TOKEN_FILE";
/// Optional: the role session name.
pub const ENV_ROLE_SESSION_NAME: &str = "AWS_ROLE_SESSION_NAME";
/// A static access key ID (fallback only, ADR-0159 Decision 4).
pub const ENV_ACCESS_KEY_ID: &str = "AWS_ACCESS_KEY_ID";
/// A static secret access key (fallback only).
pub const ENV_SECRET_ACCESS_KEY: &str = "AWS_SECRET_ACCESS_KEY";
/// The session token of static temporary credentials (fallback only).
pub const ENV_SESSION_TOKEN: &str = "AWS_SESSION_TOKEN";

/// Exchange again when the cached session has this little left: at ten
/// minutes of a fifteen-minute session (ADR-0174 Decision 6).
pub const REFRESH_BEFORE_EXPIRY: Duration = Duration::from_secs(300);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// STS's answers are a few KiB; anything far larger is not one.
const MAX_STS_RESPONSE_BYTES: usize = 64 * 1024;
const REDACTED: &str = "<redacted>";

/// Supplies the credentials every request is signed with.
#[async_trait]
pub trait CredentialSource: Send + Sync {
    /// Credentials valid for at least [`REFRESH_BEFORE_EXPIRY`] when they
    /// expire at all.
    ///
    /// # Errors
    /// Whatever stopped the source from having any: an unreadable token
    /// file, STS refusing the exchange, a transport failure.
    async fn credentials(&self) -> Result<Credentials>;
}

/// Whether a session expiring at `expiration` must be exchanged again at
/// `now`: when it has [`REFRESH_BEFORE_EXPIRY`] or less left, or is past.
#[must_use]
pub fn needs_refresh(expiration: Timestamp, now: Timestamp) -> bool {
    let margin = SignedDuration::from_secs(
        i64::try_from(REFRESH_BEFORE_EXPIRY.as_secs()).unwrap_or(i64::MAX),
    );
    expiration.duration_since(now) <= margin
}

/// What the web identity exchange needs.
#[derive(Clone)]
pub struct WebIdentityConfig {
    /// The region; STS is called at its regional endpoint.
    pub region: String,
    /// The role to assume.
    pub role_arn: String,
    /// The role session name ([`crate::sts::role_session_name`]).
    pub role_session_name: String,
    /// The projected ServiceAccount token, audience `sts.amazonaws.com`.
    pub token_file: PathBuf,
    /// The session length asked for; [`SESSION_DURATION`] by default and
    /// never shorter.
    pub session_duration: Duration,
    /// Tests only: an STS endpoint other than the region's. Must be
    /// `https://`, or `http://` on loopback.
    pub sts_endpoint: Option<String>,
}

impl fmt::Debug for WebIdentityConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebIdentityConfig")
            .field("region", &self.region)
            .field("role_session_name", &self.role_session_name)
            .field("token_file", &self.token_file)
            .field("session_duration", &self.session_duration)
            .finish_non_exhaustive()
    }
}

impl WebIdentityConfig {
    /// A configuration with [`SESSION_DURATION`] and the region's STS.
    #[must_use]
    pub fn new(
        region: &str,
        role_arn: &str,
        role_session_name: &str,
        token_file: impl Into<PathBuf>,
    ) -> Self {
        Self {
            region: region.to_string(),
            role_arn: role_arn.to_string(),
            role_session_name: role_session_name.to_string(),
            token_file: token_file.into(),
            session_duration: SESSION_DURATION,
            sts_endpoint: None,
        }
    }

    /// Reads [`ENV_REGION`], [`ENV_ROLE_ARN`], [`ENV_WEB_IDENTITY_TOKEN_FILE`]
    /// and, if set, [`ENV_ROLE_SESSION_NAME`] through `lookup` (normally
    /// `std::env::var(..).ok()`); `default_session_name` otherwise.
    ///
    /// # Errors
    /// [`Error::Invalid`] naming the first required variable that is unset.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
        default_session_name: &str,
    ) -> Result<Self> {
        let get = |name: &str| lookup(name).filter(|v| !v.trim().is_empty());
        let required =
            |name: &str| get(name).ok_or_else(|| Error::Invalid(format!("{name} is not set")));
        let session =
            get(ENV_ROLE_SESSION_NAME).unwrap_or_else(|| default_session_name.to_string());
        Ok(Self::new(
            &required(ENV_REGION)?,
            &required(ENV_ROLE_ARN)?,
            &session,
            required(ENV_WEB_IDENTITY_TOKEN_FILE)?,
        ))
    }
}

/// Exchanges the projected ServiceAccount token at STS, with a cache.
pub struct WebIdentitySource {
    http: reqwest::Client,
    endpoint: String,
    config: WebIdentityConfig,
    cached: Mutex<Option<SessionCredentials>>,
}

impl fmt::Debug for WebIdentitySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebIdentitySource")
            .field("endpoint", &self.endpoint)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl WebIdentitySource {
    /// Builds the source. Makes no request and reads no file.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a region that is not one, a session name STS
    /// would refuse, a session shorter than [`SESSION_DURATION`], or an STS
    /// endpoint that is neither `https://` nor loopback `http://`;
    /// [`Error::Transport`] if the HTTP client cannot be built.
    pub fn new(config: WebIdentityConfig) -> Result<Self> {
        validate_region(&config.region)?;
        validate_role_session_name(&config.role_session_name)?;
        if config.session_duration < SESSION_DURATION {
            return Err(Error::Invalid(format!(
                "session duration {}s is under STS's floor of {}s (ADR-0174 Decision 6)",
                config.session_duration.as_secs(),
                SESSION_DURATION.as_secs()
            )));
        }
        let endpoint = match &config.sts_endpoint {
            Some(url) => check_endpoint(url)?.to_string(),
            None => sts_endpoint(&config.region)?,
        };
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            // Never follow a redirect: the token would go with it.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        Ok(Self {
            http,
            endpoint,
            config,
            cached: Mutex::new(None),
        })
    }

    /// The STS endpoint in use.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    async fn read_token(&self) -> Result<String> {
        let path = self.config.token_file.display();
        let text = tokio::fs::read_to_string(&self.config.token_file)
            .await
            .map_err(|e| Error::Invalid(format!("web identity token file {path}: {}", e.kind())))?;
        let token = text.trim();
        if token.is_empty() {
            return Err(Error::Invalid(format!(
                "web identity token file {path} is empty"
            )));
        }
        Ok(token.to_string())
    }

    async fn exchange(&self) -> Result<SessionCredentials> {
        // Read on every exchange: the kubelet rotates the file.
        let token = self.read_token().await?;
        let form = AssumeRoleWithWebIdentity {
            role_arn: &self.config.role_arn,
            role_session_name: &self.config.role_session_name,
            web_identity_token: &token,
            duration: self.config.session_duration,
        }
        .form()?;
        // Unsigned: the ServiceAccount token is the credential.
        let response = self
            .http
            .post(&self.endpoint)
            .header(reqwest::header::CONTENT_TYPE, FORM_CONTENT_TYPE)
            .body(form)
            .send()
            .await
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        let status = response.status();
        let body = read_capped(response, MAX_STS_RESPONSE_BYTES).await?;
        let text = String::from_utf8_lossy(&body);
        if !status.is_success() {
            return Err(parse_error(status.as_u16(), &text));
        }
        parse_assume_role_response(&text)
    }
}

#[async_trait]
impl CredentialSource for WebIdentitySource {
    async fn credentials(&self) -> Result<Credentials> {
        // Held across the exchange so concurrent callers wait for one
        // exchange instead of each making their own.
        let mut cached = self.cached.lock().await;
        if let Some(session) = cached.as_ref()
            && !needs_refresh(session.expiration, Timestamp::now())
        {
            return Ok(session.credentials.clone());
        }
        let session = self.exchange().await?;
        let credentials = session.credentials.clone();
        *cached = Some(session);
        Ok(credentials)
    }
}

/// Explicit credentials, never refreshed: ADR-0159 Decision 4's documented
/// fallback, and the live tier's operator session. `Debug` shows nothing
/// of them.
#[derive(Clone)]
pub struct StaticSource {
    credentials: Credentials,
}

impl fmt::Debug for StaticSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticSource")
            .field("credentials", &REDACTED)
            .finish()
    }
}

impl StaticSource {
    /// A source that always returns `credentials`.
    #[must_use]
    pub fn new(credentials: Credentials) -> Self {
        Self { credentials }
    }

    /// Reads [`ENV_ACCESS_KEY_ID`], [`ENV_SECRET_ACCESS_KEY`] and, if set,
    /// [`ENV_SESSION_TOKEN`] through `lookup`.
    ///
    /// # Errors
    /// [`Error::Invalid`] naming the first required variable that is unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |name: &str| lookup(name).filter(|v| !v.trim().is_empty());
        let required =
            |name: &str| get(name).ok_or_else(|| Error::Invalid(format!("{name} is not set")));
        Ok(Self::new(Credentials::new(
            required(ENV_ACCESS_KEY_ID)?,
            required(ENV_SECRET_ACCESS_KEY)?,
            get(ENV_SESSION_TOKEN),
        )))
    }
}

#[async_trait]
impl CredentialSource for StaticSource {
    async fn credentials(&self) -> Result<Credentials> {
        Ok(self.credentials.clone())
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod auth_tests;
