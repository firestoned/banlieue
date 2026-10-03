// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Where the artifacts come from: their URLs over HTTPS (the upstream
//! releases, or a mirror an operator names, ADR-0084 Decision 2), or a
//! local directory (`--artifacts-dir`) for a host with no network or a
//! container test. Either way the caller verifies the sha256 before
//! anything is installed (ADR-0067 Decision 4); this module only fetches.
//! It also reads the digests GitHub publishes, for a release other than
//! the pinned one (ADR-0084 Decision 4).

use crate::error::Error;
use crate::pins::Artifact;
use crate::release::{Digests, parse_published_digests};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Larger than any pinned artifact (the VMM is ~5 MiB, firmware ~2 MiB),
/// so a hostile or broken server cannot make the installer buffer without
/// bound before the digest check refuses it.
pub const MAX_ARTIFACT_BYTES: usize = 256 * 1024 * 1024;
/// Larger than any GitHub release document (a few KiB per asset).
pub const MAX_RELEASE_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
/// GitHub's REST API.
const GITHUB_API: &str = "https://api.github.com";
/// GitHub refuses API requests without a User-Agent.
const USER_AGENT: &str = concat!("banlieue-host/", env!("CARGO_PKG_VERSION"));
/// The media type of GitHub's REST API.
const GITHUB_JSON: &str = "application/vnd.github+json";

/// An HTTPS-only client verifying against the system's trust store.
fn https_client() -> Result<reqwest::Client, String> {
    // reqwest's `rustls-no-provider` needs a process-wide provider;
    // installing it twice is harmless.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .https_only(true)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("TLS client (is ca-certificates installed?): {e}"))
}

/// `url`'s body, refused past `limit` bytes before it is all buffered.
async fn get_capped(
    client: &reqwest::Client,
    url: &str,
    accept: Option<&str>,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut request = client.get(url);
    if let Some(a) = accept {
        request = request.header(reqwest::header::ACCEPT, a);
    }
    let mut resp = request
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("{url}: {e}"))?;
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("{url}: {e}"))? {
        if body.len() + chunk.len() > limit {
            return Err(format!("{url}: larger than {limit} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// A source of artifacts.
#[async_trait]
pub trait Fetch: Send + Sync {
    /// The artifact's bytes, unverified.
    ///
    /// # Errors
    /// [`Error::Fetch`] naming the artifact.
    async fn fetch(&self, artifact: &Artifact) -> Result<Vec<u8>, Error>;
}

/// Each artifact's URL, over HTTPS.
///
/// The TLS client is built on the first download, not before, so a
/// command that downloads nothing never needs a trust store.
#[derive(Default)]
pub struct Download {
    client: tokio::sync::OnceCell<reqwest::Client>,
}

impl Download {
    /// A downloader; nothing is built until the first download.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Fetch for Download {
    async fn fetch(&self, artifact: &Artifact) -> Result<Vec<u8>, Error> {
        let fail = |why: String| Error::Fetch {
            name: artifact.name,
            why,
        };
        let client = self
            .client
            .get_or_try_init(|| async { https_client() })
            .await
            .map_err(fail)?;
        get_capped(client, &artifact.url, None, MAX_ARTIFACT_BYTES)
            .await
            .map_err(fail)
    }
}

/// The digests GitHub publishes, from `api.github.com`. Asked only for a
/// release other than the pinned one, when no digest flag was given.
#[derive(Default)]
pub struct GitHubDigests {
    client: tokio::sync::OnceCell<reqwest::Client>,
}

impl GitHubDigests {
    /// A reader; nothing is built until the first request.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Digests for GitHubDigests {
    async fn published(
        &self,
        org: &str,
        repo: &str,
        tag: &str,
    ) -> Result<BTreeMap<String, String>, String> {
        let client = self
            .client
            .get_or_try_init(|| async { https_client() })
            .await?;
        let url = format!("{GITHUB_API}/repos/{org}/{repo}/releases/tags/{tag}");
        let body = get_capped(client, &url, Some(GITHUB_JSON), MAX_RELEASE_DOCUMENT_BYTES).await?;
        parse_published_digests(&body)
    }
}

/// A directory holding the release assets under their upstream names.
pub struct FromDir {
    /// The directory.
    pub dir: PathBuf,
}

#[async_trait]
impl Fetch for FromDir {
    async fn fetch(&self, artifact: &Artifact) -> Result<Vec<u8>, Error> {
        let path = self.dir.join(artifact.name);
        let len = std::fs::metadata(&path)
            .map_err(|e| Error::Fetch {
                name: artifact.name,
                why: format!("{}: {e}", path.display()),
            })?
            .len();
        if usize::try_from(len).map_or(true, |l| l > MAX_ARTIFACT_BYTES) {
            return Err(Error::Fetch {
                name: artifact.name,
                why: format!("{}: larger than {MAX_ARTIFACT_BYTES} bytes", path.display()),
            });
        }
        std::fs::read(&path).map_err(|e| Error::Fetch {
            name: artifact.name,
            why: format!("{}: {e}", path.display()),
        })
    }
}
