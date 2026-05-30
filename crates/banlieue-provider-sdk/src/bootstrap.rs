// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared process bootstrap helpers.
//!
//! Every banlieue controller role (`controller`, each `provider <name>`)
//! needs the same three things before it can run its reconcilers:
//!
//! - structured logging initialised ([`init_tracing`]),
//! - a minimal health server for liveness/readiness probes ([`serve_health`]),
//! - and a SIGTERM / Ctrl-C shutdown future ([`shutdown_signal`]).
//!
//! This module is the single home for that boilerplate so it isn't copied
//! into every role's `run()` entry point (see ADR-0004 — the single `banlieue`
//! binary dispatches into independent library crates that all share these).

use tracing::{error, info};

/// Log level used when neither an explicit `--log-level` nor `RUST_LOG` is set.
const DEFAULT_LOG_LEVEL: &str = "info";

/// `--log-format` value selecting JSON (SIEM-friendly) output. Any other value
/// falls back to the human-readable text formatter.
const JSON_LOG_FORMAT: &str = "json";

/// Body returned by the health endpoints.
const HEALTH_BODY: &str = "ok";

/// Read buffer for the (ignored) inbound health request line.
const HEALTH_READ_BUF_SIZE: usize = 1024;

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

/// Initialise the global `tracing` subscriber.
///
/// # Arguments
/// * `format` - `"json"` for structured output; any other value selects the
///   human-readable text formatter.
/// * `level` - an explicit log level (e.g. from `--log-level`). When `Some`,
///   it takes precedence over `RUST_LOG` and is combined with `extra`. When
///   `None`, `RUST_LOG` is honoured, falling back to [`DEFAULT_LOG_LEVEL`] plus
///   `extra`.
/// * `extra` - per-crate directives always appended to the base level (e.g.
///   `["kube=warn", "vim_rs=warn"]`).
///
/// # Errors
/// Returns [`BootstrapError::LogFilter`] if the assembled directive string is
/// invalid, or [`BootstrapError::Init`] if a subscriber is already installed.
pub fn init_tracing(
    format: &str,
    level: Option<&str>,
    extra: &[&str],
) -> Result<(), BootstrapError> {
    use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

    let filter = match level {
        Some(lvl) => {
            let spec = join_directives(lvl, extra);
            EnvFilter::try_new(&spec).map_err(|e| BootstrapError::LogFilter(spec, e.to_string()))?
        }
        None => EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(join_directives(DEFAULT_LOG_LEVEL, extra))),
    };

    let registry = tracing_subscriber::registry().with(filter);

    match format {
        JSON_LOG_FORMAT => registry
            .with(tracing_subscriber::fmt::layer().json())
            .try_init()
            .map_err(|e| BootstrapError::Init(e.to_string())),
        _ => registry
            .with(tracing_subscriber::fmt::layer())
            .try_init()
            .map_err(|e| BootstrapError::Init(e.to_string())),
    }
}

/// Build the fixed HTTP/1.1 200 response served on every health probe.
fn health_http_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body,
    )
}

/// Minimal health server. Returns `200 ok` for any request on `/livez` and
/// `/readyz` (the path is not inspected — a connection that completes a request
/// is treated as healthy). Runs until the process exits.
pub async fn serve_health(port: u16) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            error!(error = %e, port, "failed to bind health port");
            return;
        }
    };
    info!(port, "health server listening");

    loop {
        let (mut socket, _) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "health accept failed");
                continue;
            }
        };
        tokio::spawn(async move {
            let mut buf = [0u8; HEALTH_READ_BUF_SIZE];
            let _ = socket.read(&mut buf).await;
            let response = health_http_response(HEALTH_BODY);
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
    }
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
