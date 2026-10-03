// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The pinned VMM release (ADR-0067 Decision 4), and the layout of any
//! release `install` places (ADR-0084).
//!
//! The pinned version must equal the release the client's vendored REST
//! spec is pinned to (ADR-0061 Decision 2); `pins_tests.rs` asserts it.
//! Changing the pin is a code change, with new digests and that test. Any
//! other version is chosen per host and resolved in `release.rs`.

use crate::paths::{BIN_DIR, OPT_ROOT};
use std::path::PathBuf;

/// The Cloud Hypervisor release.
pub const VMM_VERSION: &str = "v53.0";
/// sha256 of that release's `cloud-hypervisor-static`.
pub const VMM_SHA256: &str = "448af3d4e59b22c2987f7df94c213ad40fb53a10d437e42b5ee6c4fce7c29ecc";
/// sha256 of that release's `ch-remote-static`.
pub const CH_REMOTE_SHA256: &str =
    "13f32ba952e6791fd901f2279be2055fbacc64005f96c42a8e90d58860df84a7";
/// The Cloud Hypervisor edk2 firmware release.
pub const FIRMWARE_TAG: &str = "ch-97eeb7b09";
/// sha256 of that release's `CLOUDHV.fd`.
pub const FIRMWARE_SHA256: &str =
    "dc2fc8f0e43b96712d9fccc52a3a590769606412b3e1bc911d217addd3bef624";

/// The GitHub organization both upstream repositories live in.
pub const UPSTREAM_ORG: &str = "cloud-hypervisor";
/// The repository releasing the VMM and `ch-remote`.
pub const VMM_REPO: &str = "cloud-hypervisor";
/// The repository releasing the firmware.
pub const FIRMWARE_REPO: &str = "edk2";
/// The VMM's release asset.
pub const VMM_ASSET: &str = "cloud-hypervisor-static";
/// `ch-remote`'s release asset.
pub const CH_REMOTE_ASSET: &str = "ch-remote-static";
/// The firmware's release asset.
pub const FIRMWARE_ASSET: &str = "CLOUDHV.fd";

const GITHUB: &str = "https://github.com";

/// Executables.
const MODE_EXECUTABLE: u32 = 0o755;
/// Firmware: read by every guest's VMM.
const MODE_FIRMWARE: u32 = 0o644;

/// One file to install: where it comes from, what it must hash to, where
/// it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    /// The upstream release asset's name, also its name in `--artifacts-dir`.
    pub name: &'static str,
    /// Where to download it.
    pub url: String,
    /// Its sha256, lowercase hex.
    pub sha256: String,
    /// Where it is installed.
    pub dest: PathBuf,
    /// Its mode once installed.
    pub mode: u32,
}

/// Where a file comes from and what it must hash to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Its URL.
    pub url: String,
    /// Its sha256, lowercase hex.
    pub sha256: String,
}

/// The github.com download URL of an upstream release asset.
#[must_use]
pub fn github_url(repo: &str, tag: &str, asset: &str) -> String {
    format!("{GITHUB}/{UPSTREAM_ORG}/{repo}/releases/download/{tag}/{asset}")
}

/// The versioned directory holding the VMM and `ch-remote`.
#[must_use]
pub fn vmm_dir(version: &str) -> PathBuf {
    PathBuf::from(OPT_ROOT)
        .join("cloud-hypervisor")
        .join(version)
}

/// The versioned directory holding the firmware.
#[must_use]
pub fn firmware_dir(tag: &str) -> PathBuf {
    PathBuf::from(OPT_ROOT).join("firmware").join(tag)
}

/// The installed firmware, as the host config names it.
#[must_use]
pub fn firmware_path(tag: &str) -> PathBuf {
    firmware_dir(tag).join(FIRMWARE_ASSET)
}

/// What the `vmm` stage installs and the self-test checks: the pinned
/// release, one chosen per host (`release.rs`), or a release of known bytes
/// in the unit tests (no test can produce bytes matching the real digests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// The VMM version, a directory name under `/opt/banlieue`.
    pub version: String,
    /// The firmware tag, a directory name under `/opt/banlieue`.
    pub firmware_tag: String,
    /// The files, fetched and verified before any is installed.
    pub artifacts: Vec<Artifact>,
    /// `(link, target)`, set once every artifact verified.
    pub symlinks: Vec<(PathBuf, PathBuf)>,
    /// The installed firmware.
    pub firmware: PathBuf,
    /// Its sha256.
    pub firmware_sha256: String,
}

impl Release {
    /// The layout of `version` and `firmware_tag`, from these sources.
    /// The caller has checked that both are safe directory names.
    #[must_use]
    pub fn new(
        version: &str,
        firmware_tag: &str,
        vmm: Source,
        ch_remote: Source,
        firmware: Source,
    ) -> Self {
        let dir = vmm_dir(version);
        let firmware_file = firmware_path(firmware_tag);
        Self {
            version: version.into(),
            firmware_tag: firmware_tag.into(),
            artifacts: vec![
                Artifact {
                    name: VMM_ASSET,
                    url: vmm.url,
                    sha256: vmm.sha256,
                    dest: dir.join("cloud-hypervisor"),
                    mode: MODE_EXECUTABLE,
                },
                Artifact {
                    name: CH_REMOTE_ASSET,
                    url: ch_remote.url,
                    sha256: ch_remote.sha256,
                    dest: dir.join("ch-remote"),
                    mode: MODE_EXECUTABLE,
                },
                Artifact {
                    name: FIRMWARE_ASSET,
                    url: firmware.url,
                    sha256: firmware.sha256.clone(),
                    dest: firmware_file.clone(),
                    mode: MODE_FIRMWARE,
                },
            ],
            symlinks: vec![
                (
                    PathBuf::from(BIN_DIR).join("cloud-hypervisor"),
                    dir.join("cloud-hypervisor"),
                ),
                (
                    PathBuf::from(BIN_DIR).join("ch-remote"),
                    dir.join("ch-remote"),
                ),
            ],
            firmware: firmware_file,
            firmware_sha256: firmware.sha256,
        }
    }

    /// The one pinned release (ADR-0067 Decision 4), from github.com.
    #[must_use]
    pub fn pinned() -> Self {
        Self::new(
            VMM_VERSION,
            FIRMWARE_TAG,
            Source {
                url: github_url(VMM_REPO, VMM_VERSION, VMM_ASSET),
                sha256: VMM_SHA256.into(),
            },
            Source {
                url: github_url(VMM_REPO, VMM_VERSION, CH_REMOTE_ASSET),
                sha256: CH_REMOTE_SHA256.into(),
            },
            Source {
                url: github_url(FIRMWARE_REPO, FIRMWARE_TAG, FIRMWARE_ASSET),
                sha256: FIRMWARE_SHA256.into(),
            },
        )
    }
}

/// Lowercase hex sha256 of `data`.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
#[path = "pins_tests.rs"]
mod pins_tests;
