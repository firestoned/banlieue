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
//! 3. Starts the health server on `:health_port` (`/livez`, `/readyz`,
//!    ADR-0093) and the metrics server on `:metrics_port` (ADR-0091).
//! 4. (Unless `--no-leader-elect`) acquires the leader Lease before any
//!    reconciler runs; spawns a background renewer.
//! 5. Runs the [`kube::runtime::Controller`]s for `Provider` and
//!    `ProviderClass` through the SDK's instrumented [`run_controller`].

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{Provider, ProviderClass};
use banlieue_provider_sdk::bootstrap::{
    Observability, init_tracing, shutdown_signal, start_observability,
};
use banlieue_provider_sdk::client::build_client;
use banlieue_provider_sdk::health::Election;
use banlieue_provider_sdk::leader::{
    DEFAULT_LEASE_DURATION_SECS, DEFAULT_RENEW_PERIOD_SECS, DEFAULT_RETRY_PERIOD_SECS,
    LeaderConfig, acquire_or_wait, renew_forever,
};
use banlieue_provider_sdk::runner::run_controller;
use banlieue_provider_sdk::scope::{
    NamespaceGate, Scope, ScopeError, check_selector, resolve_scope, scope_is_implicit,
    scope_trigger, spawn_namespace_gate,
};
use clap::Args;
use k8s_openapi::api::apps::v1::Deployment;
use kube::{
    Api, ResourceExt,
    runtime::{Controller, reflector::ObjectRef, watcher::Config},
};
use tracing::{error, info, warn};

use crate::{
    context::Context,
    reconciler::{provider, providerclass},
};

const DEFAULT_HEALTH_PORT: u16 = 8081;
const DEFAULT_METRICS_PORT: u16 = 8080;
const DEFAULT_NAMESPACE: &str = "banlieue-system";
const DEFAULT_LEADER_ELECTION_ID: &str = "banlieue-operator";

/// This role's name: the OTLP `service.name` (ADR-0092) and the `role`
/// label of `banlieue_leader` (ADR-0091).
pub const ROLE: &str = "banlieue-operator";

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

    /// Seconds without a Lease renewal after which a provider's heartbeat is
    /// `Stale` and it takes no new VirtualMachine (ADR-0102).
    #[arg(
        long = "provider-stale-after-secs",
        env = "BANLIEUE_PROVIDER_STALE_AFTER_SECS",
        default_value_t = crate::heartbeat::DEFAULT_STALE_AFTER_SECS
    )]
    pub provider_stale_after_secs: u64,

    /// Serve the Providers in this one namespace. Used when
    /// `--watch-namespace-selector` is not set; when neither is, the operator
    /// serves the namespace it runs in (`--namespace`). It never defaults to
    /// every namespace (ADR-0131).
    #[arg(long, env = "BANLIEUE_WATCH_NAMESPACE")]
    pub watch_namespace: Option<String>,

    /// Serve the Providers in every namespace whose labels match this label
    /// selector (for example `banlieue.io/tenant=true`). Wins over
    /// `--watch-namespace`. Needs a read-only list/watch on `Namespace`.
    /// Labelling a namespace onboards it without a restart (ADR-0131).
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
    info!(
        version = env!("CARGO_PKG_VERSION"),
        namespace = %cli.namespace,
        watch_namespace = ?cli.watch_namespace,
        watch_namespace_selector = ?cli.watch_namespace_selector,
        leader_elect = !cli.no_leader_elect,
        "banlieue-operator starting"
    );

    let client = build_client().await.context("constructing kube client")?;

    let scope = watch_scope(&cli).context("resolving which namespaces to serve")?;
    info!(scope = %scope.describe(), "serving Providers in");
    if let Scope::Selector(selector) = &scope {
        check_selector(&client, selector)
            .await
            .context("checking --watch-namespace-selector")?;
    }
    if watch_scope_is_implicit(&cli) {
        // ADR-0131 narrowed the default from every namespace to this one.
        warn!(
            scope = %scope.describe(),
            "no --watch-namespace-selector or --watch-namespace set: serving only the \
             operator's own namespace; set BANLIEUE_WATCH_NAMESPACE_SELECTOR to serve tenant \
             namespaces"
        );
    }

    let leader_cfg = (!cli.no_leader_elect).then(|| build_leader_config(&cli));
    let election = leader_cfg
        .as_ref()
        .map_or(Election::Disabled, LeaderConfig::election);
    let Observability { metrics, readiness } =
        start_observability(ROLE, cli.health_port, cli.metrics_port, election)
            .await
            .context("starting health and metrics servers")?;

    if let Some(leader_cfg) = leader_cfg {
        info!(
            namespace = %leader_cfg.namespace,
            lease = %leader_cfg.lease_name,
            identity = %leader_cfg.identity,
            "waiting for leader election"
        );
        acquire_or_wait(client.clone(), &leader_cfg, &readiness)
            .await
            .context("acquiring leader lease")?;

        let renewer_client = client.clone();
        let renewer_readiness = readiness.clone();
        tokio::spawn(async move {
            if let Err(e) = renew_forever(renewer_client, leader_cfg, renewer_readiness).await {
                error!(error = %e, "leader lease renewer terminated — exiting");
                std::process::exit(1);
            }
        });
    } else {
        info!("leader election disabled by --no-leader-elect");
    }

    // Selector mode: watch every namespace and gate each reconcile on a
    // selector-filtered Namespace reflector, whose changes reconcile every
    // Provider again. Single-namespace modes narrow the watch and need neither.
    let (scope_tx, scope_rx) = tokio::sync::watch::channel(0_u64);
    let (gate, scope_tx) = match &scope {
        Scope::Selector(selector) => {
            let gate = spawn_namespace_gate(&client, selector, scope_tx)
                .await
                .context("starting the namespace selector watch")?;
            (gate, None)
        }
        Scope::Namespace(_) => (NamespaceGate::none(), Some(scope_tx)),
    };

    let ctx = Arc::new(
        Context::with_build_scheduling(
            client.clone(),
            cli.namespace.clone(),
            crate::bootstrap::DEFAULT_IMAGEBUILD_NAMESPACE.to_string(),
            cli.build_node_selector.clone(),
            cli.build_toleration.clone(),
        )
        .with_provider_stale_after(Duration::from_secs(cli.provider_stale_after_secs))
        .with_namespace_gate(gate),
    );

    let provider_api: Api<Provider> = scope.api(&client);
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
    let provider_controller =
        Controller::new(provider_api, Config::default()).reconcile_all_on(scope_trigger(scope_rx));
    let provider_store = provider_controller.store();
    let class_watch_api: Api<ProviderClass> = Api::all(client.clone());

    let provider_controller = provider_controller
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
        );
    let controller = run_controller(
        provider_controller,
        "Provider",
        metrics.clone(),
        provider::reconcile,
        provider::error_policy,
        ctx.clone(),
    );

    // ProviderClass gets its own controller rather than being folded into the
    // Provider loop: its status is a property of the class (how many Providers
    // reference it, whether its shared ClusterRole exists), not of any one
    // Provider, and it must be published even when a class has no Providers at
    // all — which is exactly when "is this class usable?" is most worth knowing.
    let class_api: Api<ProviderClass> = Api::all(client.clone());
    let class_controller = run_controller(
        Controller::new(class_api, Config::default()),
        "ProviderClass",
        metrics.clone(),
        providerclass::reconcile,
        providerclass::error_policy,
        ctx.clone(),
    );

    readiness.controllers_started();

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

    // Held to here so a single-namespace run's trigger stream stays pending
    // instead of ending when the sender drops.
    drop(scope_tx);
    telemetry.shutdown();
    Ok(())
}

/// Resolve which namespaces' Providers this operator serves (ADR-0131):
/// `--watch-namespace-selector`, else `--watch-namespace`, else the
/// operator's own namespace (`--namespace`). Never every namespace.
///
/// # Errors
/// [`ScopeError::NoScope`] if all three are blank.
pub fn watch_scope(cli: &Cli) -> Result<Scope, ScopeError> {
    resolve_scope(
        cli.watch_namespace_selector.as_deref(),
        cli.watch_namespace.as_deref(),
        Some(&cli.namespace),
    )
}

/// Whether no watch flag chose the scope, so it is the implicit fallback to
/// the operator's own namespace.
#[must_use]
pub fn watch_scope_is_implicit(cli: &Cli) -> bool {
    scope_is_implicit(
        cli.watch_namespace_selector.as_deref(),
        cli.watch_namespace.as_deref(),
    )
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
