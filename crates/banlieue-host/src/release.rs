// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Which VMM release `install` and `selftest` mean (ADR-0084 Decisions 2
//! to 4).
//!
//! Three files, each with a URL (github.com unless a flag names a mirror)
//! and a sha256: the compiled pin for the pinned version or tag, otherwise
//! the operator's digest flag, otherwise the digest GitHub publishes for
//! that release asset. Everything is decided here, before anything is
//! fetched, so a bad flag installs nothing.

use crate::error::Error;
use crate::pins::{self, Release, Source};
use async_trait::async_trait;
use banlieue_cloud_hypervisor::wire::parse_version;
use clap::Args;
use std::collections::BTreeMap;

/// Hex digits in a sha256.
const SHA256_HEX_LEN: usize = 64;
/// How GitHub prefixes an asset's digest.
const SHA256_PREFIX: &str = "sha256:";
/// The only scheme a download may use.
const HTTPS_SCHEME: &str = "https://";

/// The release flags `install` and `selftest` share.
#[derive(Clone, Debug, Default, Args)]
pub struct ReleaseArgs {
    /// The Cloud Hypervisor release, e.g. `v54.0` (default: the pinned
    /// one). An older release than the pinned one is refused: the
    /// provider would not drive it.
    #[arg(long, env = "BANLIEUE_HOST_VMM_VERSION")]
    pub vmm_version: Option<String>,
    /// The edk2 firmware release tag (default: the pinned one).
    #[arg(long, env = "BANLIEUE_HOST_FIRMWARE_TAG")]
    pub firmware_tag: Option<String>,
    /// Download `cloud-hypervisor-static` from this HTTPS URL instead of
    /// its GitHub release, e.g. an Artifactory remote.
    #[arg(long, env = "BANLIEUE_HOST_VMM_URL")]
    pub vmm_url: Option<String>,
    /// Download `ch-remote-static` from this HTTPS URL instead of its
    /// GitHub release.
    #[arg(long, env = "BANLIEUE_HOST_CH_REMOTE_URL")]
    pub ch_remote_url: Option<String>,
    /// Download `CLOUDHV.fd` from this HTTPS URL instead of its GitHub
    /// release.
    #[arg(long, env = "BANLIEUE_HOST_FIRMWARE_URL")]
    pub firmware_url: Option<String>,
    /// sha256 of `cloud-hypervisor-static`, for a version other than the
    /// pinned one (default: the digest GitHub publishes for it).
    #[arg(long, env = "BANLIEUE_HOST_VMM_SHA256")]
    pub vmm_sha256: Option<String>,
    /// sha256 of `ch-remote-static`, for a version other than the pinned
    /// one (default: the digest GitHub publishes for it).
    #[arg(long, env = "BANLIEUE_HOST_CH_REMOTE_SHA256")]
    pub ch_remote_sha256: Option<String>,
    /// sha256 of `CLOUDHV.fd`, for a tag other than the pinned one
    /// (default: the digest GitHub publishes for it).
    #[arg(long, env = "BANLIEUE_HOST_FIRMWARE_SHA256")]
    pub firmware_sha256: Option<String>,
}

/// The digests GitHub publishes for a release's assets.
#[async_trait]
pub trait Digests: Send + Sync {
    /// Asset name to sha256 (lowercase hex, no prefix) for `org/repo`'s
    /// release `tag`.
    ///
    /// # Errors
    /// Why the release could not be read.
    async fn published(
        &self,
        org: &str,
        repo: &str,
        tag: &str,
    ) -> Result<BTreeMap<String, String>, String>;
}

/// Asset name to sha256 from a GitHub release document
/// (`GET /repos/{org}/{repo}/releases/tags/{tag}`). Assets without a
/// sha256 digest are left out.
///
/// # Errors
/// The body is not a release.
pub fn parse_published_digests(body: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let release: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("not a release: {e}"))?;
    let assets = release
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "not a release: no assets".to_string())?;
    Ok(assets
        .iter()
        .filter_map(|a| {
            let name = a.get("name")?.as_str()?;
            let digest = a.get("digest")?.as_str()?.strip_prefix(SHA256_PREFIX)?;
            Some((name.to_string(), digest.to_string()))
        })
        .collect())
}

/// One file's inputs.
struct File<'a> {
    asset: &'static str,
    repo: &'static str,
    tag: &'a str,
    /// Its compiled digest, when `tag` is the pinned one.
    pinned: Option<&'static str>,
    url: Option<&'a str>,
    url_flag: &'static str,
    sha256: Option<&'a str>,
    sha256_flag: &'static str,
}

/// Asset name to sha256, or why the release could not be read.
type Published = Result<BTreeMap<String, String>, String>;

/// GitHub's digests, read at most once per release.
struct Lookup<'a> {
    digests: &'a dyn Digests,
    seen: BTreeMap<(&'static str, String), Published>,
}

impl Lookup<'_> {
    async fn get(&mut self, repo: &'static str, tag: &str) -> &Published {
        let key = (repo, tag.to_string());
        if !self.seen.contains_key(&key) {
            let found = self.digests.published(pins::UPSTREAM_ORG, repo, tag).await;
            self.seen.insert(key.clone(), found);
        }
        &self.seen[&key]
    }
}

/// The release `args` name, every digest known.
///
/// # Errors
/// [`Error::Setting`] naming the flag: a version older than the client's
/// gate or not a version, a tag that is not a plain name, a URL that is
/// not HTTPS, a malformed digest, a digest flag for a pinned file, or no
/// digest for a file that is not pinned.
pub async fn resolve(args: &ReleaseArgs, digests: &dyn Digests) -> Result<Release, Error> {
    let version = args.vmm_version.as_deref().unwrap_or(pins::VMM_VERSION);
    let firmware_tag = args.firmware_tag.as_deref().unwrap_or(pins::FIRMWARE_TAG);
    check_vmm_version(version)?;
    check_dir_name("--firmware-tag", firmware_tag)?;
    let vmm_pinned = version == pins::VMM_VERSION;
    let firmware_pinned = firmware_tag == pins::FIRMWARE_TAG;

    let mut lookup = Lookup {
        digests,
        seen: BTreeMap::new(),
    };
    let vmm = source(
        File {
            asset: pins::VMM_ASSET,
            repo: pins::VMM_REPO,
            tag: version,
            pinned: vmm_pinned.then_some(pins::VMM_SHA256),
            url: args.vmm_url.as_deref(),
            url_flag: "--vmm-url",
            sha256: args.vmm_sha256.as_deref(),
            sha256_flag: "--vmm-sha256",
        },
        &mut lookup,
    )
    .await?;
    let ch_remote = source(
        File {
            asset: pins::CH_REMOTE_ASSET,
            repo: pins::VMM_REPO,
            tag: version,
            pinned: vmm_pinned.then_some(pins::CH_REMOTE_SHA256),
            url: args.ch_remote_url.as_deref(),
            url_flag: "--ch-remote-url",
            sha256: args.ch_remote_sha256.as_deref(),
            sha256_flag: "--ch-remote-sha256",
        },
        &mut lookup,
    )
    .await?;
    let firmware = source(
        File {
            asset: pins::FIRMWARE_ASSET,
            repo: pins::FIRMWARE_REPO,
            tag: firmware_tag,
            pinned: firmware_pinned.then_some(pins::FIRMWARE_SHA256),
            url: args.firmware_url.as_deref(),
            url_flag: "--firmware-url",
            sha256: args.firmware_sha256.as_deref(),
            sha256_flag: "--firmware-sha256",
        },
        &mut lookup,
    )
    .await?;
    Ok(Release::new(
        version,
        firmware_tag,
        vmm,
        ch_remote,
        firmware,
    ))
}

async fn source(f: File<'_>, lookup: &mut Lookup<'_>) -> Result<Source, Error> {
    let url = match f.url {
        Some(u) => check_url(f.url_flag, u)?,
        None => pins::github_url(f.repo, f.tag, f.asset),
    };
    if let Some(pin) = f.pinned {
        if f.sha256.is_some() {
            return Err(Error::Setting {
                name: f.sha256_flag,
                why: format!(
                    "{} {} is the pinned release; its sha256 is compiled in",
                    f.asset, f.tag
                ),
            });
        }
        return Ok(Source {
            url,
            sha256: pin.into(),
        });
    }
    if let Some(given) = f.sha256 {
        return Ok(Source {
            url,
            sha256: check_sha256(f.sha256_flag, given)?,
        });
    }
    let published = match lookup.get(f.repo, f.tag).await {
        Ok(assets) => assets.get(f.asset).cloned(),
        Err(e) => {
            return Err(Error::Setting {
                name: f.sha256_flag,
                why: format!(
                    "{} {} is not the pinned release, and GitHub's digest for it could not be read ({e}); pass {}",
                    f.asset, f.tag, f.sha256_flag
                ),
            });
        }
    };
    let Some(digest) = published else {
        return Err(Error::Setting {
            name: f.sha256_flag,
            why: format!(
                "GitHub's {} release publishes no sha256 for {}; pass {}",
                f.tag, f.asset, f.sha256_flag
            ),
        });
    };
    Ok(Source {
        url,
        sha256: check_sha256(f.sha256_flag, &digest)?,
    })
}

/// A version the provider drives, and a safe directory name.
fn check_vmm_version(version: &str) -> Result<(), Error> {
    const FLAG: &str = "--vmm-version";
    check_dir_name(FLAG, version)?;
    let gate = banlieue_cloud_hypervisor::PINNED_VERSION;
    let oldest = format!("v{}.{}", gate.major, gate.minor);
    let Some(found) = parse_version(version) else {
        return Err(Error::Setting {
            name: FLAG,
            why: format!("{version} is not a release version, such as {oldest}"),
        });
    };
    if (found.major, found.minor) < (gate.major, gate.minor) {
        return Err(Error::Setting {
            name: FLAG,
            why: format!(
                "{version} is older than {oldest}, the oldest release the provider drives (ADR-0061 Decision 5)"
            ),
        });
    }
    Ok(())
}

/// A version or tag becomes a directory under `/opt/banlieue`: a plain
/// name, nothing that could leave it.
fn check_dir_name(flag: &'static str, name: &str) -> Result<(), Error> {
    let plain = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if plain {
        return Ok(());
    }
    Err(Error::Setting {
        name: flag,
        why: format!("{name:?}: letters, digits, '.', '_' and '-' only, not starting with '.'"),
    })
}

fn check_url(flag: &'static str, url: &str) -> Result<String, Error> {
    let ok = url.len() > HTTPS_SCHEME.len()
        && url.starts_with(HTTPS_SCHEME)
        && !url.chars().any(|c| c.is_whitespace() || c.is_control());
    if ok {
        return Ok(url.to_string());
    }
    Err(Error::Setting {
        name: flag,
        why: format!("{url:?}: downloads are HTTPS only"),
    })
}

fn check_sha256(flag: &'static str, digest: &str) -> Result<String, Error> {
    let ok = digest.len() == SHA256_HEX_LEN && digest.chars().all(|c| c.is_ascii_hexdigit());
    if ok {
        return Ok(digest.to_ascii_lowercase());
    }
    Err(Error::Setting {
        name: flag,
        why: format!("{digest:?} is not a sha256 (64 hex digits)"),
    })
}

#[cfg(test)]
#[path = "release_tests.rs"]
mod release_tests;
