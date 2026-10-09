// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Path-aware liveness and readiness endpoints (ADR-0093).
//!
//! - `GET /livez` is `200` while the process can answer at all.
//! - `GET /readyz` is `200` when the role can do its job, or could take over
//!   from the replica that does, and `503` otherwise. The one-word body says
//!   which: `leader`, `standby`, `starting` or `unreachable`.
//! - Anything else is `404`; a malformed or oversized request line is `400`.
//!
//! Readiness does **not** mean leadership. A standby that has read the lease
//! from the API server within one lease duration is Ready, because a
//! Deployment whose standby is NotReady never reaches its available count
//! (ADR-0093 Context). The leader-election loop in [`crate::leader`] feeds a
//! shared [`Readiness`]; [`evaluate`] turns a snapshot of it into a
//! [`ReadyState`] as a pure function of the snapshot and the current time.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use prometheus_client::metrics::gauge::Gauge;

use crate::bootstrap::BootstrapError;
use crate::httpd::{self, METHOD_GET, RequestLine, Response, Status};

/// Liveness probe path.
pub const LIVEZ_PATH: &str = "/livez";

/// Readiness probe path.
pub const READYZ_PATH: &str = "/readyz";

/// Body of a `200` on [`LIVEZ_PATH`].
const LIVE_BODY: &str = "ok";

/// Name used for the health listener in logs and errors.
const HEALTH_SERVER: &str = "health";

/// What `/readyz` reports. The variant name, lower-cased, is the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyState {
    /// This replica holds the lease (or runs without election) and its
    /// controllers have started. Ready.
    Leader,
    /// Another replica holds the lease, and this one read it within one lease
    /// duration, so it would take over. Ready.
    Standby,
    /// No lease read yet, or the lease is held but controllers have not
    /// started. Not ready.
    Starting,
    /// A standby whose last successful lease read is older than one lease
    /// duration: it can no longer reach the API server. Not ready.
    Unreachable,
}

impl ReadyState {
    /// The `/readyz` body.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Leader => "leader",
            Self::Standby => "standby",
            Self::Starting => "starting",
            Self::Unreachable => "unreachable",
        }
    }

    /// Whether `/readyz` answers `200`.
    pub fn is_ready(self) -> bool {
        matches!(self, Self::Leader | Self::Standby)
    }
}

/// Whether this process elects a leader before running controllers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Election {
    /// Lease-based election; a standby stays Ready for `lease_duration` after
    /// its last successful lease read.
    Enabled {
        /// The lease duration the election loop uses.
        lease_duration: Duration,
    },
    /// `--no-leader-elect`: readiness is "controllers started".
    Disabled,
}

/// Everything readiness depends on, at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessSnapshot {
    /// Election mode.
    pub election: Election,
    /// This replica holds the lease.
    pub is_leader: bool,
    /// The role's controllers have been started.
    pub controllers_started: bool,
    /// When the lease was last read (or renewed) successfully.
    pub last_lease_read: Option<Instant>,
}

/// Decide what `/readyz` reports for `snapshot` at `now`. Pure.
pub fn evaluate(snapshot: &ReadinessSnapshot, now: Instant) -> ReadyState {
    let Election::Enabled { lease_duration } = snapshot.election else {
        if snapshot.controllers_started {
            return ReadyState::Leader;
        }
        return ReadyState::Starting;
    };

    if snapshot.is_leader {
        if snapshot.controllers_started {
            return ReadyState::Leader;
        }
        return ReadyState::Starting;
    }

    let Some(last_read) = snapshot.last_lease_read else {
        return ReadyState::Starting;
    };
    if now.saturating_duration_since(last_read) <= lease_duration {
        return ReadyState::Standby;
    }
    ReadyState::Unreachable
}

/// Shared, cheaply clonable readiness state. The election loop and the role's
/// `run()` write it; the health server reads it on every probe.
///
/// Also drives the `banlieue_leader{role}` gauge (ADR-0091), so the gauge and
/// the `/readyz` body can never disagree.
#[derive(Debug, Clone)]
pub struct Readiness {
    state: Arc<Mutex<ReadinessSnapshot>>,
    leader_gauge: Gauge,
}

impl Readiness {
    /// Fresh state: not leader, controllers not started, lease never read.
    ///
    /// # Arguments
    /// * `election` - the role's election mode.
    /// * `leader_gauge` - the role's `banlieue_leader` series, from
    ///   [`crate::metrics::Metrics::leader_gauge`].
    pub fn new(election: Election, leader_gauge: Gauge) -> Self {
        leader_gauge.set(0);
        Self {
            state: Arc::new(Mutex::new(ReadinessSnapshot {
                election,
                is_leader: false,
                controllers_started: false,
                last_lease_read: None,
            })),
            leader_gauge,
        }
    }

    /// Lock the state. A poisoned lock still holds a valid snapshot (every
    /// write is a single field store), so it is used as is.
    fn lock(&self) -> MutexGuard<'_, ReadinessSnapshot> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Record a successful lease read or renewal, now.
    pub fn lease_observed(&self) {
        self.lease_observed_at(Instant::now());
    }

    /// Record a successful lease read or renewal at `at`.
    pub fn lease_observed_at(&self, at: Instant) {
        self.lock().last_lease_read = Some(at);
    }

    /// This replica acquired the lease.
    pub fn became_leader(&self) {
        let mut state = self.lock();
        state.is_leader = true;
        state.last_lease_read = Some(Instant::now());
        self.leader_gauge.set(1);
    }

    /// This replica no longer holds the lease.
    pub fn lost_leadership(&self) {
        self.lock().is_leader = false;
        self.leader_gauge.set(0);
    }

    /// The role's controllers have started. Without election this is also
    /// when the replica starts reconciling, so the leader gauge goes to 1.
    pub fn controllers_started(&self) {
        let mut state = self.lock();
        state.controllers_started = true;
        if state.election == Election::Disabled {
            self.leader_gauge.set(1);
        }
    }

    /// A copy of the current state.
    pub fn snapshot(&self) -> ReadinessSnapshot {
        *self.lock()
    }

    /// What `/readyz` reports right now.
    pub fn state(&self) -> ReadyState {
        evaluate(&self.snapshot(), Instant::now())
    }
}

/// Route one health request. Pure in `(line, ready)`.
pub fn route(line: &RequestLine<'_>, ready: ReadyState) -> Response {
    if line.method != METHOD_GET {
        return Response::not_found();
    }
    match line.path {
        LIVEZ_PATH => Response::text(Status::Ok, LIVE_BODY),
        READYZ_PATH if ready.is_ready() => Response::text(Status::Ok, ready.as_str()),
        READYZ_PATH => Response::text(Status::ServiceUnavailable, ready.as_str()),
        _ => Response::not_found(),
    }
}

/// Answer raw request bytes for the health server: `400` when the request
/// line does not parse, otherwise [`route`].
pub fn handle(buf: &[u8], ready: ReadyState) -> Response {
    httpd::respond(buf, |line| route(line, ready))
}

/// Bind the health port and start serving it in the background.
///
/// Called before leader election, so a standby answers probes too.
///
/// # Errors
/// [`BootstrapError::Bind`] when the port cannot be bound. Fatal by design
/// (ADR-0093 Decision 4): a role without its health server would run
/// unobservable until the probes it cannot answer kill it.
pub async fn start_health_server(port: u16, readiness: Readiness) -> Result<(), BootstrapError> {
    let listener = httpd::bind(port)
        .await
        .map_err(|source| BootstrapError::Bind {
            server: HEALTH_SERVER,
            port,
            source,
        })?;
    httpd::spawn_server(
        listener,
        HEALTH_SERVER,
        Arc::new(move |buf: &[u8]| handle(buf, readiness.state())),
    );
    Ok(())
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod health_tests;
