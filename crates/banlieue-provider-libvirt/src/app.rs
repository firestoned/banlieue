// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # `banlieue provider libvirt` entry point
//!
//! Library form of the libvirt provider, invoked by the unified `banlieue`
//! binary (ADR-0004). [`run`] owns the full lifecycle: tracing, kube client,
//! health and metrics servers (ADR-0093, ADR-0091), leader election, then the
//! `Provider`, `VMImage` and `LibvirtMachine` controllers through the SDK's
//! instrumented [`run_controller`].

use std::sync::Arc;

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{Provider, VMImage};
use banlieue_api::infrastructure::LibvirtMachine;
use banlieue_provider_sdk::bootstrap::{
    Observability, init_tracing, shutdown_signal, start_observability,
};
use banlieue_provider_sdk::client::build_client_with;
use banlieue_provider_sdk::defaults::{
    DEFAULT_BUILD_NAMESPACE, DEFAULT_HEALTH_PORT, DEFAULT_IMPORT_IMAGE,
    DEFAULT_IMPORT_SERVICE_ACCOUNT, DEFAULT_LEADER_ELECTION_NAMESPACE, DEFAULT_METRICS_PORT,
};
use banlieue_provider_sdk::health::Election;
use banlieue_provider_sdk::leader::{LeaderConfig, elect};
use banlieue_provider_sdk::reconciler::{provider_watch_config, requeue_error_policy};
use banlieue_provider_sdk::runner::run_controller;
use banlieue_provider_sdk::scope::{ProviderScope, start_gate};
use clap::{Args, Subcommand};
use kube::{
    Api,
    runtime::{Controller, watcher::Config},
};
use tracing::info;

use crate::{
    client::TlsClientFactory,
    context::Context,
    import::ImportArgs,
    machine_client::TlsMachineClientFactory,
    reconciler::{libvirtmachine, provider, vmimage},
};

const DEFAULT_LEADER_ELECTION_ID: &str = "banlieue-provider-libvirt";

/// This role's name: the OTLP `service.name` (ADR-0092) and the `role`
/// label of `banlieue_leader` (ADR-0091).
pub const ROLE: &str = "banlieue-provider-libvirt";
/// Per-crate `tracing` directives layered on top of the base log level.
const LOG_DIRECTIVES: &[&str] = &["kube=warn"];

/// Command-line arguments for `banlieue provider libvirt`.
#[derive(Debug, Args)]
pub struct Cli {
    /// One-shot subcommand. Without one, this runs the controller.
    #[command(subcommand)]
    pub command: Option<LibvirtCommand>,

    /// Kubeconfig path, or a `KUBECONFIG`-style list (also read from
    /// `$KUBECONFIG`). When set it wins over every other source, in-cluster
    /// config included; when unset: in-cluster config, then
    /// `~/.kube/config`.
    #[arg(long, env = "KUBECONFIG")]
    pub kubeconfig: Option<String>,

    /// Namespace the provider serves. Defaults to the namespace it runs in;
    /// never every namespace (ADR-0131).
    #[arg(long, env = "BANLIEUE_NAMESPACE")]
    pub namespace: Option<String>,

    /// Label selector (`key=value,...`) over namespaces whose machines this
    /// provider also serves (ADR-0133). `--namespace` stays the home namespace:
    /// the `Provider`, credentials and CA bundle are read there only. The
    /// operator renders it from `Provider.spec.namespaceSelector`. Unset means
    /// the home namespace only; never every namespace.
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

    /// Namespace holding the artifacts PVC and import Jobs. Must match
    /// banlieue-imagebuilder's --build-namespace.
    #[arg(
        long,
        env = "BANLIEUE_BUILD_NAMESPACE",
        default_value = DEFAULT_BUILD_NAMESPACE,
    )]
    pub build_namespace: String,

    /// Taints import Jobs may tolerate (`key[=value]:Effect`, repeatable).
    ///
    /// There is deliberately no matching node selector. Where an import Job
    /// runs follows from the artifacts PVC it mounts — the scheduler resolves
    /// that from the bound PV — so the only thing banlieue must supply is
    /// permission to land on a node that happens to be tainted, which is the
    /// case when the volume sits on a dedicated build node.
    #[arg(long = "build-toleration", value_name = "KEY[=VALUE]:EFFECT")]
    pub build_toleration: Vec<String>,

    /// Image the import Job runs. Defaults to the banlieue image itself.
    #[arg(long, env = "BANLIEUE_IMPORT_IMAGE", default_value = DEFAULT_IMPORT_IMAGE)]
    pub import_image: String,

    /// ServiceAccount the import Job runs as, in `--build-namespace`.
    ///
    /// Deliberately **not** this controller's own identity: that one can create
    /// Jobs, so a workload in the privileged build namespace holding it could
    /// create further privileged pods. The import identity is read-only, and
    /// the operator grants it access to exactly this Provider and its
    /// credentials (ADR-0016 §4).
    #[arg(
        long,
        env = "BANLIEUE_IMPORT_SERVICE_ACCOUNT",
        default_value = DEFAULT_IMPORT_SERVICE_ACCOUNT,
    )]
    pub import_service_account: String,

    /// Restrict the Provider watch to a single object by name.
    ///
    /// Narrowed **server-side** with a field selector, so this process's
    /// informer cache holds only its own Provider — one process per Provider
    /// instance. The operator always passes this; unset means "every
    /// Provider of this class", which is what a statically installed provider
    /// wants.
    #[arg(long, env = "BANLIEUE_PROVIDER_NAME")]
    pub provider_name: Option<String>,
}

/// Subcommands of `banlieue provider libvirt`.
#[derive(Debug, Subcommand)]
pub enum LibvirtCommand {
    /// Stream a built raw disk into a libvirt storage volume and exit.
    ///
    /// Runs inside the Job the `VMImage` reconciler creates; not
    /// normally invoked by hand, though the flags are stable so a failed
    /// import can be reproduced.
    Import(ImportArgs),
}

/// Run the libvirt provider to completion.
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

    // One-shot roles exit when their work is done; only the controller path
    // below needs a health server, a leader lease, or a watch.
    if let Some(LibvirtCommand::Import(args)) = cli.command {
        return crate::import::run(args).await;
    }

    info!(
        version = env!("CARGO_PKG_VERSION"),
        namespace = ?cli.namespace,
        provider_name = ?cli.provider_name,
        leader_elect = !cli.no_leader_elect,
        "banlieue-provider-libvirt starting"
    );

    let client = build_client_with(cli.kubeconfig.as_deref().map(std::ffi::OsStr::new))
        .await
        .context("constructing kube client")?;

    // ADR-0131: one namespace, never every namespace. `--namespace` wins, else
    // the namespace this provider runs in.
    // ADR-0133 adds the namespaces a label selector picks, for machines only.
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

    // Parsed once at startup: a malformed toleration must fail the process
    // rather than surface later as an unschedulable Job.
    let import_tolerations =
        banlieue_provider_sdk::scheduling::parse_tolerations(&cli.build_toleration)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    // ADR-0133: with a selector, gate machine reconciles on a Namespace reflector
    // the API server has already filtered by it. An invalid selector fails
    // startup here rather than later as a provider that serves nothing.
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
            Arc::new(TlsClientFactory::new()),
            Arc::new(TlsMachineClientFactory::new()),
            cli.build_namespace.clone(),
            cli.import_image.clone(),
            cli.import_service_account.clone(),
            import_tolerations,
        )
        .with_provider_name(cli.provider_name.clone())
        .with_scope(scope.clone(), gate),
    );

    let provider_api: Api<Provider> = Api::namespaced(client.clone(), &namespace);

    info!("starting Provider controller (class=libvirt)");
    let ctx2 = ctx.clone();
    let ctx3 = ctx.clone();
    let provider_ctrl = run_controller(
        Controller::new(
            provider_api,
            provider_watch_config(cli.provider_name.as_deref()),
        ),
        "Provider",
        metrics.clone(),
        provider::reconcile,
        requeue_error_policy,
        ctx,
    );

    // VMImage is cluster-scoped: always watch every namespace.
    let image_api: Api<VMImage> = Api::all(client.clone());
    let image_ctrl = run_controller(
        Controller::new(image_api, Config::default()),
        "VMImage",
        metrics.clone(),
        vmimage::reconcile,
        requeue_error_policy,
        ctx2,
    );

    // LibvirtMachine — the VM lifecycle (ADR-0050). It follows the scope: the
    // home namespace, or every namespace behind the selector gate (ADR-0133).
    // The Provider above never does: its credentials are read from the home
    // namespace only.
    info!("starting LibvirtMachine controller");
    let machine_api: Api<LibvirtMachine> = scope.api(&client);
    let machine_ctrl = run_controller(
        Controller::new(machine_api, Config::default()).reconcile_all_on(scope_changes.trigger()),
        "LibvirtMachine",
        metrics.clone(),
        libvirtmachine::reconcile,
        requeue_error_policy,
        ctx3,
    );

    readiness.controllers_started();

    tokio::select! {
        () = provider_ctrl => info!("Provider controller stream ended"),
        () = image_ctrl => info!("VMImage controller stream ended"),
        () = machine_ctrl => info!("LibvirtMachine controller stream ended"),
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
