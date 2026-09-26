// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `neigh.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::net::Ipv4Addr;

    /// The shape of `/proc/net/arp` on a Debian 13 host, with documentation
    /// and RFC 1918 addresses: a complete guest entry, an incomplete one, one
    /// on another interface, and an uppercase MAC.
    const TABLE: &str = "\
IP address       HW type     Flags       HW address            Mask     Device
192.168.122.106  0x1         0x2         52:54:00:c4:00:01     *        br0
192.168.122.107  0x1         0x0         00:00:00:00:00:00     *        br0
192.0.2.1        0x1         0x2         aa:bb:cc:dd:ee:ff     *        enp1s0
192.168.122.108  0x1         0x2         02:7D:4E:2A:9C:00     *        br0
";

    #[test]
    fn complete_entries_parse_and_incomplete_ones_are_dropped() {
        let n = parse_arp(TABLE);
        assert_eq!(n.len(), 3, "{n:?}");
        assert!(n.iter().all(|e| e.mac != "00:00:00:00:00:00"));
    }

    #[test]
    fn the_guest_address_is_found_by_mac_on_its_bridge() {
        let n = parse_arp(TABLE);
        assert_eq!(
            addresses_for(&n, "52:54:00:c4:00:01", "br0"),
            vec![Ipv4Addr::new(192, 168, 122, 106)]
        );
    }

    /// Derived MACs are lowercase; the kernel may print either case.
    #[test]
    fn mac_matching_ignores_case() {
        let n = parse_arp(TABLE);
        assert_eq!(
            addresses_for(&n, "02:7d:4e:2a:9c:00", "br0"),
            vec![Ipv4Addr::new(192, 168, 122, 108)]
        );
    }

    /// The same MAC on another interface is not this guest's NIC.
    #[test]
    fn an_entry_on_another_interface_does_not_count() {
        let n = parse_arp(TABLE);
        assert!(addresses_for(&n, "aa:bb:cc:dd:ee:ff", "br0").is_empty());
    }

    #[test]
    fn nothing_known_yet_is_an_empty_list() {
        assert!(addresses_for(&parse_arp(TABLE), "02:00:00:00:00:01", "br0").is_empty());
        assert!(parse_arp("").is_empty());
        assert!(parse_arp("IP address HW type Flags HW address Mask Device\n").is_empty());
    }

    #[test]
    fn malformed_rows_are_skipped_not_fatal() {
        let text = "header\nnot an arp row\n999.1.1.1 0x1 0x2 aa:bb:cc:dd:ee:ff * br0\n";
        assert!(parse_arp(text).is_empty());
    }

    #[test]
    fn the_real_table_is_readable_here() {
        assert!(read_arp(std::path::Path::new(PROC_NET_ARP)).is_ok());
    }
}
