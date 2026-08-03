// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # `banlieue provider libvirt` entry point
//!
//! Library form of the libvirt provider, invoked by the unified `banlieue`
//! binary (ADR-0004). [`run`] owns the full lifecycle: tracing, kube client,
//! health server, leader election, then the `Provider` controller.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{Provider, VMImage};
use banlieue_provider_sdk::bootstrap::{init_tracing, serve_health, shutdown_signal};
use banlieue_provider_sdk::client::build_client;
use banlieue_provider_sdk::leader::{
    DEFAULT_LEASE_DURATION_SECS, DEFAULT_RENEW_PERIOD_SECS, DEFAULT_RETRY_PERIOD_SECS,
    LeaderConfig, acquire_or_wait, renew_forever,
};
use clap::{Args, Subcommand};
use futures::StreamExt;
use kube::{
    Api,
    runtime::{Controller, watcher::Config},
};
use tracing::{error, info};

use crate::{
    client::TlsClientFactory,
    context::Context,
    import::ImportArgs,
    reconciler::{provider, vmimage},
};

const DEFAULT_HEALTH_PORT: u16 = 8081;
const DEFAULT_METRICS_PORT: u16 = 8080;
const DEFAULT_LEADER_ELECTION_NAMESPACE: &str = "banlieue-system";
const DEFAULT_LEADER_ELECTION_ID: &str = "banlieue-provider-libvirt";
// The namespace image builds run in. Deliberately NOT `banlieue-system`:
// kairos-operator's OSArtifact build pods require `privileged: true`, which
// `baseline` denies as well as `restricted`, so this namespace enforces
// `privileged` — no admission floor. Confining that exception keeps the
// control-plane namespace restricted (ADR-0016).
//
// Must match every other component's default: the imagebuilder creates the
// artifacts PVC here and each provider's import Job mounts it, and a PVC
// cannot be mounted across namespaces. A cross-crate test in the `banlieue`
// binary asserts the defaults agree.
const DEFAULT_BUILD_NAMESPACE: &str = "banlieue-imagebuild";
const DEFAULT_IMPORT_IMAGE: &str = "ghcr.io/firestoned/banlieue:v0.1.0";
/// Identity import Jobs run as, created in the build namespace by
/// `banlieue bootstrap imagebuilder` (ADR-0016 §4).
const DEFAULT_IMPORT_SERVICE_ACCOUNT: &str = "banlieue-import";

/// Per-crate `tracing` directives layered on top of the base log level.
const LOG_DIRECTIVES: &[&str] = &["kube=warn"];

/// Command-line arguments for `banlieue provider libvirt`.
#[derive(Debug, Args)]
pub struct Cli {
    /// One-shot subcommand. Without one, this runs the controller.
    #[command(subcommand)]
    pub command: Option<LibvirtCommand>,

    /// Path to a kubeconfig file. Falls back to in-cluster config.
    #[arg(long, env = "KUBECONFIG")]
    pub kubeconfig: Option<String>,

    /// Restrict the provider to a single namespace. Cluster-wide when unset.
    #[arg(long, env = "BANLIEUE_NAMESPACE")]
    pub namespace: Option<String>,

    /// Health server bind port.
    #[arg(long, env = "BANLIEUE_HEALTH_PORT", default_value_t = DEFAULT_HEALTH_PORT)]
    pub health_port: u16,

    /// Metrics server bind port (reserved).
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

/// Watch configuration for the `Provider` controller.
///
/// Standalone so the scoping rule is unit-testable without a cluster.
#[must_use]
pub fn provider_watch_config(provider_name: Option<&str>) -> Config {
    match provider_name {
        Some(name) => Config::default().fields(&format!("metadata.name={name}")),
        None => Config::default(),
    }
}

/// Run the libvirt provider to completion.
///
/// # Errors
/// Returns an error if logging init, kube client construction, or leader-lease
/// acquisition fails.
pub async fn run(cli: Cli) -> Result<()> {
    init_tracing(&cli.log_format, cli.log_level.as_deref(), LOG_DIRECTIVES)
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

    let client = build_client().await.context("constructing kube client")?;
    tokio::spawn(serve_health(cli.health_port));

    if !cli.no_leader_elect {
        let leader_cfg = build_leader_config(&cli);
        info!(lease = %leader_cfg.lease_name, "waiting for leader election");
        acquire_or_wait(client.clone(), &leader_cfg)
            .await
            .context("acquiring leader lease")?;
        let renewer_client = client.clone();
        tokio::spawn(async move {
            if let Err(e) = renew_forever(renewer_client, leader_cfg).await {
                error!(error = %e, "leader lease renewer terminated — exiting");
                std::process::exit(1);
            }
        });
    } else {
        info!("leader election disabled by --no-leader-elect");
    }

    // Parsed once at startup: a malformed toleration must fail the process
    // rather than surface later as an unschedulable Job.
    let import_tolerations =
        banlieue_provider_sdk::scheduling::parse_tolerations(&cli.build_toleration)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    let ctx = Arc::new(Context::new(
        client.clone(),
        cli.namespace.clone(),
        Arc::new(TlsClientFactory::new()),
        cli.build_namespace.clone(),
        cli.import_image.clone(),
        cli.import_service_account.clone(),
        import_tolerations,
    ));

    let provider_api: Api<Provider> = match cli.namespace.as_deref() {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };

    info!("starting Provider controller (class=libvirt)");
    let ctx2 = ctx.clone();
    let provider_ctrl = Controller::new(
        provider_api,
        provider_watch_config(cli.provider_name.as_deref()),
    )
    .run(provider::reconcile, provider::error_policy, ctx)
    .for_each(|res| async move {
        match res {
            Ok((obj, _)) => info!(kind = "Provider", ?obj, "reconciled"),
            Err(e) => error!(kind = "Provider", error = %e, "reconcile error"),
        }
    });

    // VMImage is cluster-scoped: always watch every namespace.
    let image_api: Api<VMImage> = Api::all(client.clone());
    let image_ctrl = Controller::new(image_api, Config::default())
        .run(vmimage::reconcile, vmimage::error_policy, ctx2)
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => info!(kind = "VMImage", ?obj, "reconciled"),
                Err(e) => error!(kind = "VMImage", error = %e, "reconcile error"),
            }
        });

    tokio::select! {
        () = provider_ctrl => info!("Provider controller stream ended"),
        () = image_ctrl => info!("VMImage controller stream ended"),
        _ = shutdown_signal() => info!("shutdown signal received; releasing controllers"),
    }
    Ok(())
}

/// Build a [`LeaderConfig`] from parsed CLI flags.
fn build_leader_config(cli: &Cli) -> LeaderConfig {
    let identity = cli
        .leader_election_identity
        .clone()
        .unwrap_or_else(LeaderConfig::default_identity);
    LeaderConfig {
        namespace: cli.leader_election_namespace.clone(),
        lease_name: cli.leader_election_id.clone(),
        identity,
        lease_duration: Duration::from_secs(DEFAULT_LEASE_DURATION_SECS),
        renew_period: Duration::from_secs(DEFAULT_RENEW_PERIOD_SECS),
        retry_period: Duration::from_secs(DEFAULT_RETRY_PERIOD_SECS),
    }
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod app_tests;
