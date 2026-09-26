// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Registry authentication: anonymous, HTTP Basic, and the Bearer token
//! challenge (`WWW-Authenticate: Bearer realm=…,service=…,scope=…`) that
//! GHCR, Docker Hub, Harbor and most hosted registries use.

use std::path::Path;

use crate::error::{Error, Result};

/// File holding the username in a credentials directory — the key of a
/// `kubernetes.io/basic-auth` Secret, so a mounted Secret is one as-is.
pub const USERNAME_FILE: &str = "username";
/// File holding the password or token.
pub const PASSWORD_FILE: &str = "password";

/// Credentials for a registry.
#[derive(Clone, Default)]
pub struct Credentials {
    /// Username, when the registry needs one.
    pub username: Option<String>,
    /// Password or token.
    pub password: Option<String>,
}

impl std::fmt::Debug for Credentials {
    // Never print the password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Credentials {
    /// Anonymous.
    #[must_use]
    pub fn anonymous() -> Self {
        Self::default()
    }

    /// Read `username` and `password` from `dir`: a mounted
    /// `kubernetes.io/basic-auth` Secret, or the host's registry
    /// credentials directory. Neither file present is anonymous.
    ///
    /// # Errors
    /// [`Error::Auth`] when only one of the two is present, or
    /// [`Error::Io`] when one cannot be read.
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let read = |name: &str| -> Result<Option<String>> {
            match std::fs::read_to_string(dir.join(name)) {
                Ok(s) => Ok(Some(s.trim_end_matches(['\r', '\n']).to_string())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(Error::Io(e)),
            }
        };
        let username = read(USERNAME_FILE)?;
        let password = read(PASSWORD_FILE)?;
        if username.is_some() != password.is_some() {
            return Err(Error::Auth(format!(
                "{}: {USERNAME_FILE} and {PASSWORD_FILE} must both be present, or neither",
                dir.display()
            )));
        }
        Ok(Self { username, password })
    }

    /// Whether any credential is set.
    #[must_use]
    pub fn is_anonymous(&self) -> bool {
        self.username.is_none() && self.password.is_none()
    }
}

/// A parsed `WWW-Authenticate` challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Challenge {
    /// `Basic realm=…`.
    Basic,
    /// `Bearer realm=…,service=…,scope=…`.
    Bearer {
        /// Token endpoint.
        realm: String,
        /// `service` parameter, if any.
        service: Option<String>,
        /// `scope` parameter, if any.
        scope: Option<String>,
    },
}

/// Parse a `WWW-Authenticate` header value.
///
/// # Errors
/// [`Error::Auth`] for an unknown scheme or a Bearer challenge without a
/// realm.
pub fn parse_challenge(header: &str) -> Result<Challenge> {
    let (scheme, params) = header.trim().split_once(' ').unwrap_or((header.trim(), ""));
    if scheme.eq_ignore_ascii_case("basic") {
        return Ok(Challenge::Basic);
    }
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(Error::Auth(format!("unsupported scheme {scheme:?}")));
    }
    let mut realm = None;
    let mut service = None;
    let mut scope = None;
    for (k, v) in split_params(params) {
        match k.to_ascii_lowercase().as_str() {
            "realm" => realm = Some(v),
            "service" => service = Some(v),
            "scope" => scope = Some(v),
            _ => {}
        }
    }
    Ok(Challenge::Bearer {
        realm: realm.ok_or_else(|| Error::Auth("Bearer challenge without realm".into()))?,
        service,
        scope,
    })
}

/// `k="v",k2="v,with,commas"` → pairs. Quoted values may contain commas.
fn split_params(params: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = params.trim();
    while !rest.is_empty() {
        let Some((key, after)) = rest.split_once('=') else {
            break;
        };
        let key = key.trim().trim_start_matches(',').trim().to_string();
        let after = after.trim_start();
        let (value, remainder) = if let Some(quoted) = after.strip_prefix('"') {
            match quoted.split_once('"') {
                Some((v, r)) => (v.to_string(), r),
                None => (quoted.to_string(), ""),
            }
        } else {
            match after.split_once(',') {
                Some((v, r)) => (v.trim().to_string(), r),
                None => (after.trim().to_string(), ""),
            }
        };
        out.push((key, value));
        rest = remainder.trim_start().trim_start_matches(',').trim_start();
    }
    out
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod auth_tests;
