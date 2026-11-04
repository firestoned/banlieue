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
//! machine. The document itself is rendered by the provider SDK, shared with
//! libvirt and Cloud Hypervisor (ADR-0114).

use banlieue_api::infrastructure::ProxmoxNicSpec;
use banlieue_provider_sdk::cloudinit::{self, SeedNic};

/// Proxmox's registered OUI (`BC:24:11`), so derived addresses look native.
const MAC_PREFIX: &str = "bc:24:11";

/// A stable MAC for `nic_name` on the machine with `uid`, lower-case.
#[must_use]
pub fn derive_mac(uid: &str, nic_name: &str) -> String {
    cloudinit::derive_mac(MAC_PREFIX, uid, nic_name)
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

/// The `network-config` document, or `None` when no NIC is static (cloud-init
/// then applies its own DHCP default). `nics` must come from
/// [`effective_nics`] so every NIC has a MAC to match on. Rendered by the
/// shared [`cloudinit::network_config`] (ADR-0114).
#[must_use]
pub fn network_config(nics: &[ProxmoxNicSpec]) -> Option<String> {
    let seed: Vec<SeedNic<'_>> = nics
        .iter()
        .map(|n| SeedNic {
            name: &n.name,
            mac: n.mac_address.as_deref(),
            ipam: &n.ipam,
        })
        .collect();
    cloudinit::network_config(&seed)
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod network_tests;
