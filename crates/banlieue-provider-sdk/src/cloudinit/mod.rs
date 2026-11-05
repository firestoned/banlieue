// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! cloud-init NoCloud seed images (ADR-0054), shared by every provider that
//! delivers user-data as a disk: libvirt, Cloud Hypervisor and Proxmox. A
//! static NIC adds a `network-config` (ADR-0114, [`network`]).
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
pub mod network;

pub use iso9660::{IsoError, IsoFile, build_iso9660};
pub use network::{SeedNic, any_static, derive_mac, network_config};

/// The `user-data` a seed carries when the machine sets none: an empty
/// cloud-config, which changes nothing in the guest.
pub const EMPTY_USER_DATA: &str = "#cloud-config\n";

/// Name of the seed file carrying static addressing (ADR-0114).
pub const NETWORK_CONFIG_FILE: &str = "network-config";

/// Volume label cloud-init's NoCloud datasource matches on. Not
/// configurable: the datasource looks for exactly this.
pub const CIDATA_LABEL: &str = "CIDATA";

/// The files a NoCloud seed carries, for `domain_name` / `domain_uuid`.
///
/// `meta-data` and `user-data` are always present: cloud-init accepts a
/// NoCloud seed only when it has both, and ignores a seed missing either,
/// `network-config` and hostname included. With no user-data set, the seed
/// carries [`EMPTY_USER_DATA`]. (Found live on cloud-init 25.1: a seed with
/// `meta-data` and `network-config` but no `user-data` was rejected as "not
/// a valid seed" and the guest stayed on DHCP.)
/// `instance-id` comes from the domain UUID so that a rebuilt VM is a new
/// instance by construction: cloud-init keys its "have I run for this
/// instance already?" decision on that value, and a reused one makes a
/// fresh VM skip every per-instance module as though it had merely
/// rebooted (ADR-0054 Decision 5).
///
/// `network_config`, when set, is carried verbatim as `network-config`
/// (ADR-0114); build it with [`network_config`].
#[must_use]
pub fn seed_files(
    domain_name: &str,
    domain_uuid: &str,
    user_data: Option<&str>,
    network_config: Option<&str>,
) -> Vec<IsoFile> {
    let meta = format!("instance-id: {domain_uuid}\nlocal-hostname: {domain_name}\n");
    let ud = user_data.unwrap_or(EMPTY_USER_DATA);
    let mut files = vec![
        IsoFile::new("meta-data", meta.into_bytes()),
        IsoFile::new("user-data", ud.as_bytes().to_vec()),
    ];
    if let Some(nc) = network_config {
        files.push(IsoFile::new(NETWORK_CONFIG_FILE, nc.as_bytes().to_vec()));
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
    network_config: Option<&str>,
) -> Result<Vec<u8>, IsoError> {
    build_iso9660(
        CIDATA_LABEL,
        &seed_files(domain_name, domain_uuid, user_data, network_config),
    )
}

#[cfg(test)]
#[path = "seed_tests.rs"]
mod seed_tests;
