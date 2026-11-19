// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Helpers the Cloud Hypervisor e2e suites share: required settings, polling,
//! and finding what a machine left on the host.

// Each test binary compiles this module in full but uses only the parts it
// needs; without this, every binary warns about the rest.
#![allow(dead_code)]

use std::path::Path;
use std::time::{Duration, Instant};

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;

/// Tap names are `bch` + the first 10 hex digits of the machine UID + NIC
/// (`crate::plan`'s tap naming).
const TAP_PREFIX: &str = "bch";
const TAP_UID_DIGITS: usize = 10;

/// The environment variable `name`, or a panic that names the suite whose
/// module docs say how to set it.
pub fn required(name: &str) -> String {
    // The first path segment is the test binary: `e2e_machine::common`.
    let suite = module_path!().split("::").next().unwrap_or_default();
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is not set; see the module docs of tests/{suite}.rs"))
}

/// Poll `check` every `poll` until it returns `Some`, or fail naming `what`.
pub async fn wait_for<T, F, Fut>(poll: Duration, what: &str, timeout: Duration, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = check().await {
            return v;
        }
        assert!(
            start.elapsed() < timeout,
            "timed out after {timeout:?} waiting for {what}"
        );
        tokio::time::sleep(poll).await;
    }
}

/// [`wait_for`] that prints each wait as it starts and as it ends, for the
/// suites whose runs are long enough to need a progress log.
pub async fn wait_for_logged<T, F, Fut>(
    poll: Duration,
    what: &str,
    timeout: Duration,
    mut check: F,
) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = Instant::now();
    eprintln!("  … {what}");
    loop {
        if let Some(v) = check().await {
            eprintln!("  ✓ {what} ({}s)", start.elapsed().as_secs());
            return v;
        }
        assert!(
            start.elapsed() < timeout,
            "timed out after {}s waiting for: {what}",
            timeout.as_secs()
        );
        tokio::time::sleep(poll).await;
    }
}

/// Whether the condition of type `kind` is `True`.
pub fn condition_true(conditions: &[Condition], kind: &str) -> bool {
    conditions
        .iter()
        .any(|c| c.type_ == kind && c.status == "True")
}

/// Host interfaces that are this machine's taps.
pub fn taps_for(uid: &str) -> Vec<String> {
    let hex: String = uid
        .chars()
        .filter(|c| *c != '-')
        .take(TAP_UID_DIGITS)
        .collect();
    let prefix = format!("{TAP_PREFIX}{hex}");
    std::fs::read_dir("/sys/class/net")
        .expect("reading /sys/class/net")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with(&prefix))
        .collect()
}

/// Whether `p` exists, without needing to read inside it (the directories
/// are `2770 guest:banlieue`; their `0711` parents let anyone stat them).
pub fn present(p: &Path) -> bool {
    stat(p, "")
}

/// Whether anything is at `p`. Permission errors fail: the suite runs as
/// root, and a check it cannot make proves nothing.
pub fn present_as_root(p: &Path) -> bool {
    stat(p, " (run as root: see the module docs)")
}

fn stat(p: &Path, hint: &str) -> bool {
    match std::fs::symlink_metadata(p) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("stat {}{hint}: {e}", p.display()),
    }
}
