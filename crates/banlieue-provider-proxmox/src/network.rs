// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Guest network configuration for the NoCloud seed.
//!
//! Static addressing travels in a cloud-init `network-config` (v2) document
//! inside the seed ISO (ADR-0075 Decision 5). cloud-init matches each
//! interface by MAC, so a machine with a static NIC needs **known** MACs:
//! Proxmox otherwise generates one at config-write time. For those machines a
//! MAC is derived from the machine UID, the way the Cloud Hypervisor provider
//! does (ADR-0062), so it is stable across restarts and re-clones of the same
//! machine.

use banlieue_api::infrastructure::ProxmoxNicSpec;
use sha2::{Digest, Sha256};

/// Proxmox's registered OUI (`BC:24:11`), so derived addresses look native.
const MAC_PREFIX: &str = "bc:24:11";
/// Digest bytes appended after the OUI.
const MAC_SUFFIX_BYTES: usize = 3;

/// A stable MAC for `nic_name` on the machine with `uid`, lower-case.
#[must_use]
pub fn derive_mac(uid: &str, nic_name: &str) -> String {
    let digest = Sha256::digest(format!("{uid}/{nic_name}").as_bytes());
    let suffix: Vec<String> = digest[..MAC_SUFFIX_BYTES]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{MAC_PREFIX}:{}", suffix.join(":"))
}

/// The NICs as they will be configured: when any NIC is statically
/// addressed, every NIC without a MAC gets a derived one so the seed can name
/// it. DHCP-only machines are returned unchanged and keep Proxmox's own MACs.
#[must_use]
pub fn effective_nics(nics: &[ProxmoxNicSpec], uid: &str) -> Vec<ProxmoxNicSpec> {
    let any_static = nics.iter().any(|n| n.ipam.static_.is_some());
    nics.iter()
        .cloned()
        .map(|mut n| {
            if any_static && n.mac_address.is_none() {
                n.mac_address = Some(derive_mac(uid, &n.name));
            }
            n
        })
        .collect()
}

/// Single-quote `s` as a YAML scalar.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The `network-config` document, or `None` when no NIC is static (cloud-init
/// then applies its own DHCP default). `nics` must come from
/// [`effective_nics`] so every NIC has a MAC to match on.
#[must_use]
pub fn network_config(nics: &[ProxmoxNicSpec]) -> Option<String> {
    if !nics.iter().any(|n| n.ipam.static_.is_some()) {
        return None;
    }
    let mut doc = String::from("version: 2\nethernets:\n");
    for nic in nics {
        doc.push_str(&format!("  {}:\n", quote(&nic.name)));
        if let Some(mac) = &nic.mac_address {
            doc.push_str(&format!("    match:\n      macaddress: {}\n", quote(mac)));
            doc.push_str(&format!("    set-name: {}\n", quote(&nic.name)));
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
