// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The one rtnetlink request the provider makes: put a bridge port in kernel
//! port isolation (ADR-0144 Decision 3), the same as
//! `bridge link set dev <tap> isolated on`.
//!
//! Pure bytes in and out; the socket is `sys::isolate_port`. Written by hand
//! rather than with a netlink crate: it is one fixed 44-byte request and a
//! four-field reply, no new dependency, and it is not sysfs. The provider
//! unit mounts `/sys` read-only, so writing
//! `/sys/class/net/<tap>/brport/isolated` cannot work there, while a netlink
//! request needs only `CAP_NET_ADMIN`, which the provider holds.
//!
//! Constant names are the kernel's (`<linux/netlink.h>`, `<linux/rtnetlink.h>`,
//! `<linux/if_link.h>`); `libc` exports only `RTM_SETLINK`.

use std::io;

/// `RTM_SETLINK`: modify a link. The same value for the bridge family.
pub(crate) const RTM_SETLINK: u16 = libc::RTM_SETLINK;
/// `NLM_F_REQUEST`.
pub(crate) const NLM_F_REQUEST: u16 = 0x01;
/// `NLM_F_ACK`: reply with an `NLMSG_ERROR` even on success.
pub(crate) const NLM_F_ACK: u16 = 0x04;
/// `NLMSG_ERROR`: the reply type that carries an errno (0 for an ack).
pub(crate) const NLMSG_ERROR: u16 = 2;
/// `AF_BRIDGE` as the one-byte `ifi_family` of `struct ifinfomsg`.
pub(crate) const AF_BRIDGE: u8 = 7;
/// `IFLA_PROTINFO`: per-protocol data; for `AF_BRIDGE` the bridge port's.
pub(crate) const IFLA_PROTINFO: u16 = 12;
/// `NLA_F_NESTED`: the attribute's payload is more attributes.
pub(crate) const NLA_F_NESTED: u16 = 0x8000;
/// `IFLA_BRPORT_ISOLATED`: the port's isolation flag (a `u8`).
pub(crate) const IFLA_BRPORT_ISOLATED: u16 = 33;

/// `sizeof(struct nlmsghdr)`: length, type, flags, sequence, port id.
pub(crate) const NLMSG_HDRLEN: usize = 16;
/// `sizeof(struct ifinfomsg)`: family, pad, type, index, flags, change.
pub(crate) const IFINFOMSG_LEN: usize = 16;
/// `sizeof(struct nlattr)`: length and type.
pub(crate) const NLA_HDRLEN: usize = 4;
/// Netlink attributes and messages are padded to this many bytes.
pub(crate) const NLA_ALIGNTO: usize = 4;
/// Bytes of the payload this module sends: one flag byte.
const FLAG_LEN: usize = 1;
/// An `NLMSG_ERROR` reply: a header, an `i32` errno, then the echoed header.
const ERROR_ERRNO_AT: usize = NLMSG_HDRLEN;

fn align(len: usize) -> usize {
    len.div_ceil(NLA_ALIGNTO) * NLA_ALIGNTO
}

fn len_u16(len: usize) -> u16 {
    // Every length built here is a few dozen bytes.
    u16::try_from(len).unwrap_or(u16::MAX)
}

/// The `RTM_SETLINK` request that sets the isolation flag of the bridge port
/// with interface index `ifindex` to `isolated`, tagged with sequence `seq`.
///
/// The message is a netlink header, an `ifinfomsg` of family `AF_BRIDGE`
/// naming the port, and `IFLA_PROTINFO` (nested) holding one
/// `IFLA_BRPORT_ISOLATED` byte.
#[must_use]
pub(crate) fn set_port_isolated(seq: u32, ifindex: i32, isolated: bool) -> Vec<u8> {
    let inner_len = NLA_HDRLEN + FLAG_LEN;
    let nest_len = NLA_HDRLEN + align(inner_len);
    let total = NLMSG_HDRLEN + IFINFOMSG_LEN + nest_len;

    let mut m = Vec::with_capacity(total);
    // struct nlmsghdr
    m.extend_from_slice(&u32::try_from(total).unwrap_or(u32::MAX).to_ne_bytes());
    m.extend_from_slice(&RTM_SETLINK.to_ne_bytes());
    m.extend_from_slice(&(NLM_F_REQUEST | NLM_F_ACK).to_ne_bytes());
    m.extend_from_slice(&seq.to_ne_bytes());
    m.extend_from_slice(&0u32.to_ne_bytes()); // port id: the kernel assigns ours
    // struct ifinfomsg
    m.push(AF_BRIDGE);
    m.push(0); // pad
    m.extend_from_slice(&0u16.to_ne_bytes()); // ifi_type
    m.extend_from_slice(&ifindex.to_ne_bytes());
    m.extend_from_slice(&0u32.to_ne_bytes()); // ifi_flags
    m.extend_from_slice(&0u32.to_ne_bytes()); // ifi_change
    // IFLA_PROTINFO { IFLA_BRPORT_ISOLATED = isolated }
    m.extend_from_slice(&len_u16(nest_len).to_ne_bytes());
    m.extend_from_slice(&(IFLA_PROTINFO | NLA_F_NESTED).to_ne_bytes());
    m.extend_from_slice(&len_u16(inner_len).to_ne_bytes());
    m.extend_from_slice(&IFLA_BRPORT_ISOLATED.to_ne_bytes());
    m.push(u8::from(isolated));
    m.resize(total, 0); // pad the inner attribute to its alignment
    m
}

/// Read the kernel's reply to a request sent with `NLM_F_ACK`.
///
/// # Errors
/// The OS error the kernel reported (`-errno` in the reply), or
/// `InvalidData` for a reply that is short or is not an `NLMSG_ERROR`:
/// anything but an explicit ack is a failure, never silent success.
pub(crate) fn parse_ack(reply: &[u8]) -> io::Result<()> {
    let invalid = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_string());
    if reply.len() < ERROR_ERRNO_AT + 4 {
        return Err(invalid("netlink reply too short"));
    }
    let kind = u16::from_ne_bytes([reply[4], reply[5]]);
    if kind != NLMSG_ERROR {
        return Err(invalid("netlink reply is not an acknowledgement"));
    }
    let errno = i32::from_ne_bytes([
        reply[ERROR_ERRNO_AT],
        reply[ERROR_ERRNO_AT + 1],
        reply[ERROR_ERRNO_AT + 2],
        reply[ERROR_ERRNO_AT + 3],
    ]);
    if errno == 0 {
        return Ok(());
    }
    Err(io::Error::from_raw_os_error(errno.saturating_neg()))
}

#[cfg(test)]
#[path = "netlink_tests.rs"]
mod netlink_tests;
