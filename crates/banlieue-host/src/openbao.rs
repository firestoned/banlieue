// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! OpenBao as the signer of the host CA's request (ADR-0146 Decision 8).
//!
//! One exchange, once, by the installer: log in, post the pending request to
//! the node-attestation mount's `root/sign-intermediate`, revoke the token.
//! The login is the host's own ServiceAccount token (so OpenBao can pin the
//! CA's name to the host's identity), or, for a host with no cluster
//! identity, a token an administrator minted with the same pins.
//!
//! What this module must never do, and the tests hold it to:
//!
//! - talk to anything but an `https` origin with the host's own trust store
//!   (there is no way to turn verification off outside tests);
//! - send a parameter the policy does not list (a parameter not listed is
//!   refused by OpenBao, so the request is exactly five fields);
//! - print or return a response body, a token or a request: an error names
//!   the step and the HTTP status, and nothing else;
//! - trust the answer: what comes back is only text for the CA crate's
//!   `import`, which checks it against the pending key before installing it.

use crate::error::Error;
use async_trait::async_trait;
use std::time::Duration;

/// How long a connection may take to open.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a whole request may take.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Larger than any reply to a login or a signing (a few KiB), so a hostile or
/// broken server cannot make the installer buffer without bound.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// The CA's path length the request pins: it may sign leaves and nothing else.
const CA_PATH_LENGTH: u8 = 0;
/// The CA lifetime sent when none is asked for. It is **pinned by the policy**
/// the OpenBao repository's onboarding script writes, so it is sent exactly as
/// written, and must match that script's value.
pub const DEFAULT_CA_TTL: &str = "8760h";
/// The header OpenBao reads a token from (Vault-compatible, as OpenBao accepts).
const TOKEN_HEADER: &str = "X-Vault-Token";
const USER_AGENT: &str = concat!("banlieue-host/", env!("CARGO_PKG_VERSION"));
const JSON: &str = "application/json";

/// How the installer proves itself to OpenBao.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    /// Log in to the JWT auth mount `auth_mount`, role `role`, with the host's
    /// ServiceAccount token.
    Jwt {
        /// The JWT auth mount of this cluster.
        auth_mount: String,
        /// The login role, bound to this host's ServiceAccount.
        role: String,
        /// The ServiceAccount token.
        jwt: String,
    },
    /// A token already minted for this one signing.
    Token(String),
}

impl std::fmt::Debug for Credential {
    /// Never prints a secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Jwt {
                auth_mount, role, ..
            } => write!(
                f,
                "Jwt {{ auth_mount: {auth_mount:?}, role: {role:?}, jwt: <redacted> }}"
            ),
            Self::Token(_) => write!(f, "Token(<redacted>)"),
        }
    }
}

/// One request to sign.
#[derive(Debug)]
pub struct SignRequest<'a> {
    /// The pending request, PEM.
    pub csr_pem: &'a str,
    /// The CA's name: pinned by the policy, and the common name of the result.
    pub common_name: &'a str,
    /// The CA's lifetime, as the policy pins it (`8760h`).
    pub ttl: &'a str,
    /// How to authenticate.
    pub credential: &'a Credential,
}

/// Something that can sign a host CA request.
#[async_trait]
pub trait Pki: Send + Sync {
    /// The signed certificate followed by the chain above it, PEM: the text
    /// the CA crate's `import` takes. Unverified: the caller checks it.
    ///
    /// # Errors
    /// [`Error::Pki`], naming the step and the HTTP status and never a body.
    async fn sign_intermediate(&self, req: &SignRequest<'_>) -> Result<String, Error>;
}

/// The installer with no OpenBao client: every route but the OpenBao one.
#[derive(Debug)]
pub struct NoPki;

#[async_trait]
impl Pki for NoPki {
    async fn sign_intermediate(&self, _req: &SignRequest<'_>) -> Result<String, Error> {
        Err(Error::Pki("this run has no OpenBao client".into()))
    }
}

/// Whether `path` is a mount path: one or more `/`-separated segments of
/// letters, digits, dots, dashes and underscores, none empty, none `.` or `..`.
#[must_use]
pub fn valid_path(path: &str) -> bool {
    !path.is_empty() && path.split('/').all(valid_segment)
}

/// Whether `s` is one path segment (a role name, a mount's single component).
#[must_use]
pub fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

fn pki(why: impl Into<String>) -> Error {
    Error::Pki(why.into())
}

/// The real client, for one OpenBao and one mount.
pub struct OpenBao {
    base: String,
    mount: String,
    client: reqwest::Client,
}

impl OpenBao {
    /// A client for `addr` (a bare `https` origin) and the PKI mount `mount`.
    ///
    /// # Errors
    /// [`Error::Pki`] for an address that is not a bare `https` origin (no
    /// credentials, path, query or fragment), a mount that could leave its
    /// place, or a TLS client that cannot be built.
    pub fn new(addr: &str, mount: &str) -> Result<Self, Error> {
        Self::build(addr, mount, true)
    }

    /// As [`OpenBao::new`] but `http` is accepted, so a test can talk to a
    /// loopback server. Compiled only into tests: no run of the installer can
    /// turn verification off.
    #[cfg(test)]
    pub(crate) fn new_plain_http_for_tests(addr: &str, mount: &str) -> Result<Self, Error> {
        Self::build(addr, mount, false)
    }

    fn build(addr: &str, mount: &str, https_only: bool) -> Result<Self, Error> {
        let url = reqwest::Url::parse(addr).map_err(|_| pki(format!("{addr:?} is not a URL")))?;
        let scheme_ok = url.scheme() == "https" || (!https_only && url.scheme() == "http");
        if !scheme_ok {
            return Err(pki("the address must be https"));
        }
        if url.host_str().is_none() {
            return Err(pki("the address names no host"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(pki("the address must not carry credentials"));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(pki("the address must not carry a query or a fragment"));
        }
        if !matches!(url.path(), "" | "/") {
            return Err(pki("the address must be an origin, with no path"));
        }
        if !valid_path(mount) {
            return Err(pki(format!("{mount:?} is not a mount path")));
        }
        // reqwest's `rustls-no-provider` needs a process-wide provider;
        // installing it twice is harmless.
        let _ = banlieue_tls::install_default_crypto_provider();
        let client = reqwest::Client::builder()
            .https_only(https_only)
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| pki(format!("TLS client (is ca-certificates installed?): {e}")))?;
        Ok(Self {
            base: url.origin().ascii_serialization(),
            mount: mount.to_string(),
            client,
        })
    }

    /// POST `body` to `path`, with `token` when there is one; the reply's
    /// status and its body, refused past [`MAX_RESPONSE_BYTES`].
    async fn post(
        &self,
        path: &str,
        token: Option<&str>,
        body: String,
        step: &str,
    ) -> Result<(u16, Vec<u8>), Error> {
        let mut request = self
            .client
            .post(format!("{}/v1/{path}", self.base))
            .header(reqwest::header::CONTENT_TYPE, JSON)
            .body(body);
        if let Some(t) = token {
            request = request.header(TOKEN_HEADER, t);
        }
        // reqwest's own error text names the URL and the cause, never a body.
        let mut resp = request
            .send()
            .await
            .map_err(|e| pki(format!("{step}: {}", e.without_url())))?;
        let status = resp.status().as_u16();
        let mut buf = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| pki(format!("{step}: {}", e.without_url())))?
        {
            if buf.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(pki(format!(
                    "{step}: reply larger than {MAX_RESPONSE_BYTES} bytes"
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok((status, buf))
    }

    /// POST and require a 2xx reply with a JSON body.
    async fn post_json(
        &self,
        path: &str,
        token: Option<&str>,
        body: String,
        step: &str,
    ) -> Result<serde_json::Value, Error> {
        let (status, bytes) = self.post(path, token, body, step).await?;
        if !(200..300).contains(&status) {
            return Err(pki(format!("{step} refused (HTTP {status})")));
        }
        serde_json::from_slice(&bytes).map_err(|_| pki(format!("{step}: the reply is not JSON")))
    }

    async fn login(&self, auth_mount: &str, role: &str, jwt: &str) -> Result<String, Error> {
        if !valid_path(auth_mount) || !valid_segment(role) {
            return Err(pki("the auth mount or the role is not a plain name"));
        }
        let body = serde_json::json!({"role": role, "jwt": jwt}).to_string();
        let reply = self
            .post_json(&format!("auth/{auth_mount}/login"), None, body, "login")
            .await?;
        reply
            .pointer("/auth/client_token")
            .and_then(serde_json::Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .ok_or_else(|| pki("login: the reply carries no client token"))
    }

    async fn sign(&self, token: &str, req: &SignRequest<'_>) -> Result<String, Error> {
        // Exactly the parameters the policy lists: one it does not list is refused.
        let body = serde_json::json!({
            "csr": req.csr_pem,
            "common_name": req.common_name,
            "max_path_length": CA_PATH_LENGTH,
            "ttl": req.ttl,
            "use_csr_values": false,
        })
        .to_string();
        let reply = self
            .post_json(
                &format!("{}/root/sign-intermediate", self.mount),
                Some(token),
                body,
                "sign",
            )
            .await?;
        bundle_from(&reply)
    }

    /// Best effort: a one-use token is already gone when the signing returns,
    /// so a refusal here is the expected case.
    async fn revoke(&self, token: &str) {
        let _ = self
            .post(
                "auth/token/revoke-self",
                Some(token),
                String::new(),
                "revoke",
            )
            .await;
    }
}

/// The certificate followed by its chain, as one PEM text: `ca_chain` when the
/// reply has one, else `issuing_ca`.
fn bundle_from(reply: &serde_json::Value) -> Result<String, Error> {
    let text = |v: &serde_json::Value| {
        v.as_str()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
    };
    let certificate = reply
        .pointer("/data/certificate")
        .and_then(text)
        .ok_or_else(|| pki("sign: the reply carries no certificate"))?;
    let chain: Vec<String> = match reply.pointer("/data/ca_chain").and_then(|c| c.as_array()) {
        Some(items) => items.iter().filter_map(text).collect(),
        None => reply
            .pointer("/data/issuing_ca")
            .and_then(text)
            .into_iter()
            .collect(),
    };
    let mut out = String::new();
    for part in std::iter::once(certificate).chain(chain) {
        out.push_str(&part);
        if !part.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}

#[async_trait]
impl Pki for OpenBao {
    async fn sign_intermediate(&self, req: &SignRequest<'_>) -> Result<String, Error> {
        let token = match req.credential {
            Credential::Token(t) => t.clone(),
            Credential::Jwt {
                auth_mount,
                role,
                jwt,
            } => self.login(auth_mount, role, jwt).await?,
        };
        let signed = self.sign(&token, req).await;
        self.revoke(&token).await;
        signed
    }
}

#[cfg(test)]
#[path = "openbao_tests.rs"]
mod openbao_tests;
