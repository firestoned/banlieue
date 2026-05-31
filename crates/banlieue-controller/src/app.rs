// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # `banlieue controller` entry point
//!
//! This is the library form of the main controller, invoked by the unified
//! `banlieue` binary as `banlieue controller` (see ADR-0004). [`run`] owns the
//! full lifecycle:
//!
//! 1. Initialises structured logging via [`banlieue_provider_sdk::bootstrap`].
//! 2. Builds a [`kube::Client`] via [`banlieue_provider_sdk::client`].
//! 3. Starts a tiny health server on `:health_port` (livez + readyz).
//! 4. (Unless `--no-leader-elect`) acquires the
//!    `coordination.k8s.io/v1.Lease` named `--leader-election-id`
//!    before any reconciler runs; spawns a background renewer.
//! 5. Runs the [`kube::runtime::Controller`]s for `VirtualMachine` and
//!    `VSphereCluster`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{Provider, VMImage, VirtualMachine};
use banlieue_api::infrastructure::{VSphereCluster, VSphereMachine};
use banlieue_provider_sdk::bootstrap::{init_tracing, serve_health, shutdown_signal};
use banlieue_provider_sdk::client::build_client;
use banlieue_provider_sdk::leader::{
    DEFAULT_LEASE_DURATION_SECS, DEFAULT_RENEW_PERIOD_SECS, DEFAULT_RETRY_PERIOD_SECS,
    LeaderConfig, acquire_or_wait, renew_forever,
};
use clap::Args;
use futures::StreamExt;
use kube::{
    Api, ResourceExt,
    runtime::{Controller, reflector::ObjectRef, watcher::Config},
};
use tracing::{error, info};

use crate::{
    context::Context,
    reconciler::virtualmachine::{error_policy, reconcile},
    reconciler::vsphere_cluster,
};

const DEFAULT_HEALTH_PORT: u16 = 8081;
const DEFAULT_METRICS_PORT: u16 = 8080;
const DEFAULT_LEADER_ELECTION_NAMESPACE: &str = "banlieue-system";
const DEFAULT_LEADER_ELECTION_ID: &str = "banlieue-controller";

/// Per-crate `tracing` directives layered on top of the base log level.
const LOG_DIRECTIVES: &[&str] = &["kube=warn"];

/// Command-line arguments for `banlieue controller`.
#[derive(Debug, Args)]
pub struct Cli {
    /// Path to a kubeconfig file. Falls back to in-cluster config or
    /// `$KUBECONFIG` / `~/.kube/config` when unset.
    #[arg(long, env = "KUBECONFIG")]
    pub kubeconfig: Option<String>,

    /// Restrict the controller to a single namespace. Cluster-wide when unset.
    #[arg(long, env = "BANLIEUE_NAMESPACE")]
    pub namespace: Option<String>,

    /// Health server bind port.
    #[arg(long, env = "BANLIEUE_HEALTH_PORT", default_value_t = DEFAULT_HEALTH_PORT)]
    pub health_port: u16,

    /// Metrics server bind port (Phase 4 will populate; the port is reserved now).
    #[arg(long, env = "BANLIEUE_METRICS_PORT", default_value_t = DEFAULT_METRICS_PORT)]
    pub metrics_port: u16,

    /// Log format: `json` for SIEM-friendly output, `text` for human-readable
    /// (local development).
    #[arg(long, env = "RUST_LOG_FORMAT", default_value = "text")]
    pub log_format: String,

    /// Log level (`error`, `warn`, `info`, `debug`, `trace`). Overrides
    /// `RUST_LOG`; ignored if `RUST_LOG` is unset and this flag is also unset.
    #[arg(long, env = "BANLIEUE_LOG_LEVEL")]
    pub log_level: Option<String>,

    /// Disable leader election. Default is to elect a leader before
    /// running reconcilers, so multiple replicas can run safely.
    #[arg(long, env = "BANLIEUE_NO_LEADER_ELECT", default_value_t = false)]
    pub no_leader_elect: bool,

    /// Namespace the leader-election Lease lives in.
    #[arg(
        long,
        env = "BANLIEUE_LEADER_ELECTION_NAMESPACE",
        default_value = DEFAULT_LEADER_ELECTION_NAMESPACE,
    )]
    pub leader_election_namespace: String,

    /// Lease object name (the lock).
    #[arg(
        long,
        env = "BANLIEUE_LEADER_ELECTION_ID",
        default_value = DEFAULT_LEADER_ELECTION_ID,
    )]
    pub leader_election_id: String,

    /// Holder identity to write into the Lease. Falls back to `POD_NAME` /
    /// `HOSTNAME` / "unknown" if unset.
    #[arg(long, env = "BANLIEUE_LEADER_ELECTION_IDENTITY")]
    pub leader_election_identity: Option<String>,
}

/// Run the main banlieue controller to completion (until a shutdown signal or a
/// controller stream ends).
///
/// # Arguments
/// * `cli` - parsed `banlieue controller` arguments.
///
/// # Errors
/// Returns an error if logging init, kube client construction, or leader-lease
/// acquisition fails.
pub async fn run(cli: Cli) -> Result<()> {
    init_tracing(&cli.log_format, cli.log_level.as_deref(), LOG_DIRECTIVES)
        .context("initialising tracing")?;
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
        .run(reconcile, error_policy, ctx.clone())
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => info!(?obj, "reconciled"),
                Err(e) => error!(error = %e, "reconcile error"),
            }
        });

    // VSphereCluster (CAPI InfraCluster) controller. Aggregates Provider
    // failure domains into the CAPI status. A Provider's status changing
    // (failure domains added/removed) requeues every VSphereCluster — these
    // events are rare and operator-driven, so requeuing all is cheap.
    let vsc_api: Api<VSphereCluster> = match cli.namespace.as_deref() {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };
    let provider_api: Api<Provider> = match cli.namespace.as_deref() {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };

    info!("starting VSphereCluster controller");
    let vsc_controller = Controller::new(vsc_api, Config::default());
    let vsc_store = vsc_controller.store();

    let vsc_fut = vsc_controller
        .watches(
            provider_api,
            Config::default(),
            move |_provider: Provider| {
                vsc_store
                    .state()
                    .into_iter()
                    .map(|c| ObjectRef::from_obj(c.as_ref()))
                    .collect::<Vec<_>>()
            },
        )
        .run(
            vsphere_cluster::reconcile,
            vsphere_cluster::error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => info!(kind = "VSphereCluster", ?obj, "reconciled"),
                Err(e) => error!(kind = "VSphereCluster", error = %e, "reconcile error"),
            }
        });

    tokio::select! {
        () = controller_fut => {
            info!("VirtualMachine controller stream ended");
        }
        () = vsc_fut => {
            info!("VSphereCluster controller stream ended");
        }
        _ = shutdown_signal() => {
            info!("shutdown signal received; releasing controllers");
        }
    }

    Ok(())
}

/// Build a [`LeaderConfig`] from parsed CLI flags, filling the holder identity
/// from `--leader-election-identity` or the `POD_NAME` / `HOSTNAME` fallback.
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
