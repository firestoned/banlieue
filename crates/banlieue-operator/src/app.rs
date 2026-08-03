// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # `banlieue operator` entry point
//!
//! The library form of the provider lifecycle role, invoked by the unified
//! `banlieue` binary as `banlieue operator` (ADR-0004). [`run`] owns the full
//! lifecycle:
//!
//! 1. Initialises structured logging via [`banlieue_provider_sdk::bootstrap`].
//! 2. Builds a [`kube::Client`] via [`banlieue_provider_sdk::client`].
//! 3. Starts a health server on `:health_port` (livez + readyz).
//! 4. (Unless `--no-leader-elect`) acquires the leader Lease before any
//!    reconciler runs; spawns a background renewer.
//! 5. Runs the [`kube::runtime::Controller`] for `Provider`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{Provider, ProviderClass};
use banlieue_provider_sdk::bootstrap::{init_tracing, serve_health, shutdown_signal};
use banlieue_provider_sdk::client::build_client;
use banlieue_provider_sdk::leader::{
    DEFAULT_LEASE_DURATION_SECS, DEFAULT_RENEW_PERIOD_SECS, DEFAULT_RETRY_PERIOD_SECS,
    LeaderConfig, acquire_or_wait, renew_forever,
};
use clap::Args;
use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use kube::{
    Api, ResourceExt,
    runtime::{Controller, reflector::ObjectRef, watcher::Config},
};
use tracing::{error, info};

use crate::{
    context::Context,
    reconciler::{provider, providerclass},
};

const DEFAULT_HEALTH_PORT: u16 = 8081;
const DEFAULT_METRICS_PORT: u16 = 8080;
const DEFAULT_NAMESPACE: &str = "banlieue-system";
const DEFAULT_LEADER_ELECTION_ID: &str = "banlieue-operator";

/// Per-crate `tracing` directives layered on top of the base log level.
const LOG_DIRECTIVES: &[&str] = &["kube=warn"];

/// Command-line arguments for `banlieue operator`.
#[derive(Debug, Args)]
pub struct Cli {
    /// Namespace the operator runs in. Provider workloads are created in their
    /// own Provider's namespace unless the `ProviderClass` pins
    /// `workloadNamespace`, so this is used for the leader Lease and for
    /// reporting — not as a workload target.
    #[arg(long, env = "BANLIEUE_NAMESPACE", default_value = DEFAULT_NAMESPACE)]
    pub namespace: String,

    /// Forwarded to every provider workload as `--build-node-selector`, so its
    /// import Jobs can be scheduled where the artifacts PV lives
    /// (`key=value`, repeatable). See ADR-0016.
    #[arg(long = "build-node-selector", value_name = "KEY=VALUE")]
    pub build_node_selector: Vec<String>,

    /// Forwarded to every provider workload as `--build-toleration`
    /// (`key[=value]:Effect`, repeatable).
    #[arg(long = "build-toleration", value_name = "KEY[=VALUE]:EFFECT")]
    pub build_toleration: Vec<String>,

    /// Restrict the Provider watch to a single namespace. Defaults to watching
    /// every namespace, which is what a cluster-wide operator wants.
    #[arg(long, env = "BANLIEUE_WATCH_NAMESPACE")]
    pub watch_namespace: Option<String>,

    /// Health server bind port.
    #[arg(long, env = "BANLIEUE_HEALTH_PORT", default_value_t = DEFAULT_HEALTH_PORT)]
    pub health_port: u16,

    /// Metrics server bind port (Phase 4 will populate; reserved now).
    #[arg(long, env = "BANLIEUE_METRICS_PORT", default_value_t = DEFAULT_METRICS_PORT)]
    pub metrics_port: u16,

    /// Log format: `json` for SIEM-friendly output, `text` for local dev.
    #[arg(long, env = "RUST_LOG_FORMAT", default_value = "text")]
    pub log_format: String,

    /// Log level (`error`, `warn`, `info`, `debug`, `trace`). Overrides
    /// `RUST_LOG`; ignored if both `RUST_LOG` and this flag are unset.
    #[arg(long, env = "BANLIEUE_LOG_LEVEL")]
    pub log_level: Option<String>,

    /// Disable leader election. Default is to elect a leader before running
    /// reconcilers so multiple replicas can run safely.
    #[arg(long, env = "BANLIEUE_NO_LEADER_ELECT", default_value_t = false)]
    pub no_leader_elect: bool,

    /// Namespace the leader-election Lease lives in.
    #[arg(
        long,
        env = "BANLIEUE_LEADER_ELECTION_NAMESPACE",
        default_value = DEFAULT_NAMESPACE,
    )]
    pub leader_election_namespace: String,

    /// Lease object name.
    #[arg(
        long,
        env = "BANLIEUE_LEADER_ELECTION_ID",
        default_value = DEFAULT_LEADER_ELECTION_ID,
    )]
    pub leader_election_id: String,

    /// Holder identity. Falls back to `POD_NAME` / `HOSTNAME` / "unknown".
    #[arg(long, env = "BANLIEUE_LEADER_ELECTION_IDENTITY")]
    pub leader_election_identity: Option<String>,
}

/// Run `banlieue-operator` to completion (until a shutdown signal or the
/// controller stream ends).
///
/// # Arguments
/// * `cli` - parsed `banlieue operator` arguments.
///
/// # Errors
/// Returns an error if logging init, kube client construction, or
/// leader-lease acquisition fails.
pub async fn run(cli: Cli) -> Result<()> {
    init_tracing(&cli.log_format, cli.log_level.as_deref(), LOG_DIRECTIVES)
        .context("initialising tracing")?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        namespace = %cli.namespace,
        watch_namespace = ?cli.watch_namespace,
        leader_elect = !cli.no_leader_elect,
        "banlieue-operator starting"
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

    let ctx = Arc::new(Context::with_build_scheduling(
        client.clone(),
        cli.namespace.clone(),
        crate::bootstrap::DEFAULT_IMAGEBUILD_NAMESPACE.to_string(),
        cli.build_node_selector.clone(),
        cli.build_toleration.clone(),
    ));

    let provider_api: Api<Provider> = match cli.watch_namespace.as_deref() {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };
    let deployment_api: Api<Deployment> = Api::all(client.clone());

    info!("starting Provider lifecycle controller");

    // `owns(Deployment)` gives immediate feedback as replicas become ready, so
    // status.workload tracks reality without waiting for the periodic requeue.
    //
    // `watches(ProviderClass)` maps a class edit back to every Provider that
    // references it. The mapper kube calls is *synchronous*, so it cannot list
    // Providers itself — it reads the Controller's own reflector store instead,
    // which is already maintained for the primary watch and costs nothing extra.
    //
    // Without this, a class edit was only noticed on the next periodic requeue:
    // un-pausing a class took up to five minutes (bug-117), and an image bump
    // took up to 30s. Both are now immediate.
    let provider_controller = Controller::new(provider_api, Config::default());
    let provider_store = provider_controller.store();
    let class_watch_api: Api<ProviderClass> = Api::all(client.clone());

    let controller = provider_controller
        .owns(deployment_api, Config::default())
        .watches(
            class_watch_api,
            Config::default(),
            move |class: ProviderClass| {
                let class_name = class.name_any();
                provider_store
                    .state()
                    .into_iter()
                    .filter(move |p| p.spec.provider_class_ref.name == class_name)
                    .map(|p| ObjectRef::from_obj(p.as_ref()))
                    .collect::<Vec<_>>()
            },
        )
        .run(provider::reconcile, provider::error_policy, ctx.clone())
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => info!(kind = "Provider", ?obj, "reconciled"),
                Err(e) => error!(kind = "Provider", error = %e, "reconcile error"),
            }
        });

    // ProviderClass gets its own controller rather than being folded into the
    // Provider loop: its status is a property of the class (how many Providers
    // reference it, whether its shared ClusterRole exists), not of any one
    // Provider, and it must be published even when a class has no Providers at
    // all — which is exactly when "is this class usable?" is most worth knowing.
    let class_api: Api<ProviderClass> = Api::all(client.clone());
    let class_controller = Controller::new(class_api, Config::default())
        .run(
            providerclass::reconcile,
            providerclass::error_policy,
            ctx.clone(),
        )
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => info!(kind = "ProviderClass", ?obj, "reconciled"),
                Err(e) => error!(kind = "ProviderClass", error = %e, "reconcile error"),
            }
        });

    tokio::select! {
        () = controller => {
            info!("Provider controller stream ended");
        }
        () = class_controller => {
            info!("ProviderClass controller stream ended");
        }
        _ = shutdown_signal() => {
            info!("shutdown signal received; releasing controller");
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
