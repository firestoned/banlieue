// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # `banlieue provider proxmox` entry point
//!
//! Library form of the Proxmox provider, invoked by the unified `banlieue`
//! binary (ADR-0004). [`run`] owns the full lifecycle: tracing, the rustls
//! crypto provider, kube client, health and metrics servers (ADR-0093,
//! ADR-0091), leader election, then the `Provider`, `VMImage` and
//! `ProxmoxMachine` controllers through the SDK's instrumented
//! [`run_controller`]. All three are watch-driven: nothing here polls.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{Provider, VMImage};
use banlieue_api::infrastructure::ProxmoxMachine;
use banlieue_provider_sdk::bootstrap::{
    Observability, init_tracing, shutdown_signal, start_observability,
};
use banlieue_provider_sdk::client::build_client_with;
use banlieue_provider_sdk::health::Election;
use banlieue_provider_sdk::leader::{
    DEFAULT_LEASE_DURATION_SECS, DEFAULT_RENEW_PERIOD_SECS, DEFAULT_RETRY_PERIOD_SECS,
    LeaderConfig, acquire_or_wait, renew_forever,
};
use banlieue_provider_sdk::runner::run_controller;
use clap::Args;
use kube::{
    Api,
    runtime::{Controller, watcher::Config},
};
use tracing::{error, info};

use crate::client::{HttpClientFactory, install_default_crypto_provider};
use crate::context::Context;
use crate::reconciler::{image, provider, proxmoxmachine};

const DEFAULT_HEALTH_PORT: u16 = 8081;
const DEFAULT_METRICS_PORT: u16 = 8080;
const DEFAULT_LEADER_ELECTION_NAMESPACE: &str = "banlieue-system";
const DEFAULT_LEADER_ELECTION_ID: &str = "banlieue-provider-proxmox";

/// This role's name: the OTLP `service.name` (ADR-0092) and the `role`
/// label of `banlieue_leader` (ADR-0091).
pub const ROLE: &str = "banlieue-provider-proxmox";
/// Per-crate `tracing` directives layered on top of the base log level.
const LOG_DIRECTIVES: &[&str] = &["kube=warn"];

/// Command-line arguments for `banlieue provider proxmox`.
#[derive(Debug, Args)]
pub struct Cli {
    /// Kubeconfig path, or a `KUBECONFIG`-style list. When set it wins over
    /// every other source; otherwise in-cluster config, then `~/.kube/config`.
    #[arg(long, env = "KUBECONFIG")]
    pub kubeconfig: Option<String>,

    /// Restrict the provider to a single namespace. Cluster-wide when unset.
    #[arg(long, env = "BANLIEUE_NAMESPACE")]
    pub namespace: Option<String>,

    /// Health server bind port.
    #[arg(long, env = "BANLIEUE_HEALTH_PORT", default_value_t = DEFAULT_HEALTH_PORT)]
    pub health_port: u16,

    /// Metrics server bind port: Prometheus `GET /metrics` (ADR-0091).
    #[arg(long, env = "BANLIEUE_METRICS_PORT", default_value_t = DEFAULT_METRICS_PORT)]
    pub metrics_port: u16,

    /// Log format: `json` for SIEM-friendly output, `text` for local dev.
    #[arg(long, env = "RUST_LOG_FORMAT", default_value = "text")]
    pub log_format: String,

    /// Log level. Overrides `RUST_LOG`.
    #[arg(long, env = "BANLIEUE_LOG_LEVEL")]
    pub log_level: Option<String>,

    /// Disable leader election.
    #[arg(long, env = "BANLIEUE_NO_LEADER_ELECT", default_value_t = false)]
    pub no_leader_elect: bool,

    /// Namespace the leader-election Lease lives in.
    #[arg(
        long,
        env = "BANLIEUE_LEADER_ELECTION_NAMESPACE",
        default_value = DEFAULT_LEADER_ELECTION_NAMESPACE,
    )]
    pub leader_election_namespace: String,

    /// Lease object name.
    #[arg(
        long,
        env = "BANLIEUE_LEADER_ELECTION_ID",
        default_value = DEFAULT_LEADER_ELECTION_ID,
    )]
    pub leader_election_id: String,

    /// Holder identity. Falls back to `POD_NAME` / `HOSTNAME`.
    #[arg(long, env = "BANLIEUE_LEADER_ELECTION_IDENTITY")]
    pub leader_election_identity: Option<String>,

    /// Restrict the Provider watch to a single object by name, narrowed
    /// **server-side** with a field selector. The operator always passes this
    /// (one process per Provider); unset means every Provider of this class.
    #[arg(long, env = "BANLIEUE_PROVIDER_NAME")]
    pub provider_name: Option<String>,

    /// Accepted and ignored. The operator passes the same command line to
    /// every backend; the Proxmox provider spawns no Jobs, so has no use for
    /// an import image.
    #[arg(long, env = "BANLIEUE_IMPORT_IMAGE", hide = true)]
    pub import_image: Option<String>,

    /// Accepted and ignored, for the same reason as `--import-image`.
    #[arg(
        long = "build-toleration",
        value_name = "KEY[=VALUE]:EFFECT",
        hide = true
    )]
    pub build_toleration: Vec<String>,
}

/// Watch configuration for the `Provider` controller, standalone so the
/// scoping rule is unit-testable without a cluster.
#[must_use]
pub fn provider_watch_config(provider_name: Option<&str>) -> Config {
    match provider_name {
        Some(name) => Config::default().fields(&format!("metadata.name={name}")),
        None => Config::default(),
    }
}

/// Run the Proxmox provider to completion.
///
/// # Errors
/// Returns an error if logging init, kube client construction, binding the
/// health or metrics port, or leader-lease acquisition fails.
pub async fn run(cli: Cli) -> Result<()> {
    let telemetry = init_tracing(
        ROLE,
        &cli.log_format,
        cli.log_level.as_deref(),
        LOG_DIRECTIVES,
    )
    .context("initialising tracing")?;
    // Before any TLS: the kube client's and every Proxmox client's.
    install_default_crypto_provider();

    info!(
        version = env!("CARGO_PKG_VERSION"),
        namespace = ?cli.namespace,
        provider_name = ?cli.provider_name,
        leader_elect = !cli.no_leader_elect,
        "banlieue-provider-proxmox starting"
    );

    let client = build_client_with(cli.kubeconfig.as_deref().map(std::ffi::OsStr::new))
        .await
        .context("constructing kube client")?;
    let leader_cfg = (!cli.no_leader_elect).then(|| build_leader_config(&cli));
    let election = leader_cfg
        .as_ref()
        .map_or(Election::Disabled, LeaderConfig::election);
    let Observability { metrics, readiness } =
        start_observability(ROLE, cli.health_port, cli.metrics_port, election)
            .await
            .context("starting health and metrics servers")?;

    if let Some(leader_cfg) = leader_cfg {
        info!(lease = %leader_cfg.lease_name, "waiting for leader election");
        acquire_or_wait(client.clone(), &leader_cfg, &readiness)
            .await
            .context("acquiring leader lease")?;
        let renewer_client = client.clone();
        let renewer_readiness = readiness.clone();
        tokio::spawn(async move {
            if let Err(e) = renew_forever(renewer_client, leader_cfg, renewer_readiness).await {
                error!(error = %e, "leader lease renewer terminated; exiting");
                std::process::exit(1);
            }
        });
    } else {
        info!("leader election disabled by --no-leader-elect");
    }

    let ctx = Arc::new(Context::new(
        client.clone(),
        cli.namespace.clone(),
        Arc::new(HttpClientFactory),
        cli.provider_name.clone(),
    ));

    info!("starting Provider controller (class=proxmox)");
    let provider_api: Api<Provider> = scoped_api(client.clone(), cli.namespace.as_deref());
    let provider_ctrl = run_controller(
        Controller::new(
            provider_api,
            provider_watch_config(cli.provider_name.as_deref()),
        ),
        "Provider",
        metrics.clone(),
        provider::reconcile,
        provider::error_policy,
        ctx.clone(),
    );

    // VMImage is cluster-scoped: always watch every namespace.
    let image_api: Api<VMImage> = Api::all(client.clone());
    let image_ctrl = run_controller(
        Controller::new(image_api, Config::default()),
        "VMImage",
        metrics.clone(),
        image::reconcile,
        image::error_policy,
        ctx.clone(),
    );

    info!("starting ProxmoxMachine controller");
    let machine_api: Api<ProxmoxMachine> = scoped_api(client.clone(), cli.namespace.as_deref());
    let machine_ctrl = run_controller(
        Controller::new(machine_api, Config::default()),
        "ProxmoxMachine",
        metrics.clone(),
        proxmoxmachine::reconcile,
        proxmoxmachine::error_policy,
        ctx,
    );

    readiness.controllers_started();

    tokio::select! {
        () = provider_ctrl => info!("Provider controller stream ended"),
        () = image_ctrl => info!("VMImage controller stream ended"),
        () = machine_ctrl => info!("ProxmoxMachine controller stream ended"),
        _ = shutdown_signal() => info!("shutdown signal received; releasing controllers"),
    }
    telemetry.shutdown();
    Ok(())
}

/// An \`Api\` scoped to \`namespace\`, or cluster-wide when unset.
fn scoped_api<K>(client: kube::Client, namespace: Option<&str>) -> Api<K>
where
    K: kube::Resource<Scope = kube::core::NamespaceResourceScope>,
    K::DynamicType: Default,
{
    match namespace {
        Some(ns) => Api::namespaced(client, ns),
        None => Api::all(client),
    }
}

/// Build a [`LeaderConfig`] from parsed CLI flags.
fn build_leader_config(cli: &Cli) -> LeaderConfig {
    LeaderConfig {
        namespace: cli.leader_election_namespace.clone(),
        lease_name: cli.leader_election_id.clone(),
        identity: cli
            .leader_election_identity
            .clone()
            .unwrap_or_else(LeaderConfig::default_identity),
        lease_duration: Duration::from_secs(DEFAULT_LEASE_DURATION_SECS),
        renew_period: Duration::from_secs(DEFAULT_RENEW_PERIOD_SECS),
        retry_period: Duration::from_secs(DEFAULT_RETRY_PERIOD_SECS),
    }
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod app_tests;
