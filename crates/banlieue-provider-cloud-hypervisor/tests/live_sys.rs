// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Live test of the tap and bridge syscalls in `sys.rs`.
//!
//! Needs `CAP_NET_ADMIN`, which a user + network namespace grants without
//! root and without touching the host's real interfaces. A mount namespace
//! with sysfs remounted is needed too: `/sys/class/net` otherwise still
//! shows the host's interfaces, not the new namespace's.
//!
//! ```sh
//! unshare --user --map-root-user --net --mount sh -c \
//!   'mount -t sysfs sysfs /sys && cargo test -p banlieue-provider-cloud-hypervisor --test live_sys -- --ignored'
//! ```
//!
//! `#[ignore]`d, so running it is an explicit request: without the
//! capability it fails, it does not pass quietly.

#![deny(clippy::undocumented_unsafe_blocks)]

use banlieue_provider_cloud_hypervisor::sys;
use std::os::fd::AsRawFd;

/// `SIOCBRADDBR` from `<linux/sockios.h>`: create a bridge. Test-only; the
/// provider never creates bridges (the host owner does, see the guide).
const SIOCBRADDBR: libc::c_ulong = 0x89a0;
const BRIDGE: &str = "bchtestbr";
const TAP: &str = "bchtest0";

fn create_bridge(name: &str) {
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").expect("socket");
    let c = std::ffi::CString::new(name).unwrap();
    // SAFETY: `sock` is a live socket; SIOCBRADDBR takes a pointer to a
    // NUL-terminated name, and `c` is one, alive for the call.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let rc = unsafe { libc::ioctl(sock.as_raw_fd(), SIOCBRADDBR, c.as_ptr()) };
    assert_eq!(rc, 0, "SIOCBRADDBR: {}", std::io::Error::last_os_error());
}

#[test]
#[ignore = "needs CAP_NET_ADMIN and a fresh sysfs: see the module docs"]
fn a_tap_is_created_owned_joined_to_a_bridge_brought_up_and_deleted() {
    // Loopback must be up for the UDP socket above; a fresh netns has it down.
    sys::set_link_up("lo").expect("bring lo up (are we in a netns with CAP_NET_ADMIN?)");
    create_bridge(BRIDGE);
    sys::set_link_up(BRIDGE).unwrap();

    // Create, idempotently.
    sys::create_persistent_tap(TAP, Some(sys::effective_uid())).expect("create tap");
    sys::create_persistent_tap(TAP, Some(sys::effective_uid())).expect("create again");
    assert!(sys::interface_exists(TAP));

    // Join the bridge, idempotently, and come up.
    sys::add_to_bridge(BRIDGE, TAP).expect("enslave");
    sys::add_to_bridge(BRIDGE, TAP).expect("enslave again");
    assert_eq!(sys::master_of(TAP).as_deref(), Some(BRIDGE));
    sys::set_link_up(TAP).expect("link up");

    // The owner is what lets an unprivileged VMM open it.
    let owner = std::fs::read_to_string(format!("/sys/class/net/{TAP}/owner")).unwrap();
    assert_eq!(owner.trim(), sys::effective_uid().to_string());

    // Delete, and verify rather than assume (the ADR-0050 lesson).
    sys::delete_persistent_tap(TAP).expect("delete tap");
    assert!(
        !sys::interface_exists(TAP),
        "tap still present after delete"
    );
    sys::delete_persistent_tap(TAP).expect("delete absent tap");
}

#[test]
#[ignore = "needs CAP_NET_ADMIN and a fresh sysfs: see the module docs"]
fn joining_a_missing_port_says_not_found() {
    sys::set_link_up("lo").unwrap();
    let err = sys::add_to_bridge("bchnobridge", "bchnoport").unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
}

/// The first live run found this: re-attaching to a tap on every pass
/// fails with EBUSY once the VMM holds it (a single-queue tap takes one
/// queue). `ensure_tap` creates only when absent and never attaches to an
/// existing tap, so a running guest's queue is left alone.
#[test]
#[ignore = "needs CAP_NET_ADMIN and a fresh sysfs: see the module docs"]
fn ensuring_a_tap_the_vmm_holds_leaves_its_queue_alone() {
    const TAP_HELD: &str = "bchtest1";
    const BRIDGE_HELD: &str = "bchtestbr1";
    sys::set_link_up("lo").unwrap();
    create_bridge(BRIDGE_HELD);
    sys::set_link_up(BRIDGE_HELD).unwrap();

    sys::ensure_tap(TAP_HELD, Some(sys::effective_uid()), BRIDGE_HELD).expect("first ensure");
    assert_eq!(sys::master_of(TAP_HELD).as_deref(), Some(BRIDGE_HELD));

    // Play the VMM: attach a queue, as Cloud Hypervisor does, and hold it.
    let vmm = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
        .unwrap();
    // SAFETY: `ifreq` is plain old data; all-zero is a valid value.
    // SAFETY: `ifreq` is plain old data (arrays and a union of integers,
    // pointers and sockaddrs); all-zero is a valid value of every field.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let mut req: libc::ifreq = unsafe { std::mem::zeroed() };
    assert!(
        TAP_HELD.len() < libc::IFNAMSIZ,
        "name must leave room for NUL"
    );
    for (d, s) in req.ifr_name.iter_mut().zip(TAP_HELD.bytes()) {
        *d = s as libc::c_char;
    }
    req.ifr_ifru.ifru_flags =
        (libc::IFF_TAP | libc::IFF_NO_PI | libc::IFF_VNET_HDR) as libc::c_short;
    // SAFETY: `vmm` is an open /dev/net/tun; TUNSETIFF takes a pointer to
    // a `struct ifreq`, and `req` is one we own exclusively for the call.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let rc = unsafe { libc::ioctl(vmm.as_raw_fd(), libc::TUNSETIFF, &raw mut req) };
    assert_eq!(rc, 0, "VMM attach: {}", std::io::Error::last_os_error());
    let carrier = || std::fs::read_to_string(format!("/sys/class/net/{TAP_HELD}/carrier")).unwrap();
    assert_eq!(carrier().trim(), "1");

    // A later reconcile pass.
    sys::ensure_tap(TAP_HELD, Some(sys::effective_uid()), BRIDGE_HELD).expect("ensure while held");
    assert_eq!(carrier().trim(), "1", "the VMM's queue must survive");

    drop(vmm);
    sys::delete_persistent_tap(TAP_HELD).unwrap();
    assert!(!sys::interface_exists(TAP_HELD));
}
