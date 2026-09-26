// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The provider renews its own cluster credential (ADR-0060 Decision 5).
//!
//! The host's kubeconfig names a **token file**, not an inline token. The
//! kube client re-reads that file about once a minute, so replacing it is
//! all renewal takes: at half of the current token's lifetime the provider
//! asks the API server for a new bound token for its own ServiceAccount
//! (TokenRequest, which its Role allows on that one ServiceAccount only) and
//! atomically swaps the file. The kubeconfig itself never changes and holds
//! no secret.
//!
//! The provider learns which ServiceAccount it is, and when its token
//! expires, from its own token's claims. It reads them without verifying
//! the signature: it is its own token, and the API server verifies it on
//! every request anyway.
//!
//! A host down for longer than one lifetime comes back with an expired
//! token and must be re-bootstrapped. That is the intended failure: a
//! stolen, stale credential dies on its own.

use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use k8s_openapi::api::authentication::v1::{TokenRequest, TokenRequestSpec};
use k8s_openapi::api::core::v1::ServiceAccount;
use kube::Client;
use kube::api::{Api, PostParams};
use kube::config::Kubeconfig;
use serde::Deserialize;
use tracing::{info, warn};

/// Default bound-token lifetime (ADR-0060 Decision 5).
pub const DEFAULT_TOKEN_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
/// How soon to try again after a failed renewal. Well inside half a
/// lifetime, so a transient API outage costs nothing.
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(60);
/// Mode of the token file: the provider's user only.
const TOKEN_MODE: u32 = 0o600;
/// Prefix of a ServiceAccount subject: `system:serviceaccount:<ns>:<name>`.
const SA_SUBJECT_PREFIX: &str = "system:serviceaccount:";
/// A JWT has three dot-separated parts; the claims are the second.
const JWT_CLAIMS_INDEX: usize = 1;

/// The claims the provider reads from its own token.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Claims {
    /// `system:serviceaccount:<namespace>:<name>`.
    pub sub: String,
    /// Issued at, Unix seconds.
    pub iat: i64,
    /// Expires at, Unix seconds.
    pub exp: i64,
}

impl Claims {
    /// Decode a JWT's claims without verifying it.
    ///
    /// # Errors
    /// `InvalidData` if it is not a JWT with `sub`, `iat` and `exp`.
    pub fn parse(token: &str) -> io::Result<Self> {
        let invalid = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
        let payload = token
            .trim()
            .split('.')
            .nth(JWT_CLAIMS_INDEX)
            .ok_or_else(|| invalid("not a JWT".into()))?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .map_err(|e| invalid(format!("token claims: {e}")))?;
        serde_json::from_slice(&bytes).map_err(|e| invalid(format!("token claims: {e}")))
    }

    /// The ServiceAccount the token belongs to, as (namespace, name).
    #[must_use]
    pub fn service_account(&self) -> Option<(&str, &str)> {
        self.sub.strip_prefix(SA_SUBJECT_PREFIX)?.split_once(':')
    }

    /// How long from `now` (Unix seconds) until renewal is due: half-way
    /// through the token's lifetime, or immediately once past it.
    #[must_use]
    pub fn renew_in(&self, now: i64) -> Duration {
        let half_life = self.iat.saturating_add((self.exp - self.iat) / 2);
        u64::try_from(half_life.saturating_sub(now)).map_or(Duration::ZERO, Duration::from_secs)
    }
}

/// The token file the kubeconfig's current user reads, if it uses one.
///
/// # Errors
/// The kubeconfig cannot be read or parsed.
pub fn token_file_of(kubeconfig: &Path) -> io::Result<Option<PathBuf>> {
    let kc = Kubeconfig::read_from(kubeconfig).map_err(io::Error::other)?;
    let Some(context_name) = kc.current_context.as_deref() else {
        return Ok(None);
    };
    let Some(user_name) = kc
        .contexts
        .iter()
        .find(|c| c.name == context_name)
        .and_then(|c| c.context.as_ref())
        .and_then(|c| c.user.clone())
    else {
        return Ok(None);
    };
    Ok(kc
        .auth_infos
        .iter()
        .find(|a| a.name == user_name)
        .and_then(|a| a.auth_info.as_ref())
        .and_then(|a| a.token_file.clone())
        .map(PathBuf::from))
}

/// Replace the token file atomically: a new file in the same directory,
/// `0600`, written and synced, then renamed over the old one.
///
/// # Errors
/// The I/O error.
pub fn write_token(path: &Path, token: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "token path has no file name")
    })?;
    let tmp = dir.join(format!(".{}.new", name.to_string_lossy()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(TOKEN_MODE)
        .open(&tmp)?;
    f.set_permissions(std::fs::Permissions::from_mode(TOKEN_MODE))?;
    f.write_all(token.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Ask for a new token for the ServiceAccount `namespace`/`name`.
async fn request(
    client: &Client,
    namespace: &str,
    name: &str,
    lifetime: Duration,
) -> kube::Result<String> {
    let api: Api<ServiceAccount> = Api::namespaced(client.clone(), namespace);
    let req = TokenRequest {
        spec: Some(TokenRequestSpec {
            // No audiences: the API server's default, which is what the kube
            // client presents.
            audiences: None,
            expiration_seconds: i64::try_from(lifetime.as_secs()).ok(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let resp = api
        .create_token_request(name, &PostParams::default(), &req)
        .await?;
    Ok(resp.status.and_then(|s| s.token).unwrap_or_default())
}

/// Renew the token in `path` for as long as the process runs. Returns only
/// if renewal cannot work at all (the file is not a ServiceAccount token).
pub async fn renew_forever(client: Client, path: PathBuf, lifetime: Duration) {
    loop {
        let current = match std::fs::read_to_string(&path).map(|t| Claims::parse(&t)) {
            Ok(Ok(c)) => c,
            Ok(Err(e)) | Err(e) => {
                warn!(path = %path.display(), error = %e, "cannot read the provider's token; renewal stops");
                return;
            }
        };
        let Some((namespace, name)) = current.service_account() else {
            warn!(sub = %current.sub, "token is not a ServiceAccount token; renewal stops");
            return;
        };
        let (namespace, name) = (namespace.to_string(), name.to_string());
        let wait = current.renew_in(now_unix());
        info!(service_account = %name, in_secs = wait.as_secs(), "next token renewal");
        tokio::time::sleep(wait).await;

        match request(&client, &namespace, &name, lifetime).await {
            Ok(token) if !token.is_empty() => match write_token(&path, &token) {
                Ok(()) => info!(service_account = %name, "token renewed"),
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "writing the renewed token failed");
                    tokio::time::sleep(RETRY_AFTER_FAILURE).await;
                }
            },
            Ok(_) => {
                warn!("TokenRequest returned no token");
                tokio::time::sleep(RETRY_AFTER_FAILURE).await;
            }
            Err(e) => {
                warn!(error = %e, "token renewal failed; retrying");
                tokio::time::sleep(RETRY_AFTER_FAILURE).await;
            }
        }
    }
}

#[cfg(test)]
#[path = "token_tests.rs"]
mod token_tests;
