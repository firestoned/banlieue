// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `cloudinit/network.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::common::{IpamSpec, StaticIpamConfig};

    const PREFIX: &str = "52:54:00";

    fn dhcp() -> IpamSpec {
        IpamSpec::default()
    }

    fn static_ipam() -> IpamSpec {
        IpamSpec {
            static_: Some(StaticIpamConfig {
                address: "192.0.2.10".to_string(),
                prefix: 24,
                gateway: Some("192.0.2.1".to_string()),
                nameservers: vec!["192.0.2.53".to_string()],
                domain: Some("bar.foo.io".to_string()),
            }),
            pool: None,
        }
    }

    fn nic<'a>(name: &'a str, mac: Option<&'a str>, ipam: &'a IpamSpec) -> SeedNic<'a> {
        SeedNic { name, mac, ipam }
    }

    #[test]
    fn a_derived_mac_carries_the_prefix_and_is_lowercase() {
        let mac = derive_mac(PREFIX, "uid-1", "eth0");
        assert!(mac.starts_with("52:54:00:"), "{mac}");
        assert_eq!(mac.len(), 17);
        assert_eq!(mac, mac.to_lowercase());
    }

    #[test]
    fn a_derived_mac_is_stable_and_depends_on_uid_and_nic() {
        assert_eq!(
            derive_mac(PREFIX, "u", "eth0"),
            derive_mac(PREFIX, "u", "eth0")
        );
        assert_ne!(
            derive_mac(PREFIX, "u", "eth0"),
            derive_mac(PREFIX, "u", "eth1")
        );
        assert_ne!(
            derive_mac(PREFIX, "u", "eth0"),
            derive_mac(PREFIX, "v", "eth0")
        );
    }

    /// The suffix is the same whatever the prefix, so the Proxmox provider's
    /// derived MACs are unchanged by moving the function here.
    #[test]
    fn the_suffix_does_not_depend_on_the_prefix() {
        let a = derive_mac("bc:24:11", "uid-1", "eth0");
        let b = derive_mac(PREFIX, "uid-1", "eth0");
        assert_eq!(a[8..], b[8..]);
    }

    #[test]
    fn any_static_is_false_for_dhcp_only() {
        let d = dhcp();
        assert!(!any_static(&[nic("eth0", None, &d)]));
        let s = static_ipam();
        assert!(any_static(&[nic("eth0", None, &d), nic("eth1", None, &s)]));
    }

    #[test]
    fn no_static_nic_means_no_network_config() {
        let d = dhcp();
        assert_eq!(
            network_config(&[nic("eth0", Some("52:54:00:00:00:01"), &d)]),
            None
        );
        assert_eq!(network_config(&[]), None);
    }

    #[test]
    fn a_static_nic_renders_address_route_and_dns() {
        let s = static_ipam();
        let doc = network_config(&[nic("eth0", Some("52:54:00:aa:bb:cc"), &s)]).unwrap();
        assert!(doc.starts_with("version: 2\nethernets:\n"), "{doc}");
        assert!(doc.contains("  'eth0':\n"), "{doc}");
        assert!(
            doc.contains("      macaddress: '52:54:00:aa:bb:cc'\n"),
            "{doc}"
        );
        assert!(doc.contains("    set-name: 'eth0'\n"), "{doc}");
        assert!(doc.contains("      - '192.0.2.10/24'\n"), "{doc}");
        assert!(doc.contains("        via: '192.0.2.1'\n"), "{doc}");
        assert!(doc.contains("addresses: ['192.0.2.53']"), "{doc}");
        assert!(doc.contains("search: ['bar.foo.io']"), "{doc}");
    }

    #[test]
    fn a_dhcp_nic_beside_a_static_one_gets_dhcp4() {
        let s = static_ipam();
        let d = dhcp();
        let doc = network_config(&[
            nic("eth0", Some("52:54:00:00:00:01"), &s),
            nic("eth1", Some("52:54:00:00:00:02"), &d),
        ])
        .unwrap();
        assert!(doc.contains("  'eth1':\n"), "{doc}");
        assert!(doc.contains("    dhcp4: true\n"), "{doc}");
    }

    #[test]
    fn quotes_in_values_cannot_break_the_document() {
        let mut s = static_ipam();
        s.static_.as_mut().unwrap().domain = Some("x'\nevil: true".to_string());
        let doc = network_config(&[nic("e'th0", Some("52:54:00:00:00:01"), &s)]).unwrap();
        assert!(doc.contains("  'e''th0':\n"), "{doc}");
        assert!(!doc.contains("\nevil: true"), "a value added a key: {doc}");
    }

    /// ADR-0109 Decision 7: a sandbox NIC has an address and no route.
    #[test]
    fn a_static_nic_without_a_gateway_has_no_route() {
        let mut s = static_ipam();
        s.static_.as_mut().unwrap().gateway = None;
        let doc = network_config(&[nic("eth0", Some("52:54:00:00:00:01"), &s)]).unwrap();
        assert!(!doc.contains("routes:"), "{doc}");
        assert!(doc.contains("      - '192.0.2.10/24'\n"), "{doc}");
    }

    #[test]
    fn a_nic_without_a_mac_is_not_matched() {
        let s = static_ipam();
        let doc = network_config(&[nic("eth0", None, &s)]).unwrap();
        assert!(!doc.contains("match:"), "{doc}");
    }
}
