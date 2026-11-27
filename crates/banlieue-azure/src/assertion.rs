// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Where the federation assertion comes from (ADR-0159, ADR-0176).
//!
//! The token exchange at Entra ID needs one signed JWT, the *assertion*. By
//! default that is the provider's projected ServiceAccount token, read from a
//! file ([`FileAssertion`]). For a cluster whose ServiceAccount issuer cannot
//! be published, [`OpenBaoAssertion`] gets the assertion from OpenBao's
//! identity secrets engine instead: it logs in with the provider's own
//! ServiceAccount token, reads `identity/oidc/token/<role>`, and revokes the
//! login token it used. Nothing is written to disk and no `Secret` is read.
//!
//! What [`OpenBaoAssertion`] must never do, and the tests hold it to:
//!
//! - send a ServiceAccount token to anything but `https` (or loopback
//!   `http`, for the tests), or follow a redirect;
//! - build a request path from a name that could change it;
//! - print or return a response body, the ServiceAccount token, the login
//!   token or the assertion in an error: an error names the step and the
//!   HTTP status, nothing else.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use crate::error::{Error, Result};
use crate::operation::is_trusted_base;

/// Path of the ServiceAccount token whose audience is OpenBao's.
pub const ENV_OPENBAO_SA_TOKEN_FILE: &str = "BANLIEUE_OPENBAO_SA_TOKEN_FILE";
/// OpenBao's address, `https://...`.
pub const ENV_OPENBAO_ADDR: &str = "BANLIEUE_OPENBAO_ADDR";
/// Optional: the namespace path the engine and the auth mount live in.
pub const ENV_OPENBAO_NAMESPACE: &str = "BANLIEUE_OPENBAO_NAMESPACE";
/// The JWT auth mount that trusts this cluster's ServiceAccount tokens.
pub const ENV_OPENBAO_AUTH_MOUNT: &str = "BANLIEUE_OPENBAO_AUTH_MOUNT";
/// The auth role bound to this Provider's ServiceAccount.
pub const ENV_OPENBAO_LOGIN_ROLE: &str = "BANLIEUE_OPENBAO_LOGIN_ROLE";
/// The identity OIDC role whose token is the assertion.
pub const ENV_OPENBAO_TOKEN_ROLE: &str = "BANLIEUE_OPENBAO_TOKEN_ROLE";

const CODE_TOKEN_FILE: &str = "ServiceAccountTokenUnreadable";
const CODE_CONFIG: &str = "InvalidConfiguration";
const CODE_LOGIN_REFUSED: &str = "OpenBaoLoginRefused";
const CODE_TOKEN_READ_REFUSED: &str = "OpenBaoTokenReadRefused";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// OpenBao's answers are a few KiB; anything far larger is not one.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

const HEADER_TOKEN: &str = "X-Vault-Token";
const HEADER_NAMESPACE: &str = "X-Vault-Namespace";

/// Supplies the assertion presented to Entra ID.
#[async_trait]
pub trait AssertionSource: Send + Sync {
    /// A signed JWT valid for at least the time the exchange takes.
    ///
    /// # Errors
    /// An [`Error`] naming the step that failed, never a secret.
    async fn assertion(&self) -> Result<String>;
}

/// The projected ServiceAccount token, read from a file every time because
/// the kubelet rotates it.
#[derive(Debug, Clone)]
pub struct FileAssertion {
    path: PathBuf,
}

impl FileAssertion {
    /// A source reading `path`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl AssertionSource for FileAssertion {
    async fn assertion(&self) -> Result<String> {
        read_token_file(&self.path).await
    }
}

async fn read_token_file(path: &std::path::Path) -> Result<String> {
    let unreadable = |why: &str| Error::TokenExchange {
        code: CODE_TOKEN_FILE.to_string(),
        message: format!("ServiceAccount token {}: {why}", path.display()),
    };
    let contents = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| unreadable(&e.kind().to_string()))?;
    let token = contents.trim();
    if token.is_empty() {
        return Err(unreadable("empty"));
    }
    Ok(token.to_string())
}

fn config_error(message: String) -> Error {
    Error::InvalidRequest {
        code: CODE_CONFIG.to_string(),
        message,
    }
}

/// What the OpenBao source needs.
#[derive(Clone, Debug)]
pub struct OpenBaoConfig {
    /// OpenBao's address, `https://host[:port]`.
    pub addr: String,
    /// Namespace path, if the engine and mount are not in the root namespace.
    pub namespace: Option<String>,
    /// The JWT auth mount path (`auth/<this>/login`).
    pub auth_mount: String,
    /// The auth role bound to this Provider's ServiceAccount.
    pub login_role: String,
    /// The identity OIDC role (`identity/oidc/token/<this>`).
    pub token_role: String,
    /// The projected ServiceAccount token whose audience is OpenBao's.
    pub service_account_token_file: PathBuf,
}

impl OpenBaoConfig {
    /// Reads the `BANLIEUE_OPENBAO_*` variables through `lookup`.
    ///
    /// # Errors
    /// [`Error::InvalidRequest`] naming the first required variable unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let optional = |name: &str| lookup(name).filter(|v| !v.trim().is_empty());
        let required =
            |name: &str| optional(name).ok_or_else(|| config_error(format!("{name} is not set")));
        Ok(Self {
            addr: required(ENV_OPENBAO_ADDR)?,
            namespace: optional(ENV_OPENBAO_NAMESPACE),
            auth_mount: required(ENV_OPENBAO_AUTH_MOUNT)?,
            login_role: required(ENV_OPENBAO_LOGIN_ROLE)?,
            token_role: required(ENV_OPENBAO_TOKEN_ROLE)?,
            service_account_token_file: PathBuf::from(required(ENV_OPENBAO_SA_TOKEN_FILE)?),
        })
    }

    fn validate(&self) -> Result<()> {
        let addr = self.addr.trim_end_matches('/');
        if !is_trusted_base(addr) {
            return Err(config_error(format!(
                "OpenBao address {addr:?} must be https://"
            )));
        }
        valid_path("auth mount", &self.auth_mount, true)?;
        valid_path("login role", &self.login_role, false)?;
        valid_path("token role", &self.token_role, false)?;
        if let Some(namespace) = &self.namespace {
            valid_path("namespace", namespace, true)?;
        }
        Ok(())
    }
}

/// A name that goes into a request path (`nested` allows `a/b`): each segment
/// is non-empty, not `.` or `..`, and only letters, digits, `.`, `_` and `-`,
/// so no name can add a segment, a query, an escape or a traversal.
fn valid_path(what: &str, value: &str, nested: bool) -> Result<()> {
    let segment_ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    let ok = if nested {
        value.split('/').all(segment_ok)
    } else {
        segment_ok(value)
    };
    if ok {
        return Ok(());
    }
    Err(config_error(format!(
        "OpenBao {what} {value:?} must be letters, digits, '.', '_' and '-'{}",
        if nested {
            " (and '/' between segments)"
        } else {
            ""
        }
    )))
}

/// Gets the assertion from OpenBao's identity secrets engine (ADR-0176).
pub struct OpenBaoAssertion {
    http: reqwest::Client,
    base: String,
    config: OpenBaoConfig,
}

impl fmt::Debug for OpenBaoAssertion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenBaoAssertion")
            .field("base", &self.base)
            .field("namespace", &self.config.namespace)
            .field("auth_mount", &self.config.auth_mount)
            .field("login_role", &self.config.login_role)
            .field("token_role", &self.config.token_role)
            .finish_non_exhaustive()
    }
}

impl OpenBaoAssertion {
    /// Builds the source. Makes no request.
    ///
    /// # Errors
    /// [`Error::InvalidRequest`] if the address is not `https://` (or
    /// loopback `http://`), or a name could change a request's path.
    pub fn new(config: OpenBaoConfig) -> Result<Self> {
        config.validate()?;
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        Ok(Self {
            http,
            base: config.addr.trim_end_matches('/').to_string(),
            config,
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let builder = self
            .http
            .request(method, format!("{}/v1/{path}", self.base));
        match &self.config.namespace {
            Some(namespace) => builder.header(HEADER_NAMESPACE, namespace),
            None => builder,
        }
    }

    /// Sends, and returns the body of a success; a refusal is an error naming
    /// `step` and the status, with `code`, and never the body.
    async fn send(
        &self,
        builder: reqwest::RequestBuilder,
        step: &str,
        code: &str,
    ) -> Result<Vec<u8>> {
        let response = builder
            .send()
            .await
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        let status = response.status();
        let body = crate::client::read_capped(response, MAX_RESPONSE_BYTES).await?;
        if !status.is_success() {
            return Err(Error::TokenExchange {
                code: code.to_string(),
                message: format!("OpenBao {step}: HTTP {}", status.as_u16()),
            });
        }
        Ok(body.to_vec())
    }

    async fn login(&self) -> Result<String> {
        let jwt = read_token_file(&self.config.service_account_token_file).await?;
        let payload = json!({ "role": self.config.login_role, "jwt": jwt }).to_string();
        let body = self
            .send(
                self.request(
                    reqwest::Method::POST,
                    &format!("auth/{}/login", self.config.auth_mount),
                )
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(payload),
                "login",
                CODE_LOGIN_REFUSED,
            )
            .await?;
        string_at(
            &body,
            &["auth", "client_token"],
            "login answer has no client token",
        )
    }

    async fn read_token(&self, login_token: &str) -> Result<String> {
        let body = self
            .send(
                self.request(
                    reqwest::Method::GET,
                    &format!("identity/oidc/token/{}", self.config.token_role),
                )
                .header(HEADER_TOKEN, login_token),
                "token read",
                CODE_TOKEN_READ_REFUSED,
            )
            .await?;
        string_at(&body, &["data", "token"], "token read answer has no token")
    }

    /// Best effort: the login token lives minutes at most, so a failure here
    /// is not worth failing the exchange for.
    async fn revoke(&self, login_token: &str) {
        let _ = self
            .request(reqwest::Method::POST, "auth/token/revoke-self")
            .header(HEADER_TOKEN, login_token)
            .send()
            .await;
    }
}

#[async_trait]
impl AssertionSource for OpenBaoAssertion {
    async fn assertion(&self) -> Result<String> {
        let login_token = self.login().await?;
        let assertion = self.read_token(&login_token).await;
        self.revoke(&login_token).await;
        assertion
    }
}

/// The string at `path` in a JSON body, or a [`Error::Decode`] naming
/// `missing` and nothing of the body.
fn string_at(body: &[u8], path: &[&str], missing: &str) -> Result<String> {
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| Error::Decode("OpenBao answer is not JSON".to_string()))?;
    path.iter()
        .try_fold(&value, |v, key| v.get(key))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Error::Decode(missing.to_string()))
}

#[cfg(test)]
#[path = "assertion_tests.rs"]
mod assertion_tests;
