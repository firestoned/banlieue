// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `network.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::common::{IpamSpec, StaticIpamConfig};
    use banlieue_api::infrastructure::{ProxmoxNicModel, ProxmoxNicSpec};

    fn nic(name: &str) -> ProxmoxNicSpec {
        ProxmoxNicSpec {
            name: name.to_string(),
            bridge: "vmbr0".to_string(),
            vlan: None,
            model: ProxmoxNicModel::default(),
            mac_address: None,
            ipam: IpamSpec::default(),
        }
    }

    fn static_nic(name: &str) -> ProxmoxNicSpec {
        let mut n = nic(name);
        n.ipam.static_ = Some(StaticIpamConfig {
            address: "192.0.2.10".to_string(),
            prefix: 24,
            gateway: Some("192.0.2.1".to_string()),
            nameservers: vec!["192.0.2.53".to_string()],
            domain: Some("bar.foo.io".to_string()),
        });
        n
    }

    #[test]
    fn a_derived_mac_is_in_the_proxmox_oui_and_lowercase() {
        let mac = derive_mac("uid-1", "eth0");
        assert!(mac.starts_with("bc:24:11:"), "{mac}");
        assert_eq!(mac.len(), 17);
        assert_eq!(mac, mac.to_lowercase());
    }

    #[test]
    fn a_derived_mac_is_stable_and_depends_on_uid_and_nic() {
        assert_eq!(derive_mac("u", "eth0"), derive_mac("u", "eth0"));
        assert_ne!(derive_mac("u", "eth0"), derive_mac("u", "eth1"));
        assert_ne!(derive_mac("u", "eth0"), derive_mac("v", "eth0"));
    }

    #[test]
    fn dhcp_only_machines_keep_proxmox_generated_macs() {
        let nics = effective_nics(&[nic("eth0")], "uid-1");
        assert_eq!(nics[0].mac_address, None);
    }

    #[test]
    fn a_static_machine_gets_a_mac_on_every_nic_lacking_one() {
        let mut fixed = nic("eth1");
        fixed.mac_address = Some("AA:BB:CC:00:00:01".to_string());
        let nics = effective_nics(&[static_nic("eth0"), fixed, nic("eth2")], "uid-1");
        assert_eq!(nics[0].mac_address, Some(derive_mac("uid-1", "eth0")));
        assert_eq!(nics[1].mac_address.as_deref(), Some("AA:BB:CC:00:00:01"));
        assert_eq!(nics[2].mac_address, Some(derive_mac("uid-1", "eth2")));
    }

    #[test]
    fn no_static_nic_means_no_network_config() {
        assert_eq!(network_config(&effective_nics(&[nic("eth0")], "u")), None);
    }

    #[test]
    fn a_static_nic_renders_address_route_and_dns() {
        let nics = effective_nics(&[static_nic("eth0")], "uid-1");
        let doc = network_config(&nics).unwrap();
        let mac = derive_mac("uid-1", "eth0");
        assert!(doc.starts_with("version: 2\nethernets:\n"), "{doc}");
        assert!(doc.contains("  'eth0':\n"), "{doc}");
        assert!(
            doc.contains(&format!("      macaddress: '{mac}'\n")),
            "{doc}"
        );
        assert!(doc.contains("      - '192.0.2.10/24'\n"), "{doc}");
        assert!(doc.contains("        via: '192.0.2.1'\n"), "{doc}");
        assert!(doc.contains("addresses: ['192.0.2.53']"), "{doc}");
        assert!(doc.contains("search: ['bar.foo.io']"), "{doc}");
    }

    #[test]
    fn a_dhcp_nic_beside_a_static_one_gets_dhcp4() {
        let nics = effective_nics(&[static_nic("eth0"), nic("eth1")], "uid-1");
        let doc = network_config(&nics).unwrap();
        assert!(doc.contains("  'eth1':\n"), "{doc}");
        assert!(doc.contains("    dhcp4: true\n"), "{doc}");
    }

    #[test]
    fn quotes_in_names_cannot_break_the_document() {
        let mut n = static_nic("e'th0");
        n.ipam.static_.as_mut().unwrap().domain = None;
        let doc = network_config(&effective_nics(&[n], "u")).unwrap();
        assert!(doc.contains("  'e''th0':\n"), "{doc}");
    }

    #[test]
    fn a_static_nic_without_a_gateway_has_no_route() {
        let mut n = static_nic("eth0");
        n.ipam.static_.as_mut().unwrap().gateway = None;
        let doc = network_config(&effective_nics(&[n], "u")).unwrap();
        assert!(!doc.contains("routes:"), "{doc}");
    }
}
