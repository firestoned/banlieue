// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Where the guest is: Azure or not, and which Kairos boot mode.
//!
//! The same image boots on libvirt, Cloud Hypervisor or vSphere when a
//! `VMImage` has sources for several classes (ADR-0171 Decision 1), so every
//! action is gated on Azure's DMI chassis asset tag, the same gate the
//! cloud-config layer uses.

use std::fs;
use std::path::Path;

/// The DMI chassis asset tag every Azure VM carries (Hyper-V on Azure).
pub const AZURE_CHASSIS_ASSET_TAG: &str = "7783-7084-3265-9085-8269-3286-77";

/// Where the kernel exposes the chassis asset tag.
pub const CHASSIS_ASSET_TAG_PATH: &str = "/sys/class/dmi/id/chassis_asset_tag";

/// Directory holding Kairos's boot-mode marker files.
pub const KAIROS_MODE_DIR: &str = "/run/cos";

/// Marker of a boot from the active image.
pub const ACTIVE_MODE_FILE: &str = "active_mode";
/// Marker of a boot from the passive (fallback) image.
pub const PASSIVE_MODE_FILE: &str = "passive_mode";
/// Marker of a boot into recovery.
pub const RECOVERY_MODE_FILE: &str = "recovery_mode";

/// The DMI asset tag file's content names Azure.
///
/// The kernel pads the value with a newline (and some firmware with
/// spaces or NULs); those are trimmed, nothing else is.
#[must_use]
pub fn asset_tag_is_azure(content: &[u8]) -> bool {
    let trimmed = content
        .iter()
        .rposition(|b| !b.is_ascii_whitespace() && *b != 0)
        .map_or(&content[..0], |end| &content[..=end]);
    let start = trimmed
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(trimmed.len());
    &trimmed[start..] == AZURE_CHASSIS_ASSET_TAG.as_bytes()
}

/// True when the asset tag at `path` names Azure. An unreadable or absent
/// file is "not Azure": the helper then does nothing.
#[must_use]
pub fn is_azure(path: &Path) -> bool {
    fs::read(path).is_ok_and(|c| asset_tag_is_azure(&c))
}

/// The Kairos boot mode, from the marker files in `/run/cos`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMode {
    /// Booted from the installed active image.
    Active,
    /// Booted from the installed passive (fallback) image.
    Passive,
    /// Booted into recovery (a raw image's first boot, which resets itself).
    Recovery,
    /// No marker: a live medium, or not Kairos.
    Unknown,
}

impl BootMode {
    /// A boot from the installed disk, the only one that may report ready
    /// (ADR-0163 Decision 2).
    #[must_use]
    pub fn is_installed(self) -> bool {
        matches!(self, Self::Active | Self::Passive)
    }
}

/// The boot mode recorded under `dir`. Recovery wins over any other marker,
/// so a confused marker set never reports ready from recovery.
#[must_use]
pub fn boot_mode(dir: &Path) -> BootMode {
    if dir.join(RECOVERY_MODE_FILE).exists() {
        return BootMode::Recovery;
    }
    if dir.join(ACTIVE_MODE_FILE).exists() {
        return BootMode::Active;
    }
    if dir.join(PASSIVE_MODE_FILE).exists() {
        return BootMode::Passive;
    }
    BootMode::Unknown
}

#[cfg(test)]
#[path = "azure_tests.rs"]
mod azure_tests;
