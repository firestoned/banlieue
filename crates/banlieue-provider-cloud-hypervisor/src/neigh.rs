// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Guest addresses from the host's IPv4 neighbour table (ADR-0062 Decision 5).
//!
//! A DHCP guest's address is not known in advance, and there is no guest
//! agent to ask. The host learns it anyway: once the guest transmits, the
//! kernel records its MAC against its IP on the bridge. The roadmap 09 spike
//! confirmed this for a Cloud Hypervisor guest on a libvirt NAT bridge.
//!
//! `/proc/net/arp` is that table as text, so no netlink dependency is needed
//! (ADR-0062 named netlink). It is IPv4 only; IPv6 guests need the netlink
//! route: a follow-up, recorded in the changelog.

use std::net::Ipv4Addr;
use std::path::Path;

/// Where the kernel exposes the IPv4 neighbour table.
pub const PROC_NET_ARP: &str = "/proc/net/arp";
/// `ATF_COM`: the entry is complete (resolved), from `<net/if_arp.h>`.
const ATF_COM: u32 = 0x2;
/// Columns in a `/proc/net/arp` data row.
const ARP_COLUMNS: usize = 6;

/// One resolved neighbour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Neighbour {
    /// Its IPv4 address.
    pub ip: Ipv4Addr,
    /// Its MAC, lowercase.
    pub mac: String,
    /// The interface it was seen on (the bridge, for a guest).
    pub device: String,
}

/// Parse `/proc/net/arp` text, keeping complete entries only.
///
/// An incomplete entry (flags without `ATF_COM`) is an address the kernel
/// asked about and got no answer for; reporting it would publish an address
/// that does not belong to the guest.
#[must_use]
pub fn parse_arp(text: &str) -> Vec<Neighbour> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() != ARP_COLUMNS {
                return None;
            }
            let flags = u32::from_str_radix(cols[2].trim_start_matches("0x"), 16).ok()?;
            if flags & ATF_COM == 0 {
                return None;
            }
            Some(Neighbour {
                ip: cols[0].parse().ok()?,
                mac: cols[3].to_ascii_lowercase(),
                device: cols[5].to_string(),
            })
        })
        .collect()
}

/// The IPv4 addresses seen for `mac` on `bridge`, in table order, deduplicated.
#[must_use]
pub fn addresses_for(neighbours: &[Neighbour], mac: &str, bridge: &str) -> Vec<Ipv4Addr> {
    let mac = mac.to_ascii_lowercase();
    let mut out: Vec<Ipv4Addr> = Vec::new();
    for n in neighbours {
        if n.mac == mac && n.device == bridge && !out.contains(&n.ip) {
            out.push(n.ip);
        }
    }
    out
}

/// Read and parse the host's table.
///
/// # Errors
/// The I/O error from reading `path`.
pub fn read_arp(path: &Path) -> std::io::Result<Vec<Neighbour>> {
    Ok(parse_arp(&std::fs::read_to_string(path)?))
}

#[cfg(test)]
#[path = "neigh_tests.rs"]
mod neigh_tests;
