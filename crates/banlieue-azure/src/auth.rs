// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Where the ARM access token comes from (ADR-0159).
//!
//! [`FederatedToken`] takes an assertion from an
//! [`AssertionSource`] (by default the provider's projected ServiceAccount
//! token, audience `api://AzureADTokenExchange`, read from a file; see
//! [`crate::assertion`] for OpenBao as the signer, ADR-0176), exchanges it at
//! Entra ID for an ARM token, and caches the result until it is within
//! [`REFRESH_MARGIN`](crate::token::REFRESH_MARGIN) of expiry. The file is
//! read again on every exchange because the kubelet rotates it. No secret is
//! stored anywhere: the cluster holds only the ServiceAccount.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::assertion::{AssertionSource, FileAssertion};
use crate::error::{Error, Result};
use crate::operation::is_trusted_base;
use crate::token::{
    ARM_SCOPE, AccessToken, DEFAULT_AUTHORITY, exchange_form, parse_token_error,
    parse_token_response, token_endpoint,
};

/// Tenant, as the workload identity webhook names it.
pub const ENV_TENANT_ID: &str = "AZURE_TENANT_ID";
/// The managed identity's or app registration's client ID.
pub const ENV_CLIENT_ID: &str = "AZURE_CLIENT_ID";
/// Path of the projected ServiceAccount token.
pub const ENV_FEDERATED_TOKEN_FILE: &str = "AZURE_FEDERATED_TOKEN_FILE";
/// Optional: a sovereign cloud's authority.
pub const ENV_AUTHORITY_HOST: &str = "AZURE_AUTHORITY_HOST";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Entra ID's answers are a few KiB; anything far larger is not one.
const MAX_TOKEN_RESPONSE_BYTES: usize = 64 * 1024;
const CODE_CONFIG: &str = "InvalidConfiguration";

/// Supplies the bearer token for ARM calls.
#[async_trait]
pub trait TokenProvider: Send + Sync {
    /// A token valid for at least the refresh margin.
    ///
    /// # Errors
    /// [`Error::TokenExchange`] when no token can be had.
    async fn token(&self) -> Result<AccessToken>;

    /// Drops any cached token, after ARM refused it.
    async fn invalidate(&self) {}
}

/// What the federated exchange needs.
#[derive(Clone, Debug)]
pub struct FederatedConfig {
    /// Entra ID authority, such as `https://login.microsoftonline.com`.
    pub authority: String,
    /// Tenant ID.
    pub tenant_id: String,
    /// Client ID of the identity holding the federated credential.
    pub client_id: String,
    /// The projected ServiceAccount token.
    pub token_file: PathBuf,
    /// Scope asked for; [`ARM_SCOPE`] for Resource Manager.
    pub scope: String,
}

impl FederatedConfig {
    /// Reads the workload identity variables through `lookup` (normally
    /// `std::env::var(..).ok()`), scoped to ARM.
    ///
    /// # Errors
    /// [`Error::InvalidRequest`] naming the first variable that is unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let required = |name: &str| {
            lookup(name)
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| config_error(format!("{name} is not set")))
        };
        Ok(Self {
            authority: lookup(ENV_AUTHORITY_HOST)
                .filter(|v| !v.trim().is_empty())
                .map_or_else(
                    || DEFAULT_AUTHORITY.to_string(),
                    |v| v.trim_end_matches('/').to_string(),
                ),
            tenant_id: required(ENV_TENANT_ID)?,
            client_id: required(ENV_CLIENT_ID)?,
            token_file: PathBuf::from(required(ENV_FEDERATED_TOKEN_FILE)?),
            scope: ARM_SCOPE.to_string(),
        })
    }
}

fn config_error(message: String) -> Error {
    Error::InvalidRequest {
        code: CODE_CONFIG.to_string(),
        message,
    }
}

/// Exchanges the projected ServiceAccount token at Entra ID, with a cache.
pub struct FederatedToken {
    http: reqwest::Client,
    endpoint: String,
    config: FederatedConfig,
    assertions: Arc<dyn AssertionSource>,
    cached: Mutex<Option<AccessToken>>,
}

impl fmt::Debug for FederatedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FederatedToken")
            .field("endpoint", &self.endpoint)
            .field("client_id", &self.config.client_id)
            .field("token_file", &self.config.token_file)
            .finish_non_exhaustive()
    }
}

impl FederatedToken {
    /// Builds the source, reading the assertion from `config.token_file`.
    /// Makes no request.
    ///
    /// # Errors
    /// [`Error::InvalidRequest`] if the authority is not HTTPS (or loopback
    /// HTTP), or the tenant could change the URL's path.
    pub fn new(config: FederatedConfig) -> Result<Self> {
        let assertions = Arc::new(FileAssertion::new(config.token_file.clone()));
        Self::with_source(config, assertions)
    }

    /// Builds the source with another assertion source, such as
    /// [`OpenBaoAssertion`](crate::assertion::OpenBaoAssertion) (ADR-0176).
    /// `config.token_file` is not read. Makes no request.
    ///
    /// # Errors
    /// As [`FederatedToken::new`].
    pub fn with_source(
        config: FederatedConfig,
        assertions: Arc<dyn AssertionSource>,
    ) -> Result<Self> {
        let authority = config.authority.trim_end_matches('/');
        if !is_trusted_base(authority) {
            return Err(config_error(format!(
                "authority {authority:?} must be https://"
            )));
        }
        let endpoint = token_endpoint(authority, &config.tenant_id)?;
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(Self {
            http,
            endpoint,
            config,
            assertions,
            cached: Mutex::new(None),
        })
    }

    async fn exchange(&self) -> Result<AccessToken> {
        let assertion = self.assertions.assertion().await?;
        let response = self
            .http
            .post(&self.endpoint)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(exchange_form(
                &self.config.client_id,
                &assertion,
                &self.config.scope,
            ))
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let success = response.status().is_success();
        let body = crate::client::read_capped(response, MAX_TOKEN_RESPONSE_BYTES).await?;
        if !success {
            return Err(parse_token_error(&body));
        }
        parse_token_response(&body, Instant::now())
    }
}

#[async_trait]
impl TokenProvider for FederatedToken {
    async fn token(&self) -> Result<AccessToken> {
        // Held across the exchange so concurrent callers wait for one
        // exchange instead of each making their own.
        let mut cached = self.cached.lock().await;
        if let Some(token) = cached.as_ref()
            && !token.needs_refresh(Instant::now())
        {
            return Ok(token.clone());
        }
        let token = self.exchange().await?;
        *cached = Some(token.clone());
        Ok(token)
    }

    async fn invalidate(&self) {
        *self.cached.lock().await = None;
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod auth_tests;
