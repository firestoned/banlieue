// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Live test of template-unit instances against the calling user's systemd
//! manager.
//!
//! The provider starts instances of root-owned templates (ADR-0063,
//! amended); the user manager speaks the same `StartUnit` and
//! `SetUnitProperties` API as the system one, so this proves the D-Bus half
//! without root, with a throwaway template in the user's unit directory.
//! The real templates, their numeric `User=%i` and devices, need the system
//! manager and are exercised by `make ch-e2e` on a bootstrapped host.
//!
//! ```sh
//! cargo test -p banlieue-provider-cloud-hypervisor --test live_systemd -- --ignored --test-threads=1
//! ```

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use banlieue_provider_cloud_hypervisor::systemd::{Bus, Systemd, UnitStart, UnitState};

const POLLS: u32 = 50;
const POLL_STEP: Duration = Duration::from_millis(100);
const MIB: u64 = 1024 * 1024;
/// How long the slow unit's `ExecStop` takes: long enough that a `stop`
/// that returns before the stop job finishes is caught every time.
const SLOW_STOP: &str = "ExecStop=/usr/bin/sleep 3\n";

async fn wait_for(sd: &Systemd, name: &str, want: impl Fn(Option<&UnitState>) -> bool) {
    for _ in 0..POLLS {
        if want(sd.state(name).await.unwrap().as_ref()) {
            return;
        }
        tokio::time::sleep(POLL_STEP).await;
    }
    panic!("{name} never reached the expected state");
}

/// A template in the user's unit directory, removed on drop.
struct Template {
    path: PathBuf,
}

impl Template {
    fn install(name: &str, exec: &str) -> Self {
        Self::install_with(name, exec, "")
    }

    /// A template with `service` appended to its `[Service]` section.
    fn install_with(name: &str, exec: &str, service: &str) -> Self {
        let home = std::env::var("HOME").expect("HOME");
        let dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(home).join(".config"))
            .join("systemd/user");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}@.service"));
        std::fs::write(
            &path,
            format!("[Unit]\nCollectMode=inactive\n[Service]\nExecStart={exec}\n{service}"),
        )
        .unwrap();
        reload();
        Self { path }
    }
}

impl Drop for Template {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        reload();
    }
}

fn reload() {
    let ok = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "systemctl --user daemon-reload");
}

#[tokio::test]
#[ignore = "needs a systemd user session bus (DBUS_SESSION_BUS_ADDRESS)"]
async fn a_template_instance_starts_with_its_memory_ceiling_and_stops_without_a_trace() {
    let template = format!("banlieue-livetest-{}", std::process::id());
    let _t = Template::install(&template, "/usr/bin/sleep 300");
    let sd = Systemd::connect(Bus::Session).await.expect("user manager");
    let name = format!("{template}@2000007.service");
    let spec = UnitStart {
        name: name.clone(),
        memory_max: Some(64 * MIB),
        environment: None,
    };

    sd.start(&spec).await.expect("StartUnit");
    wait_for(&sd, &name, |s| s.is_some_and(|s| s.is_running())).await;
    let listed = sd.list(&format!("{template}@*")).await.unwrap();
    assert!(listed.iter().any(|(n, _)| *n == name), "{listed:?}");

    sd.stop(&name).await.expect("stop");
    wait_for(&sd, &name, |s| s.is_none()).await;
    // Stopping what is not loaded is success.
    sd.stop(&name).await.expect("stop again");
}

/// An instance that cannot start stays loaded as `failed` (the template's
/// CollectMode=inactive) and says why; stop then frees the name.
#[tokio::test]
#[ignore = "needs a systemd user session bus (DBUS_SESSION_BUS_ADDRESS)"]
async fn an_instance_that_cannot_start_is_kept_failed_with_its_reason() {
    let template = format!("banlieue-livefail-{}", std::process::id());
    let _t = Template::install(&template, "/nonexistent/banlieue-no-such-binary");
    let sd = Systemd::connect(Bus::Session).await.expect("user manager");
    let name = format!("{template}@1.service");
    let spec = UnitStart {
        name: name.clone(),
        memory_max: None,
        environment: None,
    };

    sd.start(&spec).await.expect("StartUnit");
    wait_for(&sd, &name, |s| s == Some(&UnitState::Failed)).await;
    let why = sd.failure(&name).await.unwrap().expect("a failure reason");
    assert!(why.contains("status 203"), "{why}");

    sd.stop(&name).await.expect("stop");
    wait_for(&sd, &name, |s| s.is_none()).await;
    assert!(sd.failure(&name).await.unwrap().is_none());
}

/// `stop` returns only once the unit is gone, not when systemd has merely
/// queued the stop job. A guest VMM takes seconds to stop; teardown checks
/// the unit is unloaded right after `stop`, and on a real host that check
/// failed and requeued the delete one to five times per guest (roadmap 17
/// phase G run, 2026-09-28).
#[tokio::test]
#[ignore = "needs a systemd user session bus (DBUS_SESSION_BUS_ADDRESS)"]
async fn stop_returns_only_once_a_slow_stopping_unit_is_gone() {
    let template = format!("banlieue-liveslow-{}", std::process::id());
    let _t = Template::install_with(&template, "/usr/bin/sleep 300", SLOW_STOP);
    let sd = Systemd::connect(Bus::Session).await.expect("user manager");
    let name = format!("{template}@1.service");
    let spec = UnitStart {
        name: name.clone(),
        memory_max: None,
        environment: None,
    };

    sd.start(&spec).await.expect("StartUnit");
    wait_for(&sd, &name, |s| s.is_some_and(|s| s.is_running())).await;

    sd.stop(&name).await.expect("stop");
    // No waiting here: that is the property under test.
    assert_eq!(sd.state(&name).await.unwrap(), None, "{name} still loaded");
}

/// A unit that fails *as it stops* is left `failed`, so still loaded. `stop`
/// clears that too, after the stop has finished rather than before it.
#[tokio::test]
#[ignore = "needs a systemd user session bus (DBUS_SESSION_BUS_ADDRESS)"]
async fn stop_frees_the_name_of_a_unit_that_fails_as_it_stops() {
    let template = format!("banlieue-livestopfail-{}", std::process::id());
    let _t = Template::install_with(
        &template,
        "/bin/sh -c 'trap \"sleep 1; exit 3\" TERM; sleep 300 & wait'",
        "",
    );
    let sd = Systemd::connect(Bus::Session).await.expect("user manager");
    let name = format!("{template}@1.service");
    let spec = UnitStart {
        name: name.clone(),
        memory_max: None,
        environment: None,
    };

    sd.start(&spec).await.expect("StartUnit");
    wait_for(&sd, &name, |s| s.is_some_and(|s| s.is_running())).await;

    sd.stop(&name).await.expect("stop");
    assert_eq!(sd.state(&name).await.unwrap(), None, "{name} still loaded");
}
