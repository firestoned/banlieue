// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Where the pinned artifacts come from: the upstream releases over HTTPS,
//! or a local directory (`--artifacts-dir`) for an air-gapped host or a
//! container test. Either way the caller verifies the sha256 before
//! anything is installed (ADR-0067 Decision 4); this module only fetches.

use crate::error::Error;
use crate::pins::Artifact;
use async_trait::async_trait;
use std::path::PathBuf;

/// Larger than any pinned artifact (the VMM is ~5 MiB, firmware ~2 MiB),
/// so a hostile or broken server cannot make the installer buffer without
/// bound before the digest check refuses it.
pub const MAX_ARTIFACT_BYTES: usize = 256 * 1024 * 1024;

/// A source of artifacts.
#[async_trait]
pub trait Fetch: Send + Sync {
    /// The artifact's bytes, unverified.
    ///
    /// # Errors
    /// [`Error::Fetch`] naming the artifact.
    async fn fetch(&self, artifact: &Artifact) -> Result<Vec<u8>, Error>;
}

/// The upstream releases, over HTTPS.
///
/// The TLS client is built on the first download, not before: it verifies
/// against the system's trust store, which a fresh host only has once the
/// `packages` stage has installed `ca-certificates`.
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

    fn client(&self) -> Result<reqwest::Client, String> {
        // reqwest's `rustls-no-provider` needs a process-wide provider;
        // installing it twice is harmless.
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::builder()
            .https_only(true)
            .build()
            .map_err(|e| format!("TLS client (is ca-certificates installed?): {e}"))
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
            .get_or_try_init(|| async { self.client() })
            .await
            .map_err(fail)?;
        let mut resp = client
            .get(&artifact.url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| fail(format!("{}: {e}", artifact.url)))?;
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| fail(e.to_string()))? {
            if body.len() + chunk.len() > MAX_ARTIFACT_BYTES {
                return Err(fail(format!("larger than {MAX_ARTIFACT_BYTES} bytes")));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
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
