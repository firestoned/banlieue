// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Guests and their helpers as instances of root-owned template units, over
//! D-Bus (ADR-0063 Decision 1, amended).
//!
//! systemd owns the VMM processes, not the provider, so restarting or
//! upgrading the provider leaves guests running. What each unit runs, as
//! whom, and in which sandbox is fixed by a **template file** the bootstrap
//! installs (`deploy/provider-cloud-hypervisor/host/*@.service`); the
//! provider chooses only the instance, sets resource limits at runtime, and
//! for the two templates that run as its own user writes an environment
//! file first. It cannot describe a unit, so it cannot start anything as
//! another user: the transient units this replaced let whoever held the
//! provider's user start `banlieue-ch-<uuid>.service` with `User=root`.
//!
//! No `systemd-run`, no subprocess.

use futures::StreamExt as _;
use std::path::PathBuf;
use std::time::Duration;
use zbus::zvariant::{OwnedObjectPath, Value};

/// systemd's job mode: fail rather than queue behind a conflicting job.
const JOB_MODE_FAIL: &str = "fail";
/// Job mode for stop: replace any queued start.
const JOB_MODE_REPLACE: &str = "replace";
/// Longest `stop` waits for its stop job. Above systemd's default
/// `TimeoutStopSec=` (90 s), so systemd's own escalation to `SIGKILL`
/// always comes first and this only bounds a manager that never answers.
const STOP_JOB_TIMEOUT: Duration = Duration::from_secs(120);
/// systemd's exit statuses for a step it failed before exec
/// (`systemd.exec(5)`, "Process Exit Codes"), the ones a guest unit can hit.
const SYSTEMD_EXIT_STEPS: &[(i32, &str)] = &[
    (200, "CHDIR"),
    (203, "EXEC: the VMM binary could not be executed"),
    (216, "GROUP: the unit's group is unknown to NSS"),
    (217, "USER: the unit's user is unknown to NSS"),
    (226, "NAMESPACE: the sandbox could not be set up"),
    (228, "SECCOMP"),
    (243, "CREDENTIALS"),
];

/// Starting one instance of a banlieue template unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitStart {
    /// `<template>@<instance>.service`.
    pub name: String,
    /// cgroup memory ceiling, set at runtime before the start. The only
    /// property the provider may set (polkit, and systemd itself refuses
    /// anything but resource limits on a unit with a file).
    pub memory_max: Option<u64>,
    /// Written before the start, for a template that reads one.
    pub environment: Option<EnvFile>,
}

/// An `EnvironmentFile=` a template reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvFile {
    /// Where the template expects it.
    pub path: PathBuf,
    /// `NAME=value` pairs.
    pub vars: Vec<(String, String)>,
}

impl EnvFile {
    /// The file's text. Values are passed to the template's `ExecStart` as
    /// single words (`${NAME}`), so anything that could change the parse is
    /// refused: whitespace, quotes, `$`, `\`, control characters.
    ///
    /// # Errors
    /// A message naming the offending variable.
    pub fn render(&self) -> Result<String, String> {
        let mut out = String::new();
        for (name, value) in &self.vars {
            let name_ok =
                !name.is_empty() && name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_');
            let value_ok = !value.is_empty()
                && value.bytes().all(|b| {
                    b.is_ascii_graphic() && !matches!(b, b'"' | b'\'' | b'\\' | b'$' | b'`')
                });
            if !name_ok || !value_ok {
                return Err(format!("{name}: not a safe environment file entry"));
            }
            out.push_str(&format!("{name}={value}\n"));
        }
        Ok(out)
    }
}

impl UnitStart {
    /// The runtime properties set before the start.
    #[must_use]
    pub fn runtime_properties(&self) -> Vec<(&'static str, Value<'static>)> {
        self.memory_max
            .map(|b| vec![("MemoryMax", Value::from(b))])
            .unwrap_or_default()
    }
}

/// A unit's `ActiveState`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnitState {
    /// Running.
    Active,
    /// Starting.
    Activating,
    /// Stopping.
    Deactivating,
    /// Loaded and not running.
    Inactive,
    /// Stopped with an error.
    Failed,
    /// Anything else systemd reports.
    Other(String),
}

impl UnitState {
    /// Map systemd's `ActiveState` string.
    #[must_use]
    pub fn from_active_state(s: &str) -> Self {
        match s {
            "active" => Self::Active,
            "activating" | "reloading" => Self::Activating,
            "deactivating" => Self::Deactivating,
            "inactive" => Self::Inactive,
            "failed" => Self::Failed,
            other => Self::Other(other.to_string()),
        }
    }

    /// Whether a process is (or is about to be) running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Active | Self::Activating)
    }
}

/// One row of `ListUnitsByPatterns`.
type UnitRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    OwnedObjectPath,
    u32,
    String,
    OwnedObjectPath,
);

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait Manager {
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;

    fn set_unit_properties(
        &self,
        name: &str,
        runtime: bool,
        properties: &[(&str, Value<'_>)],
    ) -> zbus::Result<()>;

    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;

    fn reset_failed_unit(&self, name: &str) -> zbus::Result<()>;

    /// Ask the manager to emit its signals to this connection; without it
    /// systemd does not send `JobRemoved`.
    fn subscribe(&self) -> zbus::Result<()>;

    /// A job finished, successfully or not.
    #[zbus(signal)]
    fn job_removed(
        &self,
        id: u32,
        job: OwnedObjectPath,
        unit: String,
        result: String,
    ) -> zbus::Result<()>;

    fn list_units_by_patterns(
        &self,
        states: &[&str],
        patterns: &[&str],
    ) -> zbus::Result<Vec<UnitRow>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Service",
    default_service = "org.freedesktop.systemd1"
)]
trait Service {
    #[zbus(property)]
    fn result(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn exec_main_status(&self) -> zbus::Result<i32>;
}

/// systemd's `Result` and `ExecMainStatus` for a failed unit, readably.
#[must_use]
pub fn describe_exit(result: &str, status: i32) -> String {
    match SYSTEMD_EXIT_STEPS.iter().find(|(code, _)| *code == status) {
        Some((_, step)) if result == "exit-code" => {
            format!("{result}, status {status} ({step})")
        }
        _ => format!("{result}, status {status}"),
    }
}

/// Which systemd manager to talk to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bus {
    /// The system manager: production (ADR-0063).
    System,
    /// The calling user's manager: tests, without root.
    Session,
}

/// A handle on one systemd manager.
#[derive(Clone, Debug)]
pub struct Systemd {
    conn: zbus::Connection,
    manager: ManagerProxy<'static>,
}

impl Systemd {
    /// Connect to `bus`.
    ///
    /// # Errors
    /// The D-Bus error from connecting.
    pub async fn connect(bus: Bus) -> zbus::Result<Self> {
        let conn = match bus {
            Bus::System => zbus::Connection::system().await?,
            Bus::Session => zbus::Connection::session().await?,
        };
        let manager = ManagerProxy::new(&conn).await?;
        manager.subscribe().await?;
        Ok(Self { conn, manager })
    }

    /// Set `spec`'s runtime limits, then start the instance. The
    /// environment file, if any, must already be written.
    ///
    /// # Errors
    /// The D-Bus error, including a template that is not installed.
    pub async fn start(&self, spec: &UnitStart) -> zbus::Result<()> {
        let props = spec.runtime_properties();
        if !props.is_empty() {
            self.manager
                .set_unit_properties(&spec.name, true, &props)
                .await?;
        }
        self.manager
            .start_unit(&spec.name, JOB_MODE_FAIL)
            .await
            .map(drop)
    }

    /// Stop `name`, wait for the stop to finish, and clear any failed state
    /// so the name is free again. A unit that is not loaded is success.
    ///
    /// `StopUnit` only queues a job; a VMM takes seconds to stop. Returning
    /// before the job finishes left callers checking a unit that was still
    /// `deactivating`, and a reset of a failure that had not happened yet.
    ///
    /// # Errors
    /// Any D-Bus error other than "not loaded", or a stop job that did not
    /// finish within [`STOP_JOB_TIMEOUT`].
    pub async fn stop(&self, name: &str) -> zbus::Result<()> {
        // Listen before asking, so a fast job cannot finish unseen.
        let mut removed = self.manager.receive_job_removed().await?;
        match self.manager.stop_unit(name, JOB_MODE_REPLACE).await {
            Ok(job) => {
                tokio::time::timeout(STOP_JOB_TIMEOUT, async {
                    while let Some(signal) = removed.next().await {
                        if signal.args()?.job == job {
                            return Ok(());
                        }
                    }
                    Err(zbus::Error::Failure(format!(
                        "signal stream ended while stopping {name}"
                    )))
                })
                .await
                .map_err(|_| {
                    zbus::Error::Failure(format!(
                        "{name}: stop job still running after {}s",
                        STOP_JOB_TIMEOUT.as_secs()
                    ))
                })??;
            }
            Err(e) if is_no_such_unit(&e) => {}
            Err(e) => return Err(e),
        }
        match self.manager.reset_failed_unit(name).await {
            Ok(()) => Ok(()),
            Err(e) if is_no_such_unit(&e) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Loaded units whose names match `pattern` (a shell glob), with state.
    ///
    /// # Errors
    /// The D-Bus error.
    pub async fn list(&self, pattern: &str) -> zbus::Result<Vec<(String, UnitState)>> {
        let rows = self.manager.list_units_by_patterns(&[], &[pattern]).await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.0, UnitState::from_active_state(&r.3)))
            .collect())
    }

    /// Why `name` failed, when it is loaded and failed; `None` otherwise.
    ///
    /// # Errors
    /// The D-Bus error.
    pub async fn failure(&self, name: &str) -> zbus::Result<Option<String>> {
        let Some(row) = self
            .manager
            .list_units_by_patterns(&[], &[name])
            .await?
            .into_iter()
            .find(|r| r.0 == name)
        else {
            return Ok(None);
        };
        if UnitState::from_active_state(&row.3) != UnitState::Failed {
            return Ok(None);
        }
        let service = ServiceProxy::builder(&self.conn)
            .path(row.6)?
            .build()
            .await?;
        Ok(Some(describe_exit(
            &service.result().await?,
            service.exec_main_status().await?,
        )))
    }

    /// The state of `name`, or `None` when it is not loaded.
    ///
    /// # Errors
    /// The D-Bus error.
    pub async fn state(&self, name: &str) -> zbus::Result<Option<UnitState>> {
        Ok(self
            .list(name)
            .await?
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s))
    }
}

/// Whether a D-Bus error means "no such unit".
fn is_no_such_unit(e: &zbus::Error) -> bool {
    match e {
        zbus::Error::MethodError(name, _, _) => {
            name.as_str() == "org.freedesktop.systemd1.NoSuchUnit"
        }
        _ => false,
    }
}

#[cfg(test)]
#[path = "systemd_tests.rs"]
mod systemd_tests;
