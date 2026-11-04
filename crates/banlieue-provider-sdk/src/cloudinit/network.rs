// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Guest network configuration for the NoCloud seed (ADR-0114).
//!
//! Static addressing travels in a cloud-init `network-config` (v2) document
//! inside the seed. cloud-init matches each interface by MAC, so a machine
//! with a static NIC needs **known** MACs: each provider derives one from the
//! machine UID ([`derive_mac`]) where the hypervisor would otherwise pick it.
//!
//! Kairos guests do not apply a `network-config` from the seed; they take
//! their address from the ADR-0024 user-data placeholders instead (ADR-0114
//! Decision 4). The document is harmless to them.
//!
//! Moved here from the Proxmox provider (ADR-0075 Decision 5), whose output
//! it keeps, so libvirt and Cloud Hypervisor render the same document.

use banlieue_api::common::IpamSpec;
use sha2::{Digest, Sha256};

/// Digest bytes appended after a MAC prefix.
const MAC_SUFFIX_BYTES: usize = 3;

/// One NIC as the seed sees it: its name, the MAC the guest will see (if
/// known) and its resolved IPAM.
#[derive(Clone, Copy, Debug)]
pub struct SeedNic<'a> {
    /// The NIC's spec name; the guest interface is renamed to it.
    pub name: &'a str,
    /// The guest MAC. Without one the NIC cannot be matched.
    pub mac: Option<&'a str>,
    /// Resolved IPAM: static, or DHCP.
    pub ipam: &'a IpamSpec,
}

/// A stable MAC for `nic_name` on the machine with `uid`: `prefix` (three
/// octets, e.g. `52:54:00`) and three bytes of a SHA-256 over
/// `<uid>/<nic_name>`, lower-case.
#[must_use]
pub fn derive_mac(prefix: &str, uid: &str, nic_name: &str) -> String {
    let digest = Sha256::digest(format!("{uid}/{nic_name}").as_bytes());
    let suffix: Vec<String> = digest[..MAC_SUFFIX_BYTES]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{}:{}", prefix.to_lowercase(), suffix.join(":"))
}

/// Whether any NIC is statically addressed, and so needs a `network-config`
/// and known MACs.
#[must_use]
pub fn any_static(nics: &[SeedNic<'_>]) -> bool {
    nics.iter().any(|n| n.ipam.static_.is_some())
}

/// Single-quote `s` as a YAML scalar. A line break is replaced by a space:
/// inside a quoted scalar YAML would fold it anyway, and a value that spans
/// lines is never a valid address, name or domain.
fn quote(s: &str) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    format!("'{}'", flat.replace('\'', "''"))
}

/// The `network-config` (v2) document, or `None` when no NIC is static
/// (cloud-init then applies its own DHCP default and the seed carries no
/// such file).
///
/// Each NIC with a MAC is matched by it and renamed to its spec name. A
/// static NIC gets its address and prefix, a default route only when it has
/// a gateway (a sandbox NIC has none, ADR-0109 Decision 7), and its
/// nameservers and search domain; every other NIC gets `dhcp4: true`.
/// Every value is single-quoted, so no value can add a key.
#[must_use]
pub fn network_config(nics: &[SeedNic<'_>]) -> Option<String> {
    if !any_static(nics) {
        return None;
    }
    let mut doc = String::from("version: 2\nethernets:\n");
    for nic in nics {
        doc.push_str(&format!("  {}:\n", quote(nic.name)));
        if let Some(mac) = nic.mac {
            doc.push_str(&format!("    match:\n      macaddress: {}\n", quote(mac)));
            doc.push_str(&format!("    set-name: {}\n", quote(nic.name)));
        }
        let Some(cfg) = &nic.ipam.static_ else {
            doc.push_str("    dhcp4: true\n");
            continue;
        };
        doc.push_str(&format!(
            "    addresses:\n      - {}\n",
            quote(&format!("{}/{}", cfg.address, cfg.prefix))
        ));
        if let Some(gw) = &cfg.gateway {
            doc.push_str(&format!(
                "    routes:\n      - to: default\n        via: {}\n",
                quote(gw)
            ));
        }
        let dns: Vec<String> = cfg.nameservers.iter().map(|n| quote(n)).collect();
        let search: Vec<String> = cfg.domain.iter().map(|d| quote(d)).collect();
        if dns.is_empty() && search.is_empty() {
            continue;
        }
        doc.push_str("    nameservers:\n");
        if !dns.is_empty() {
            doc.push_str(&format!("      addresses: [{}]\n", dns.join(", ")));
        }
        if !search.is_empty() {
            doc.push_str(&format!("      search: [{}]\n", search.join(", ")));
        }
    }
    Some(doc)
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod network_tests;
