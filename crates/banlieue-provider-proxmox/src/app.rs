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

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{Provider, VMImage};
use banlieue_api::infrastructure::ProxmoxMachine;
use banlieue_provider_sdk::bootstrap::{
    Observability, init_tracing, shutdown_signal, start_observability,
};
use banlieue_provider_sdk::client::build_client_with;
use banlieue_provider_sdk::defaults::{
    DEFAULT_HEALTH_PORT, DEFAULT_LEADER_ELECTION_NAMESPACE, DEFAULT_METRICS_PORT,
};
use banlieue_provider_sdk::health::Election;
use banlieue_provider_sdk::leader::{LeaderConfig, elect};
use banlieue_provider_sdk::reconciler::{provider_watch_config, requeue_error_policy};
use banlieue_provider_sdk::runner::run_controller;
use banlieue_provider_sdk::scope::{ProviderScope, start_gate};
use clap::Args;
use kube::{
    Api,
    runtime::{Controller, watcher::Config},
};
use tracing::info;

use crate::client::{HttpClientFactory, install_default_crypto_provider};
use crate::context::Context;
use crate::reconciler::{image, provider, proxmoxmachine};

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

    /// Home namespace: where the provider reads its own `Provider`,
    /// credentials and CA bundle. Defaults to the namespace it runs in; never
    /// every namespace (ADR-0131).
    #[arg(long, env = "BANLIEUE_NAMESPACE")]
    pub namespace: Option<String>,

    /// Also serve `ProxmoxMachine`s in every namespace whose labels match this
    /// selector (ADR-0133). The operator renders it from
    /// `Provider.spec.namespaceSelector`. Unset means the home namespace only;
    /// never every namespace.
    #[arg(long, env = "BANLIEUE_WATCH_NAMESPACE_SELECTOR")]
    pub watch_namespace_selector: Option<String>,

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

    // ADR-0131: never every namespace. The home namespace is `--namespace`, else
    // the namespace this provider runs in; ADR-0133 adds the namespaces a
    // label selector picks, for machines only.
    let scope = ProviderScope::resolve(
        cli.namespace.as_deref(),
        client.default_namespace(),
        cli.watch_namespace_selector.as_deref(),
    )
    .context("resolving the namespace to serve")?;
    let namespace = scope.home().to_string();
    info!(scope = %scope.describe(), "serving Providers in the home namespace, machines in");
    let leader_cfg = (!cli.no_leader_elect).then(|| build_leader_config(&cli));
    let election = leader_cfg
        .as_ref()
        .map_or(Election::Disabled, LeaderConfig::election);
    let Observability { metrics, readiness } =
        start_observability(ROLE, cli.health_port, cli.metrics_port, election)
            .await
            .context("starting health and metrics servers")?;

    elect(client.clone(), leader_cfg, &readiness)
        .await
        .context("acquiring leader lease")?;

    // Selector mode (ADR-0133): watch machines in every namespace and gate each
    // reconcile on a Namespace reflector the API server has already filtered by
    // the selector. An invalid selector fails startup here, not later as a
    // provider that serves nothing.
    let (gate, scope_changes) = start_gate(
        &client,
        scope.selector(),
        Some("--watch-namespace-selector"),
    )
    .await?;

    let ctx = Arc::new(
        Context::new(
            client.clone(),
            Some(namespace.clone()),
            Arc::new(HttpClientFactory),
            cli.provider_name.clone(),
        )
        .with_scope(scope.clone(), gate),
    );

    info!("starting Provider controller (class=proxmox)");
    let provider_api: Api<Provider> = Api::namespaced(client.clone(), &namespace);
    let provider_ctrl = run_controller(
        Controller::new(
            provider_api,
            provider_watch_config(cli.provider_name.as_deref()),
        ),
        "Provider",
        metrics.clone(),
        provider::reconcile,
        requeue_error_policy,
        ctx.clone(),
    );

    // VMImage is cluster-scoped: always watch every namespace.
    let image_api: Api<VMImage> = Api::all(client.clone());
    let image_ctrl = run_controller(
        Controller::new(image_api, Config::default()),
        "VMImage",
        metrics.clone(),
        image::reconcile,
        requeue_error_policy,
        ctx.clone(),
    );

    info!("starting ProxmoxMachine controller");
    // ProxmoxMachine follows the scope: the home namespace, or every namespace
    // behind the selector gate (ADR-0133). The Provider above never does.
    let machine_api: Api<ProxmoxMachine> = scope.api(&client);
    let machine_ctrl = run_controller(
        Controller::new(machine_api, Config::default()).reconcile_all_on(scope_changes.trigger()),
        "ProxmoxMachine",
        metrics.clone(),
        proxmoxmachine::reconcile,
        requeue_error_policy,
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

/// Build a [`LeaderConfig`] from parsed CLI flags.
fn build_leader_config(cli: &Cli) -> LeaderConfig {
    LeaderConfig::new(&cli.leader_election_namespace, &cli.leader_election_id)
        .with_identity(cli.leader_election_identity.clone())
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod app_tests;
