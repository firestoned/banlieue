// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Live test of port isolation (ADR-0144 Decision 3) against a real kernel:
//! `sys::isolate_port` is an `RTM_SETLINK` netlink request, and only a kernel
//! can say whether it parses it and forwards accordingly.
//!
//! Three network namespaces, each joined by a veth pair to one bridge. Two
//! ports are isolated, one is not. Isolated to isolated must stop passing
//! frames; isolated to non-isolated must keep working.
//!
//! Needs root with `CAP_NET_ADMIN`, `ip` (iproute2) and `ping`; a privileged
//! container does:
//!
//! ```sh
//! docker run --rm --privileged -v "$PWD":/w -w /w <rust image with iproute2> \
//!   cargo test -p banlieue-provider-cloud-hypervisor --test live_isolate \
//!   -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d, so running it is an explicit request: a missing capability
//! or tool fails the test, it never passes quietly.

use banlieue_provider_cloud_hypervisor::sys;
use std::process::Command;

const BRIDGE: &str = "bchisobr";
/// Port name, namespace, address: two to isolate, one gateway-like.
const SANDBOX_A: (&str, &str, &str) = ("bchisoa", "bchns-a", "192.0.2.1/24");
const SANDBOX_B: (&str, &str, &str) = ("bchisob", "bchns-b", "192.0.2.2/24");
const GATEWAY: (&str, &str, &str) = ("bchisog", "bchns-g", "192.0.2.3/24");

fn ip(args: &[&str]) {
    let out = Command::new("ip")
        .args(args)
        .output()
        .expect("run `ip`: is iproute2 installed?");
    assert!(
        out.status.success(),
        "ip {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A port on the bridge, with its peer inside a fresh namespace.
fn attach((port, ns, addr): (&str, &str, &str)) {
    let peer = format!("{port}p");
    ip(&["netns", "add", ns]);
    ip(&["link", "add", port, "type", "veth", "peer", "name", &peer]);
    ip(&["link", "set", &peer, "netns", ns]);
    ip(&["link", "set", port, "master", BRIDGE]);
    ip(&["link", "set", port, "up"]);
    ip(&["-n", ns, "addr", "add", addr, "dev", &peer]);
    ip(&["-n", ns, "link", "set", &peer, "up"]);
    ip(&["-n", ns, "link", "set", "lo", "up"]);
}

/// Whether `from` can reach `to` (one ping, one second).
///
/// A missing `ping` is a failure of the rig, not an answer: without this check
/// the baseline would fail with a misleading "A does not reach B".
fn reaches(from: &str, to: &str) -> bool {
    let out = Command::new("ip")
        .args(["netns", "exec", from, "ping", "-c1", "-W1", to])
        .output()
        .expect("run ping in a namespace");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("exec of") && !stderr.contains("No such file"),
        "`ping` is not installed: {stderr}"
    );
    out.status.success()
}

fn cleanup() {
    for (port, ns, _) in [SANDBOX_A, SANDBOX_B, GATEWAY] {
        let _ = Command::new("ip").args(["netns", "del", ns]).output();
        let _ = Command::new("ip").args(["link", "del", port]).output();
    }
    let _ = Command::new("ip").args(["link", "del", BRIDGE]).output();
}

fn address<'a>(spec: (&'a str, &'a str, &'a str)) -> &'a str {
    spec.2.split('/').next().expect("an address")
}

#[test]
#[ignore = "needs root/CAP_NET_ADMIN and iproute2: see the module docs"]
fn an_isolated_port_cannot_reach_another_isolated_port_but_reaches_a_plain_one() {
    cleanup();
    ip(&["link", "add", BRIDGE, "type", "bridge"]);
    ip(&["link", "set", BRIDGE, "up"]);
    for spec in [SANDBOX_A, SANDBOX_B, GATEWAY] {
        attach(spec);
    }

    // Baseline: nothing isolated, everything talks.
    let before = reaches(SANDBOX_A.1, address(SANDBOX_B));
    assert!(before, "baseline: A reaches B before isolation");

    sys::isolate_port(SANDBOX_A.0).expect("isolate A");
    sys::isolate_port(SANDBOX_B.0).expect("isolate B");
    sys::isolate_port(SANDBOX_A.0).expect("isolating again is success");

    let a_to_b = reaches(SANDBOX_A.1, address(SANDBOX_B));
    let b_to_a = reaches(SANDBOX_B.1, address(SANDBOX_A));
    let a_to_g = reaches(SANDBOX_A.1, address(GATEWAY));
    let b_to_g = reaches(SANDBOX_B.1, address(GATEWAY));
    let g_to_a = reaches(GATEWAY.1, address(SANDBOX_A));
    cleanup();

    assert!(!a_to_b, "isolated A must not reach isolated B");
    assert!(!b_to_a, "isolated B must not reach isolated A");
    assert!(a_to_g && b_to_g, "isolated ports reach the plain port");
    assert!(g_to_a, "the plain port reaches an isolated one");
}

#[test]
#[ignore = "needs root/CAP_NET_ADMIN and iproute2: see the module docs"]
fn isolating_an_interface_that_is_not_a_bridge_port_is_refused() {
    cleanup();
    ip(&["link", "add", "bchisox", "type", "dummy"]);
    let refused = sys::isolate_port("bchisox");
    let _ = Command::new("ip").args(["link", "del", "bchisox"]).output();
    assert!(refused.is_err());
}
