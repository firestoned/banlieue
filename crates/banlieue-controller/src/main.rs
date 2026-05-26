// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-controller entry point
//!
//! 1. Parses CLI flags (with `BANLIEUE_*` env-var fallbacks).
//! 2. Initialises structured logging via `tracing`.
//! 3. Builds a [`kube::Client`] via [`banlieue_provider_sdk::client`].
//! 4. Starts a tiny health server on `:HEALTH_PORT` (livez + readyz).
//! 5. (Unless `--no-leader-elect`) acquires the
//!    `coordination.k8s.io/v1.Lease` named `--leader-election-id`
//!    before any reconciler runs; spawns a background renewer.
//! 6. Runs the [`kube::runtime::Controller`] for `VirtualMachine`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{VMImage, VirtualMachine};
use banlieue_api::infrastructure::VSphereMachine;
use banlieue_provider_sdk::client::build_client;
use banlieue_provider_sdk::leader::{
    DEFAULT_LEASE_DURATION_SECS, DEFAULT_RENEW_PERIOD_SECS, DEFAULT_RETRY_PERIOD_SECS,
    LeaderConfig, acquire_or_wait, renew_forever,
};
use clap::Parser;
use futures::StreamExt;
use kube::{
    Api, ResourceExt,
    runtime::{Controller, reflector::ObjectRef, watcher::Config},
};
use tracing::{error, info};

use banlieue_controller::{
    context::Context,
    reconciler::virtualmachine::{error_policy, reconcile},
};

const DEFAULT_HEALTH_PORT: u16 = 8081;
const DEFAULT_METRICS_PORT: u16 = 8080;
const DEFAULT_LEADER_ELECTION_NAMESPACE: &str = "banlieue-system";
const DEFAULT_LEADER_ELECTION_ID: &str = "banlieue-controller";

/// Command-line interface for the banlieue main controller.
#[derive(Debug, Parser)]
#[command(name = "banlieue-controller", version, about, long_about = None)]
struct Cli {
    /// Path to a kubeconfig file. Falls back to in-cluster config or
    /// `$KUBECONFIG` / `~/.kube/config` when unset.
    #[arg(long, env = "KUBECONFIG")]
    kubeconfig: Option<String>,

    /// Restrict the controller to a single namespace. Cluster-wide when unset.
    #[arg(long, env = "BANLIEUE_NAMESPACE")]
    namespace: Option<String>,

    /// Health server bind port.
    #[arg(long, env = "BANLIEUE_HEALTH_PORT", default_value_t = DEFAULT_HEALTH_PORT)]
    health_port: u16,

    /// Metrics server bind port (Phase 4 will populate; the port is reserved now).
    #[arg(long, env = "BANLIEUE_METRICS_PORT", default_value_t = DEFAULT_METRICS_PORT)]
    metrics_port: u16,

    /// Log format: `json` for SIEM-friendly output, `text` for human-readable
    /// (local development).
    #[arg(long, env = "RUST_LOG_FORMAT", default_value = "text")]
    log_format: String,

    /// Log level (`error`, `warn`, `info`, `debug`, `trace`). Overrides
    /// `RUST_LOG`; ignored if `RUST_LOG` is unset and this flag is also unset.
    #[arg(long, env = "BANLIEUE_LOG_LEVEL")]
    log_level: Option<String>,

    /// Disable leader election. Default is to elect a leader before
    /// running reconcilers, so multiple replicas can run safely.
    #[arg(long, env = "BANLIEUE_NO_LEADER_ELECT", default_value_t = false)]
    no_leader_elect: bool,

    /// Namespace the leader-election Lease lives in.
    #[arg(
        long,
        env = "BANLIEUE_LEADER_ELECTION_NAMESPACE",
        default_value = DEFAULT_LEADER_ELECTION_NAMESPACE,
    )]
    leader_election_namespace: String,

    /// Lease object name (the lock).
    #[arg(
        long,
        env = "BANLIEUE_LEADER_ELECTION_ID",
        default_value = DEFAULT_LEADER_ELECTION_ID,
    )]
    leader_election_id: String,

    /// Holder identity to write into the Lease. Falls back to `POD_NAME` /
    /// `HOSTNAME` / "unknown" if unset.
    #[arg(long, env = "BANLIEUE_LEADER_ELECTION_IDENTITY")]
    leader_election_identity: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    init_tracing(&cli.log_format, cli.log_level.as_deref())?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        namespace = ?cli.namespace,
        leader_elect = !cli.no_leader_elect,
        "banlieue-controller starting"
    );

    let client = build_client().await.context("constructing kube client")?;

    tokio::spawn(serve_health(cli.health_port));

    if !cli.no_leader_elect {
        let leader_cfg = build_leader_config(&cli);
        info!(
            namespace = %leader_cfg.namespace,
            lease = %leader_cfg.lease_name,
            identity = %leader_cfg.identity,
            "waiting for leader election"
        );
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

    let ctx = Arc::new(Context::new(client.clone(), cli.namespace.clone()));

    let vm_api: Api<VirtualMachine> = match cli.namespace.as_deref() {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };

    // Infra CRs we own (status-mirror feedback). `owns` follows
    // ownerReferences back to the parent VirtualMachine for fast event-driven
    // reconciles when the provider updates infra status.
    let vsphere_api: Api<VSphereMachine> = match cli.namespace.as_deref() {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };

    // VMImage is cluster-scoped; the image watcher requeues every VM
    // referencing an image whose status flipped.
    let image_api: Api<VMImage> = Api::all(client.clone());

    info!("starting VirtualMachine controller");
    let controller = Controller::new(vm_api, Config::default());
    let vm_store = controller.store();

    let controller_fut = controller
        .owns(vsphere_api, Config::default())
        .watches(image_api, Config::default(), move |image: VMImage| {
            // Requeue every VM whose spec.image_ref.name matches this image.
            // VMImage updates are rare (operator-driven template imports), so
            // the linear scan over the store is fine.
            let image_name = image.name_any();
            vm_store
                .state()
                .into_iter()
                .filter(move |vm| vm.spec.image_ref.name == image_name)
                .map(|vm| ObjectRef::from_obj(vm.as_ref()))
                .collect::<Vec<_>>()
        })
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => info!(?obj, "reconciled"),
                Err(e) => error!(error = %e, "reconcile error"),
            }
        });

    tokio::select! {
        () = controller_fut => {
            info!("controller stream ended");
        }
        _ = shutdown_signal() => {
            info!("shutdown signal received; releasing controller");
        }
    }

    Ok(())
}

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

/// Wait for SIGTERM (containers) or Ctrl-C (local dev). Resolves the
/// first one that fires; the controller will then exit its select.
async fn shutdown_signal() {
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

fn init_tracing(format: &str, level: Option<&str>) -> Result<()> {
    use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

    let filter = if let Some(lvl) = level {
        // Explicit --log-level takes precedence over RUST_LOG.
        EnvFilter::try_new(format!("{lvl},kube=warn"))
            .map_err(|e| anyhow::anyhow!("invalid --log-level {lvl:?}: {e}"))?
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,kube=warn"))
    };

    let registry = tracing_subscriber::registry().with(filter);

    match format {
        "json" => registry
            .with(tracing_subscriber::fmt::layer().json())
            .try_init()
            .map_err(|e| anyhow::anyhow!("init json tracing: {e}")),
        _ => registry
            .with(tracing_subscriber::fmt::layer())
            .try_init()
            .map_err(|e| anyhow::anyhow!("init text tracing: {e}")),
    }
}

/// Minimal health server. Returns 200 on `/livez` and `/readyz`.
async fn serve_health(port: u16) {
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
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let body = "ok";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body,
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
    }
}
