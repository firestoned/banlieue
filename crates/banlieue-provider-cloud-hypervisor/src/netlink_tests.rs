// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `netlink.rs`: the bytes of the request, and the reading of
//! the kernel's reply. Whether a kernel accepts them is `tests/live_isolate.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    fn u16_at(b: &[u8], at: usize) -> u16 {
        u16::from_ne_bytes([b[at], b[at + 1]])
    }
    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_ne_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }

    #[test]
    fn the_request_is_a_setlink_with_the_bridge_family_and_the_port_index() {
        let m = set_port_isolated(7, 42, true);
        assert_eq!(usize::try_from(u32_at(&m, 0)).unwrap(), m.len());
        assert_eq!(u16_at(&m, 4), RTM_SETLINK);
        assert_eq!(u16_at(&m, 6), NLM_F_REQUEST | NLM_F_ACK);
        assert_eq!(u32_at(&m, 8), 7, "sequence number");
        assert_eq!(m[NLMSG_HDRLEN], AF_BRIDGE);
        assert_eq!(
            i32::from_ne_bytes(m[NLMSG_HDRLEN + 4..NLMSG_HDRLEN + 8].try_into().unwrap()),
            42
        );
    }

    #[test]
    fn the_isolation_flag_is_one_byte_inside_a_nested_protinfo_attribute() {
        let on = set_port_isolated(1, 2, true);
        let off = set_port_isolated(1, 2, false);
        let nest = NLMSG_HDRLEN + IFINFOMSG_LEN;
        assert_eq!(u16_at(&on, nest + 2), IFLA_PROTINFO | NLA_F_NESTED);
        let inner = nest + NLA_HDRLEN;
        assert_eq!(u16_at(&on, inner + 2), IFLA_BRPORT_ISOLATED);
        assert_eq!(on[inner + NLA_HDRLEN], 1);
        assert_eq!(off[inner + NLA_HDRLEN], 0);
        assert_eq!(on.len() % NLA_ALIGNTO, 0, "the message stays 4-aligned");
    }

    #[test]
    fn the_kernel_attribute_numbers_match_linux_if_link_h() {
        // Pinned to the kernel's UAPI enum, not to our own constants: a test
        // that compares a constant with itself missed IFLA_BRPORT_ISOLATED
        // being 34 (BACKUP_PORT, a u32, so the kernel answered ERANGE) when
        // it is 33. Counted from IFLA_BRPORT_UNSPEC = 0 in <linux/if_link.h>.
        assert_eq!(IFLA_PROTINFO, 12);
        assert_eq!(IFLA_BRPORT_ISOLATED, 33);
    }

    #[test]
    fn the_nested_attribute_length_runs_to_the_end_of_the_message() {
        let m = set_port_isolated(1, 2, true);
        let nest = NLMSG_HDRLEN + IFINFOMSG_LEN;
        assert_eq!(nest + usize::from(u16_at(&m, nest)), m.len());
        let inner = nest + NLA_HDRLEN;
        assert_eq!(usize::from(u16_at(&m, inner)), NLA_HDRLEN + 1);
    }

    fn ack(error: i32) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend_from_slice(
            &u32::try_from(NLMSG_HDRLEN + 4 + NLMSG_HDRLEN)
                .unwrap()
                .to_ne_bytes(),
        );
        r.extend_from_slice(&NLMSG_ERROR.to_ne_bytes());
        r.extend_from_slice(&0u16.to_ne_bytes());
        r.extend_from_slice(&[0; 8]);
        r.extend_from_slice(&error.to_ne_bytes());
        r.extend_from_slice(&[0; NLMSG_HDRLEN]);
        r
    }

    #[test]
    fn an_ack_with_error_zero_is_success() {
        assert!(parse_ack(&ack(0)).is_ok());
    }

    #[test]
    fn a_negative_errno_in_the_ack_is_that_os_error() {
        let e = parse_ack(&ack(-libc::EOPNOTSUPP)).expect_err("refused");
        assert_eq!(e.raw_os_error(), Some(libc::EOPNOTSUPP));
    }

    #[test]
    fn a_short_or_foreign_reply_is_an_error_not_success() {
        assert!(parse_ack(&[]).is_err());
        assert!(parse_ack(&[0; 8]).is_err());
        let mut other = ack(0);
        other[4..6].copy_from_slice(&3u16.to_ne_bytes());
        assert!(parse_ack(&other).is_err());
    }
}
