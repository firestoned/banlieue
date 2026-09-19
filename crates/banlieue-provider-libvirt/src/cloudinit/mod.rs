// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! cloud-init NoCloud seed images (ADR-0054).
//!
//! vSphere delivers user-data through `guestinfo.userdata`, a hypervisor
//! channel with no filesystem involved. libvirt has no such channel, so the
//! payload has to arrive as a disk — a small ISO labelled `CIDATA` holding
//! `user-data` and `meta-data` at its root.
//!
//! Built in-process rather than by shelling out to `genisoimage`: the
//! provider image is distroless, and a subprocess would need the payload on
//! disk to hand it a path, writing the guest's bootstrap credentials into
//! the container's filesystem on every reconcile.

pub mod iso9660;

pub use iso9660::{IsoError, IsoFile, build_iso9660};

/// Volume label cloud-init's NoCloud datasource matches on. Not
/// configurable: the datasource looks for exactly this.
pub const CIDATA_LABEL: &str = "CIDATA";

/// The files a NoCloud seed carries, for `domain_name` / `domain_uuid`.
///
/// `meta-data` is always present — cloud-init requires it even when there
/// is no user-data, and a seed missing it is ignored rather than rejected.
/// `instance-id` comes from the domain UUID so that a rebuilt VM is a new
/// instance by construction: cloud-init keys its "have I run for this
/// instance already?" decision on that value, and a reused one makes a
/// fresh VM skip every per-instance module as though it had merely
/// rebooted (ADR-0054 Decision 5).
#[must_use]
pub fn seed_files(domain_name: &str, domain_uuid: &str, user_data: Option<&str>) -> Vec<IsoFile> {
    let meta = format!("instance-id: {domain_uuid}\nlocal-hostname: {domain_name}\n");
    let mut files = vec![IsoFile::new("meta-data", meta.into_bytes())];
    if let Some(ud) = user_data {
        files.push(IsoFile::new("user-data", ud.as_bytes().to_vec()));
    }
    files
}

/// Build the NoCloud seed image for one domain.
///
/// # Errors
/// [`IsoError`] if a name or the label cannot be represented — neither is
/// reachable with the fixed names this builds, but the writer is general
/// enough to say so.
pub fn build_seed_iso(
    domain_name: &str,
    domain_uuid: &str,
    user_data: Option<&str>,
) -> Result<Vec<u8>, IsoError> {
    build_iso9660(
        CIDATA_LABEL,
        &seed_files(domain_name, domain_uuid, user_data),
    )
}

#[cfg(test)]
#[path = "seed_tests.rs"]
mod seed_tests;
