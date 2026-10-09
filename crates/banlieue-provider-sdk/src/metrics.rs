// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The per-process Prometheus registry, served at `GET /metrics` (ADR-0091).
//!
//! One [`Metrics`] is created per process in the role's `run()` and cloned
//! wherever it is needed (it is an `Arc` inside). It owns:
//!
//! | Metric | Type | Labels |
//! | --- | --- | --- |
//! | `banlieue_reconcile_total` | counter | `controller`, `result` |
//! | `banlieue_reconcile_duration_seconds` | histogram | `controller` |
//! | `banlieue_reconcile_errors_total` | counter | `controller`, `kind` |
//! | `banlieue_leader` | gauge | `role` |
//!
//! The reconcile series are recorded by [`crate::runner`], never by call
//! sites. Every label value is a `&'static str` chosen by code (a controller
//! kind, a result, an error variant name, a role), never an object name,
//! namespace or free text, so cardinality is bounded by the number of
//! controllers and error variants. Role-specific gauges (the controller's
//! `banlieue_virtualmachines`, `banlieue_provider_failure_domains`) are
//! [`Collector`]s added with [`Metrics::register_collector`].

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use prometheus_client::collector::Collector;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::{Registry, Unit};

use crate::bootstrap::BootstrapError;
use crate::httpd::{self, METHOD_GET, RequestLine, Response, Status};

/// Path the registry is served on.
pub const METRICS_PATH: &str = "/metrics";

/// `Content-Type` of the OpenMetrics text exposition format.
pub const OPENMETRICS_CONTENT_TYPE: &str =
    "application/openmetrics-text; version=1.0.0; charset=utf-8";

/// Name used for the metrics listener in logs and errors.
const METRICS_SERVER: &str = "metrics";

/// Reconcile counter. The encoder appends `_total`.
const RECONCILE_TOTAL_NAME: &str = "banlieue_reconcile";

/// Reconcile duration histogram. The encoder appends the `_seconds` unit.
const RECONCILE_DURATION_NAME: &str = "banlieue_reconcile_duration";

/// Reconcile error counter. The encoder appends `_total`.
const RECONCILE_ERRORS_NAME: &str = "banlieue_reconcile_errors";

/// Leadership gauge (ADR-0093 Decision 3).
const LEADER_NAME: &str = "banlieue_leader";

/// Histogram buckets for one reconcile, in seconds: from a cache-only
/// reconcile (milliseconds) to one that waits on a slow backend call.
pub const RECONCILE_DURATION_BUCKETS_SECS: [f64; 12] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// A typed error that can name its own variant, for the `kind` label of
/// `banlieue_reconcile_errors_total`.
///
/// Implemented by every role's error enum with a plain `match`, so the label
/// set is the enum's variant list and nothing else: never the error message,
/// which can carry object names or backend detail.
pub trait ErrorKind {
    /// The variant name, e.g. `Kube` or `Missing`.
    fn kind(&self) -> &'static str;
}

/// The `result` label of `banlieue_reconcile_total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileResult {
    /// Reconciled to a steady state: the default or long periodic requeue, or
    /// await-change.
    Success,
    /// The reconciler returned an error.
    Error,
    /// The reconciler succeeded but asked to run again sooner than the
    /// periodic requeue, because work is still in flight.
    Requeue,
}

impl ReconcileResult {
    /// The label value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Requeue => "requeue",
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ReconcileLabels {
    controller: &'static str,
    result: &'static str,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ControllerLabels {
    controller: &'static str,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ErrorLabels {
    controller: &'static str,
    kind: &'static str,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct RoleLabels {
    role: String,
}

/// Constructor for the duration histogram family.
fn duration_histogram() -> Histogram {
    Histogram::new(RECONCILE_DURATION_BUCKETS_SECS)
}

#[derive(Debug)]
struct Inner {
    registry: Mutex<Registry>,
    role: String,
    reconcile_total: Family<ReconcileLabels, Counter>,
    reconcile_duration: Family<ControllerLabels, Histogram, fn() -> Histogram>,
    reconcile_errors: Family<ErrorLabels, Counter>,
    leader: Family<RoleLabels, Gauge>,
}

/// The process's metrics registry. Cheap to clone; clones share state.
#[derive(Debug, Clone)]
pub struct Metrics {
    inner: Arc<Inner>,
}

impl Metrics {
    /// A fresh registry with the reconcile and leader families registered.
    ///
    /// # Arguments
    /// * `role` - the `role` label of `banlieue_leader`, e.g.
    ///   `banlieue-controller`. One per process.
    pub fn new(role: impl Into<String>) -> Self {
        let reconcile_total = Family::<ReconcileLabels, Counter>::default();
        let reconcile_duration =
            Family::<ControllerLabels, Histogram, fn() -> Histogram>::new_with_constructor(
                duration_histogram,
            );
        let reconcile_errors = Family::<ErrorLabels, Counter>::default();
        let leader = Family::<RoleLabels, Gauge>::default();

        let mut registry = Registry::default();
        registry.register(
            RECONCILE_TOTAL_NAME,
            "Reconciles run, by controller and result",
            reconcile_total.clone(),
        );
        registry.register_with_unit(
            RECONCILE_DURATION_NAME,
            "Wall time of one reconcile, by controller",
            Unit::Seconds,
            reconcile_duration.clone(),
        );
        registry.register(
            RECONCILE_ERRORS_NAME,
            "Reconciles that returned an error, by controller and error variant",
            reconcile_errors.clone(),
        );
        registry.register(
            LEADER_NAME,
            "1 while this process holds its leader lease (or runs without election), else 0",
            leader.clone(),
        );

        Self {
            inner: Arc::new(Inner {
                registry: Mutex::new(registry),
                role: role.into(),
                reconcile_total,
                reconcile_duration,
                reconcile_errors,
                leader,
            }),
        }
    }

    /// The role this registry was created for.
    pub fn role(&self) -> &str {
        &self.inner.role
    }

    /// This process's `banlieue_leader{role}` series, for
    /// [`crate::health::Readiness`] to drive.
    pub fn leader_gauge(&self) -> Gauge {
        self.inner
            .leader
            .get_or_create(&RoleLabels {
                role: self.inner.role.clone(),
            })
            .clone()
    }

    /// Record one finished reconcile.
    pub fn record_reconcile(
        &self,
        controller: &'static str,
        result: ReconcileResult,
        elapsed: Duration,
    ) {
        self.inner
            .reconcile_total
            .get_or_create(&ReconcileLabels {
                controller,
                result: result.as_str(),
            })
            .inc();
        self.inner
            .reconcile_duration
            .get_or_create(&ControllerLabels { controller })
            .observe(elapsed.as_secs_f64());
    }

    /// Record one reconcile error of variant `kind`.
    pub fn record_error(&self, controller: &'static str, kind: &'static str) {
        self.inner
            .reconcile_errors
            .get_or_create(&ErrorLabels { controller, kind })
            .inc();
    }

    /// Add a collector that encodes its own series on every scrape.
    pub fn register_collector(&self, collector: Box<dyn Collector>) {
        self.lock_registry().register_collector(collector);
    }

    fn lock_registry(&self) -> MutexGuard<'_, Registry> {
        self.inner
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Encode the registry in the OpenMetrics text format.
    ///
    /// # Errors
    /// Propagates a formatter error from an encoder or collector.
    pub fn encode(&self) -> Result<String, std::fmt::Error> {
        let mut out = String::new();
        prometheus_client::encoding::text::encode(&mut out, &self.lock_registry())?;
        Ok(out)
    }
}

/// Route one metrics request.
pub fn route(line: &RequestLine<'_>, metrics: &Metrics) -> Response {
    if line.method != METHOD_GET || line.path != METRICS_PATH {
        return Response::not_found();
    }
    match metrics.encode() {
        Ok(body) => Response {
            status: Status::Ok,
            content_type: OPENMETRICS_CONTENT_TYPE,
            body,
        },
        Err(e) => {
            tracing::error!(error = %e, "encoding metrics failed");
            Response::text(Status::ServiceUnavailable, "encoding failed")
        }
    }
}

/// Answer raw request bytes for the metrics server.
pub fn handle(buf: &[u8], metrics: &Metrics) -> Response {
    httpd::respond(buf, |line| route(line, metrics))
}

/// Bind `--metrics-port` and start serving the registry in the background.
///
/// Called next to the health server, before leader election, so a standby
/// is scrapeable too (ADR-0091 Decision 1).
///
/// # Errors
/// [`BootstrapError::Bind`] when the port cannot be bound.
pub async fn start_metrics_server(port: u16, metrics: Metrics) -> Result<(), BootstrapError> {
    let listener = httpd::bind(port)
        .await
        .map_err(|source| BootstrapError::Bind {
            server: METRICS_SERVER,
            port,
            source,
        })?;
    httpd::spawn_server(
        listener,
        METRICS_SERVER,
        Arc::new(move |buf: &[u8]| handle(buf, &metrics)),
    );
    Ok(())
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod metrics_tests;
