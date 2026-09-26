// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! OCI references: `registry/repository[:tag][@sha256:<hex>]`.
//!
//! The registry is always explicit — `ghcr.io/org/disk`, `192.0.2.5:5000/x`.
//! There is no implicit Docker Hub: an image name that silently resolves to
//! someone else's registry is a supply-chain hazard, not a convenience.

use std::fmt;

use crate::error::{Error, Result};

/// `sha256:` digest prefix, the only algorithm accepted.
pub const SHA256_PREFIX: &str = "sha256:";
/// Hex characters in a sha256 digest.
const SHA256_HEX_LEN: usize = 64;

/// A parsed reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    /// Registry host, with a port if any.
    pub registry: String,
    /// Repository path within the registry.
    pub repository: String,
    /// Tag, if the reference had one.
    pub tag: Option<String>,
    /// `sha256:<hex>` digest, if the reference had one.
    pub digest: Option<String>,
}

impl Reference {
    /// Parse `s`.
    ///
    /// # Errors
    /// [`Error::Reference`] for anything that is not a registry-qualified
    /// reference with a valid tag and/or sha256 digest.
    pub fn parse(s: &str) -> Result<Self> {
        let bad = |why: &str| Error::Reference(format!("{s:?}: {why}"));
        let (rest, digest) = match s.split_once('@') {
            Some((r, d)) => {
                require_digest(d).map_err(|_| bad("digest must be sha256:<64 hex>"))?;
                (r, Some(d.to_string()))
            }
            None => (s, None),
        };
        let (registry, path) = rest
            .split_once('/')
            .ok_or_else(|| bad("must start with a registry host"))?;
        let looks_like_host =
            registry.contains('.') || registry.contains(':') || registry == "localhost";
        if !looks_like_host {
            return Err(bad(
                "the first component must be a registry host (no implicit Docker Hub)",
            ));
        }
        // A tag is after the last ':' of the path's last component.
        let (repository, tag) = match path.rsplit_once(':') {
            Some((repo, tag)) if !tag.contains('/') => (repo, Some(tag.to_string())),
            _ => (path, None),
        };
        let repo_ok = !repository.is_empty()
            && repository.split('/').all(|c| {
                !c.is_empty()
                    && c.bytes().all(|b| {
                        b.is_ascii_lowercase()
                            || b.is_ascii_digit()
                            || matches!(b, b'.' | b'_' | b'-')
                    })
            });
        if !repo_ok {
            return Err(bad("repository must be lowercase path components"));
        }
        if let Some(t) = &tag {
            let tag_ok = !t.is_empty()
                && t.len() <= 128
                && t.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
            if !tag_ok {
                return Err(bad("invalid tag"));
            }
        }
        Ok(Self {
            registry: registry.to_string(),
            repository: repository.to_string(),
            tag,
            digest,
        })
    }

    /// The same repository, addressed by `digest`.
    #[must_use]
    pub fn with_digest(&self, digest: &str) -> Self {
        Self {
            tag: None,
            digest: Some(digest.to_string()),
            ..self.clone()
        }
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.registry, self.repository)?;
        if let Some(t) = &self.tag {
            write!(f, ":{t}")?;
        }
        if let Some(d) = &self.digest {
            write!(f, "@{d}")?;
        }
        Ok(())
    }
}

/// Check `d` is `sha256:` and 64 lowercase hex digits.
///
/// # Errors
/// [`Error::Reference`] otherwise.
pub fn require_digest(d: &str) -> Result<()> {
    let ok = d.strip_prefix(SHA256_PREFIX).is_some_and(|h| {
        h.len() == SHA256_HEX_LEN
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    });
    if ok {
        return Ok(());
    }
    Err(Error::Reference(format!("{d:?} is not a sha256 digest")))
}

#[cfg(test)]
#[path = "reference_tests.rs"]
mod reference_tests;
