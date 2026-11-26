// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue-azure-guest ready`: tell Azure the VM is ready (ADR-0163
//! Decision 2, ADR-0171 Decision 4).
//!
//! A VM from a generalized image stays `Creating` until the guest fetches
//! its goal state from WireServer and posts a `Health` document with
//! `<State>Ready</State>`; without it Azure fails the VM with
//! `OSProvisioningTimedOut`. The helper only **reports**: it sends three
//! identifiers WireServer gave it and reads nothing else back, and no
//! credential is involved on either side.
//!
//! It reports only from an installed boot (active or passive, never
//! recovery), and the layer runs it after Kairos's network stage, so every
//! stage of customData has run first. A failed attempt (no route yet, a
//! refused connection, a non-2xx) is retried with doubling back-off until a
//! 2xx or the deadline.
//!
//! The address is a constant. [`run_with`] takes it as a parameter only so
//! a test can point it at a local fake; nothing on the command line or in
//! the environment can change it.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::azure::{self, BootMode};
use crate::goalstate::{GoalStateError, health_ready_document, parse_goal_state};
use crate::http::{self, HttpError, Request};
use crate::kmsg::Logger;

/// WireServer's address: Azure's fixed, host-local virtual IP.
pub const WIRESERVER_IP: Ipv4Addr = Ipv4Addr::new(168, 63, 129, 16);

/// WireServer's plain-HTTP port.
pub const WIRESERVER_PORT: u16 = 80;

/// Goal state endpoint.
pub const GOAL_STATE_PATH: &str = "/machine/?comp=goalstate";

/// Health report endpoint.
pub const HEALTH_PATH: &str = "/machine/?comp=health";

/// Protocol version WireServer is asked to speak.
pub const WIRESERVER_VERSION: &str = "2012-11-30";

/// Name the helper gives itself to WireServer.
pub const AGENT_NAME: &str = "banlieue-azure-guest";

const HEADER_AGENT_NAME: &str = "x-ms-agent-name";
const HEADER_VERSION: &str = "x-ms-version";
const HEADER_CONTENT_TYPE: &str = "Content-Type";
const HEALTH_CONTENT_TYPE: &str = "text/xml;charset=utf-8";

/// First wait after a failed attempt.
const INITIAL_BACKOFF_SECS: u64 = 1;
/// Longest wait between attempts.
const MAX_BACKOFF_SECS: u64 = 30;
/// Give up after this long. Azure's provisioning window is about 20
/// minutes from create, of which a raw image's first (recovery) boot uses
/// three to five; a report later than this would not land in time anyway.
const DEADLINE_SECS: u64 = 15 * 60;
/// Connect, read and write timeout of one request.
const REQUEST_TIMEOUT_SECS: u64 = 10;

/// WireServer's socket address.
#[must_use]
pub fn wireserver_addr() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(WIRESERVER_IP, WIRESERVER_PORT))
}

/// How hard to try.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// First back-off.
    pub initial: Duration,
    /// Back-off ceiling.
    pub max: Duration,
    /// Total time allowed.
    pub deadline: Duration,
    /// Timeout of each request.
    pub request_timeout: Duration,
}

impl RetryPolicy {
    /// The policy on a real guest.
    #[must_use]
    pub fn system() -> Self {
        Self {
            initial: Duration::from_secs(INITIAL_BACKOFF_SECS),
            max: Duration::from_secs(MAX_BACKOFF_SECS),
            deadline: Duration::from_secs(DEADLINE_SECS),
            request_timeout: Duration::from_secs(REQUEST_TIMEOUT_SECS),
        }
    }
}

/// The back-off after `current`: doubled, capped at `max`.
#[must_use]
pub fn next_backoff(current: Duration, max: Duration) -> Duration {
    current.saturating_mul(2).min(max)
}

/// Why one attempt failed.
#[derive(Debug, Error)]
pub enum AttemptError {
    /// The request did not complete.
    #[error("{0}")]
    Http(#[from] HttpError),
    /// WireServer answered with a non-2xx status.
    #[error("{what} answered HTTP {status}")]
    Status {
        /// Which request.
        what: &'static str,
        /// The status code.
        status: u16,
    },
    /// The goal state was refused.
    #[error("{0}")]
    GoalState(#[from] GoalStateError),
}

/// Why `ready` gave up.
#[derive(Debug, Error)]
#[error("no ready report accepted after {attempts} attempts in {elapsed_secs} s: {last}")]
pub struct ReadyError {
    /// Attempts made.
    pub attempts: u32,
    /// Seconds spent.
    pub elapsed_secs: u64,
    /// The last attempt's failure.
    pub last: String,
}

/// What `ready` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Not on Azure: nothing sent.
    NotAzure,
    /// Not an installed boot (recovery, or no Kairos marker): nothing sent.
    NotInstalledBoot(BootMode),
    /// WireServer accepted the report, after this many attempts.
    Reported(u32),
}

/// One attempt: fetch the goal state, then post `Ready`.
///
/// # Errors
/// [`AttemptError`] for whichever step failed.
pub fn report_once(addr: SocketAddr, timeout: Duration) -> Result<(), AttemptError> {
    let common = [
        (HEADER_AGENT_NAME, AGENT_NAME),
        (HEADER_VERSION, WIRESERVER_VERSION),
    ];
    let goal = http::send(
        addr,
        &Request {
            method: "GET",
            path: GOAL_STATE_PATH,
            headers: &common,
            body: None,
        },
        timeout,
    )?;
    if !goal.is_success() {
        return Err(AttemptError::Status {
            what: "goalstate",
            status: goal.status,
        });
    }
    let gs = parse_goal_state(&goal.body)?;
    let body = health_ready_document(&gs);
    let headers = [
        common[0],
        common[1],
        (HEADER_CONTENT_TYPE, HEALTH_CONTENT_TYPE),
    ];
    let health = http::send(
        addr,
        &Request {
            method: "POST",
            path: HEALTH_PATH,
            headers: &headers,
            body: Some(body.as_bytes()),
        },
        timeout,
    )?;
    if !health.is_success() {
        return Err(AttemptError::Status {
            what: "health",
            status: health.status,
        });
    }
    Ok(())
}

/// Retry [`report_once`] until it succeeds or `policy.deadline` passes.
/// `sleep` is injected so a test does not wait in real time.
///
/// # Errors
/// [`ReadyError`] when the deadline passes without an accepted report.
pub fn report_ready<S>(
    addr: SocketAddr,
    policy: &RetryPolicy,
    log: &mut Logger,
    mut sleep: S,
) -> Result<u32, ReadyError>
where
    S: FnMut(Duration),
{
    let start = Instant::now();
    let mut backoff = policy.initial;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let err = match report_once(addr, policy.request_timeout) {
            Ok(()) => {
                log.info(&format!(
                    "ready: reported after {attempts} attempt(s), {} ms",
                    start.elapsed().as_millis()
                ));
                return Ok(attempts);
            }
            Err(e) => e,
        };
        let elapsed = start.elapsed();
        if elapsed + backoff > policy.deadline {
            return Err(ReadyError {
                attempts,
                elapsed_secs: elapsed.as_secs(),
                last: err.to_string(),
            });
        }
        log.info(&format!(
            "ready: attempt {attempts} failed ({err}); retrying in {} ms",
            backoff.as_millis()
        ));
        sleep(backoff);
        backoff = next_backoff(backoff, policy.max);
    }
}

/// The whole `ready` command with every input injected.
///
/// # Errors
/// As [`report_ready`].
pub fn run_with<S>(
    asset_tag: &Path,
    mode_dir: &Path,
    addr: SocketAddr,
    policy: &RetryPolicy,
    log: &mut Logger,
    sleep: S,
) -> Result<Outcome, ReadyError>
where
    S: FnMut(Duration),
{
    if !azure::is_azure(asset_tag) {
        return Ok(Outcome::NotAzure);
    }
    let mode = azure::boot_mode(mode_dir);
    if !mode.is_installed() {
        log.info(&format!("ready: {mode:?} boot, not reporting"));
        return Ok(Outcome::NotInstalledBoot(mode));
    }
    report_ready(addr, policy, log, sleep).map(Outcome::Reported)
}

/// The `ready` command on a real guest.
///
/// # Errors
/// As [`report_ready`].
pub fn run(log: &mut Logger) -> Result<Outcome, ReadyError> {
    run_with(
        Path::new(azure::CHASSIS_ASSET_TAG_PATH),
        Path::new(azure::KAIROS_MODE_DIR),
        wireserver_addr(),
        &RetryPolicy::system(),
        log,
        thread::sleep,
    )
}

#[cfg(test)]
#[path = "ready_tests.rs"]
mod ready_tests;
