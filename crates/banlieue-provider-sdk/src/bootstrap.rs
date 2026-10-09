// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared process bootstrap helpers.
//!
//! Every banlieue controller role (`controller`, each `provider <name>`)
//! needs the same things before it can run its reconcilers:
//!
//! - structured logging initialised, with optional OTLP trace export
//!   ([`init_tracing`], ADR-0092),
//! - the health and metrics servers ([`crate::health`], [`crate::metrics`]),
//! - and a SIGTERM / Ctrl-C shutdown future ([`shutdown_signal`]).
//!
//! This module is the single home for that boilerplate so it isn't copied
//! into every role's `run()` entry point (see ADR-0004: the single `banlieue`
//! binary dispatches into independent library crates that all share these).

use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::{error, info, warn};

use crate::health::{Election, Readiness, start_health_server};
use crate::metrics::{Metrics, start_metrics_server};

/// Log level used when neither an explicit `--log-level` nor `RUST_LOG` is set.
const DEFAULT_LOG_LEVEL: &str = "info";

/// `--log-format` value selecting JSON (SIEM-friendly) output. Any other value
/// falls back to the human-readable text formatter.
const JSON_LOG_FORMAT: &str = "json";

/// Standard OpenTelemetry variable naming the OTLP endpoint for every signal.
/// Setting it (or [`OTEL_TRACES_ENDPOINT_ENV`]) is what turns export on.
pub const OTEL_ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";

/// Standard OpenTelemetry variable naming the OTLP endpoint for traces only.
pub const OTEL_TRACES_ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT";

/// Standard OpenTelemetry variable overriding `service.name`.
pub const OTEL_SERVICE_NAME_ENV: &str = "OTEL_SERVICE_NAME";

/// `service.version` resource attribute key.
const SERVICE_VERSION_KEY: &str = "service.version";

/// The binary's version. Every crate shares the workspace version, so the
/// SDK's is the binary's.
const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Instrumentation scope name of the tracer behind the `tracing` bridge.
const TRACER_SCOPE: &str = "banlieue";

/// Errors raised while bootstrapping a process.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// The assembled `EnvFilter` directive string was not a valid filter.
    #[error("invalid log filter {0:?}: {1}")]
    LogFilter(String, String),

    /// The global `tracing` subscriber could not be installed (typically
    /// because one was already installed in this process).
    #[error("init tracing subscriber: {0}")]
    Init(String),

    /// The OTLP span exporter could not be built from the `OTEL_*`
    /// environment (ADR-0092).
    #[error("build OTLP trace exporter: {0}")]
    Otlp(String),

    /// The health or metrics listener could not bind its port. Fatal
    /// (ADR-0093 Decision 4).
    #[error("bind {server} server on port {port}: {source}")]
    Bind {
        /// Which server (`health`, `metrics`).
        server: &'static str,
        /// The port it tried.
        port: u16,
        /// The bind error.
        #[source]
        source: std::io::Error,
    },
}

/// Assemble an `EnvFilter` directive spec from a base `level` and any number of
/// per-crate `extra` directives (e.g. `kube=warn`, `vim_rs=warn`).
fn join_directives(level: &str, extra: &[&str]) -> String {
    let mut spec = String::from(level);
    for directive in extra {
        spec.push(',');
        spec.push_str(directive);
    }
    spec
}

/// Whether a variable is set to something other than whitespace.
fn is_set(lookup: &impl Fn(&str) -> Option<String>, name: &str) -> bool {
    lookup(name).is_some_and(|v| !v.trim().is_empty())
}

/// Whether OTLP trace export is on: either standard endpoint variable is set
/// (ADR-0092 Decision 1). Pure over `lookup` so it is testable without
/// touching the process environment.
pub fn otlp_enabled(lookup: impl Fn(&str) -> Option<String>) -> bool {
    is_set(&lookup, OTEL_ENDPOINT_ENV) || is_set(&lookup, OTEL_TRACES_ENDPOINT_ENV)
}

/// The `service.name` banlieue sets explicitly: the role, unless
/// `OTEL_SERVICE_NAME` is set, in which case `None` leaves it to the
/// OpenTelemetry SDK's own environment detector (ADR-0092 Decision 3).
pub fn service_name_override(
    lookup: impl Fn(&str) -> Option<String>,
    role: &str,
) -> Option<String> {
    if is_set(&lookup, OTEL_SERVICE_NAME_ENV) {
        return None;
    }
    Some(role.to_string())
}

/// Build the OTLP tracer provider, or `None` when export is off.
///
/// The exporter reads the remaining standard variables itself (endpoint,
/// headers, timeout), as does the provider (`OTEL_TRACES_SAMPLER`, default
/// parent-based always-on; `OTEL_RESOURCE_ATTRIBUTES`). Transport is
/// HTTP/protobuf over the workspace's reqwest, whose `rustls-no-provider`
/// build needs a process crypto provider: `ring` is installed here if none
/// is yet, the same provider every other banlieue connection uses.
///
/// # Errors
/// [`BootstrapError::Otlp`] when the exporter cannot be built.
fn build_tracer_provider(
    role: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<SdkTracerProvider>, BootstrapError> {
    if !otlp_enabled(&lookup) {
        return Ok(None);
    }

    // An `Err` only means another crate installed a provider first.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .build()
        .map_err(|e| BootstrapError::Otlp(e.to_string()))?;

    let mut resource = Resource::builder();
    if let Some(name) = service_name_override(&lookup, role) {
        resource = resource.with_service_name(name);
    }
    let resource = resource
        .with_attribute(KeyValue::new(SERVICE_VERSION_KEY, SERVICE_VERSION))
        .build();

    Ok(Some(
        SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource)
            .build(),
    ))
}

/// Keeps the OTLP exporter alive. Dropping it, or calling
/// [`TracingGuard::shutdown`], flushes buffered spans and stops the exporter.
/// Hold it for the whole of the role's `run()`.
#[derive(Debug)]
#[must_use = "dropping the guard immediately shuts down trace export"]
pub struct TracingGuard {
    provider: Option<SdkTracerProvider>,
}

impl TracingGuard {
    /// Whether spans are being exported.
    pub fn exporting(&self) -> bool {
        self.provider.is_some()
    }

    /// Flush and stop the exporter now. A no-op when export is off.
    pub fn shutdown(mut self) {
        self.flush_and_stop();
    }

    fn flush_and_stop(&mut self) {
        let Some(provider) = self.provider.take() else {
            return;
        };
        if let Err(e) = provider.shutdown() {
            warn!(error = %e, "OTLP trace exporter shutdown failed; spans may be lost");
        }
    }
}

impl Drop for TracingGuard {
    fn drop(&mut self) {
        self.flush_and_stop();
    }
}

/// Initialise the global `tracing` subscriber, with OTLP trace export when
/// the standard `OTEL_*` endpoint variables ask for it (ADR-0092).
///
/// # Arguments
/// * `role` - the role's service name (e.g. `banlieue-controller`), used as
///   `service.name` unless `OTEL_SERVICE_NAME` overrides it.
/// * `format` - `"json"` for structured output; any other value selects the
///   human-readable text formatter.
/// * `level` - an explicit log level (e.g. from `--log-level`). When `Some`,
///   it takes precedence over `RUST_LOG` and is combined with `extra`. When
///   `None`, `RUST_LOG` is honoured, falling back to [`DEFAULT_LOG_LEVEL`] plus
///   `extra`.
/// * `extra` - per-crate directives always appended to the base level (e.g.
///   `["kube=warn", "vim_rs=warn"]`).
///
/// # Returns
/// A [`TracingGuard`] the role holds until it exits, so buffered spans are
/// flushed.
///
/// # Errors
/// Returns [`BootstrapError::LogFilter`] if the assembled directive string is
/// invalid, [`BootstrapError::Otlp`] if export was requested but the exporter
/// cannot be built, or [`BootstrapError::Init`] if a subscriber is already
/// installed.
pub fn init_tracing(
    role: &str,
    format: &str,
    level: Option<&str>,
    extra: &[&str],
) -> Result<TracingGuard, BootstrapError> {
    use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

    let filter = match level {
        Some(lvl) => {
            let spec = join_directives(lvl, extra);
            EnvFilter::try_new(&spec).map_err(|e| BootstrapError::LogFilter(spec, e.to_string()))?
        }
        None => EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(join_directives(DEFAULT_LOG_LEVEL, extra))),
    };

    let provider = build_tracer_provider(role, |name| std::env::var(name).ok())?;
    let otel_layer = provider
        .as_ref()
        .map(|p| tracing_opentelemetry::layer().with_tracer(p.tracer(TRACER_SCOPE)));

    let registry = tracing_subscriber::registry().with(filter).with(otel_layer);

    let installed = match format {
        JSON_LOG_FORMAT => registry
            .with(tracing_subscriber::fmt::layer().json())
            .try_init(),
        _ => registry.with(tracing_subscriber::fmt::layer()).try_init(),
    };
    installed.map_err(|e| BootstrapError::Init(e.to_string()))?;

    let guard = TracingGuard { provider };
    if guard.exporting() {
        info!(role, "OTLP trace export enabled");
    }
    Ok(guard)
}

/// The shared observability state a role's `run()` threads through: the
/// process metrics registry and the readiness the health server reports.
#[derive(Debug, Clone)]
pub struct Observability {
    /// The process registry (ADR-0091).
    pub metrics: Metrics,
    /// What `/readyz` reports (ADR-0093).
    pub readiness: Readiness,
}

/// Create the registry and readiness state, then bind and start the health
/// and metrics servers. Call before leader election, so a standby answers
/// probes and scrapes too.
///
/// # Arguments
/// * `role` - the `role` label of `banlieue_leader`.
/// * `health_port` / `metrics_port` - the role's `--health-port` and
///   `--metrics-port`.
/// * `election` - [`Election::Disabled`] under `--no-leader-elect`, else
///   [`crate::leader::LeaderConfig::election`].
///
/// # Errors
/// [`BootstrapError::Bind`] when either port cannot be bound. The role must
/// treat it as fatal (ADR-0093 Decision 4).
pub async fn start_observability(
    role: &str,
    health_port: u16,
    metrics_port: u16,
    election: Election,
) -> Result<Observability, BootstrapError> {
    let metrics = Metrics::new(role);
    let readiness = Readiness::new(election, metrics.leader_gauge());
    start_health_server(health_port, readiness.clone()).await?;
    start_metrics_server(metrics_port, metrics.clone()).await?;
    Ok(Observability { metrics, readiness })
}

/// Resolve when the process receives SIGTERM (containers) or Ctrl-C (local
/// dev), whichever fires first. If the SIGTERM handler cannot be installed the
/// future falls back to Ctrl-C only.
pub async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "failed to install SIGTERM handler — will only respond to Ctrl-C");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod bootstrap_tests;
