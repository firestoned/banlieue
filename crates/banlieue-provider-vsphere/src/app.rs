// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # `banlieue provider vsphere` entry point
//!
//! This is the library form of the vSphere provider, invoked by the unified
//! `banlieue` binary as `banlieue provider vsphere` (see ADR-0004). [`run`]
//! owns the full lifecycle:
//!
//! 1. Initialises structured logging via [`banlieue_provider_sdk::bootstrap`].
//! 2. Builds a [`kube::Client`] via [`banlieue_provider_sdk::client`].
//! 3. Starts a tiny health server on `:health_port` (livez + readyz).
//! 4. (Unless `--no-leader-elect`) acquires the leader Lease before any
//!    reconciler runs; spawns a background renewer.
//! 5. Runs the [`kube::runtime::Controller`]s for `Provider` (vSphere class)
//!    and `VMImage`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{Provider, VMImage};
use banlieue_api::infrastructure::VSphereMachine;
use banlieue_provider_sdk::bootstrap::{init_tracing, serve_health, shutdown_signal};
use banlieue_provider_sdk::client::build_client;
use banlieue_provider_sdk::leader::{
    DEFAULT_LEASE_DURATION_SECS, DEFAULT_RENEW_PERIOD_SECS, DEFAULT_RETRY_PERIOD_SECS,
    LeaderConfig, acquire_or_wait, renew_forever,
};
use clap::{Args, Subcommand};
use futures::StreamExt;
use k8s_openapi::api::batch::v1::Job;
use kube::{
    Api, ResourceExt,
    runtime::{Controller, reflector::ObjectRef, watcher::Config},
};
use tracing::{error, info};

use crate::{
    client::{VimClientFactory, install_default_crypto_provider},
    context::Context,
    reconciler::provider,
    reconciler::vmimage,
    reconciler::vspheremachine,
};

const DEFAULT_HEALTH_PORT: u16 = 8081;
const DEFAULT_METRICS_PORT: u16 = 8080;
const DEFAULT_LEADER_ELECTION_NAMESPACE: &str = "banlieue-system";
const DEFAULT_LEADER_ELECTION_ID: &str = "banlieue-provider-vsphere";
const DEFAULT_VSPHERE_TASK_TIMEOUT_SECS: u64 = 600;
/// Default image for VMImage import Jobs. Matches libvirt's default; normally
/// overridden by `banlieue-operator`, which passes `--import-image` with the
/// running image so the whole fleet stays on one build.
const DEFAULT_IMPORT_IMAGE: &str = "ghcr.io/firestoned/banlieue:v0.1.0";
/// Namespace holding the artifacts PVC and per-zone import Jobs. Must match
/// `banlieue-imagebuilder`'s `--build-namespace` (ADR-0016 / ADR-0020).
const DEFAULT_BUILD_NAMESPACE: &str = "banlieue-imagebuild";
/// ServiceAccount the import Job runs as — a dedicated read-only identity,
/// never this controller's own (ADR-0016 §4).
const DEFAULT_IMPORT_SERVICE_ACCOUNT: &str = "banlieue-import";

/// Per-crate `tracing` directives layered on top of the base log level.
const LOG_DIRECTIVES: &[&str] = &["kube=warn", "vim_rs=warn"];

/// Command-line arguments for `banlieue provider vsphere`.
#[derive(Debug, Args)]
pub struct Cli {
    /// Path to a kubeconfig file. Falls back to in-cluster config or
    /// `$KUBECONFIG` / `~/.kube/config` when unset.
    #[arg(long, env = "KUBECONFIG")]
    pub kubeconfig: Option<String>,

    /// Restrict the provider to a single namespace. Cluster-wide when unset.
    #[arg(long, env = "BANLIEUE_NAMESPACE")]
    pub namespace: Option<String>,

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

    /// Holder identity. Falls back to `POD_NAME` / `HOSTNAME` / "unknown".
    #[arg(long, env = "BANLIEUE_LEADER_ELECTION_IDENTITY")]
    pub leader_election_identity: Option<String>,

    /// Timeout for individual vCenter tasks (clone, power, reconfigure).
    /// Used in Phase 1B iter 2+; accepted here so the flag matrix is stable.
    #[arg(
        long,
        env = "BANLIEUE_VSPHERE_TASK_TIMEOUT_SECS",
        default_value_t = DEFAULT_VSPHERE_TASK_TIMEOUT_SECS,
    )]
    pub vsphere_task_timeout_secs: u64,

    /// Serve exactly one `Provider`, by name — the per-instance topology
    /// `banlieue-operator` uses for every workload it spawns.
    ///
    /// The watch is narrowed **server-side** with a field selector, so this
    /// process's informer cache holds only its own Provider. Filtering in the
    /// reconciler instead would still pay the full cluster-wide cache cost and
    /// leave one hung backend able to stall every other.
    ///
    /// Unset means "watch every Provider of this class", which is what a
    /// statically installed provider (`banlieue bootstrap provider <backend>`)
    /// wants.
    #[arg(long, env = "BANLIEUE_PROVIDER_NAME")]
    pub provider_name: Option<String>,

    /// Namespace holding the artifacts PVC and per-zone import Jobs. Must match
    /// banlieue-imagebuilder's `--build-namespace` (ADR-0016 / ADR-0020).
    #[arg(
        long,
        env = "BANLIEUE_BUILD_NAMESPACE",
        default_value = DEFAULT_BUILD_NAMESPACE,
    )]
    pub build_namespace: String,

    /// Taints the per-zone import Jobs may tolerate (`key[=value]:Effect`,
    /// repeatable). Not a node selector: placement follows the artifacts PVC
    /// the Job mounts, which the scheduler resolves from the bound PV.
    #[arg(long = "build-toleration", value_name = "KEY[=VALUE]:EFFECT")]
    pub build_toleration: Vec<String>,

    /// Image the per-zone import Job runs — the banlieue image itself, so the
    /// data path stays inside banlieue's own supply chain. `banlieue-operator`
    /// passes this on every spawned provider so the whole fleet runs one image.
    #[arg(long, env = "BANLIEUE_IMPORT_IMAGE", default_value = DEFAULT_IMPORT_IMAGE)]
    pub import_image: String,

    /// ServiceAccount the import Job runs as, in `--build-namespace`.
    ///
    /// Deliberately **not** this controller's own identity: that one can create
    /// Jobs, so a workload in the privileged build namespace holding it could
    /// create further privileged pods. The import identity is read-only and the
    /// operator scopes it to exactly this Provider and its Secret (ADR-0016 §4).
    #[arg(
        long,
        env = "BANLIEUE_IMPORT_SERVICE_ACCOUNT",
        default_value = DEFAULT_IMPORT_SERVICE_ACCOUNT,
    )]
    pub import_service_account: String,

    /// Optional subcommand. When absent, `run` starts the controllers; when
    /// `image-import`, it runs one per-zone ISO import and exits.
    #[command(subcommand)]
    pub command: Option<VsphereCommand>,
}

/// Subcommands of `banlieue provider vsphere`.
#[derive(Debug, Subcommand)]
pub enum VsphereCommand {
    /// Upload a built ISO to one failure domain's datastore, create an empty
    /// VM, attach the ISO, and `MarkAsTemplate`, then exit (ADR-0020).
    ///
    /// Runs inside the Job the `VMImage` reconciler creates; not normally
    /// invoked by hand, though the flags are stable so a failed import can be
    /// reproduced.
    ImageImport(crate::import::ImportArgs),
}

/// Watch configuration for the `Provider` controller.
///
/// Narrows to a single object by name when `--provider-name` is set. Kept as a
/// standalone function so the scoping rule is unit-testable without a cluster.
#[must_use]
pub fn provider_watch_config(provider_name: Option<&str>) -> Config {
    match provider_name {
        Some(name) => Config::default().fields(&format!("metadata.name={name}")),
        None => Config::default(),
    }
}

/// Map a per-zone import `Job` to the `VMImage` it belongs to, via the
/// [`crate::reconciler::vmimage::LABEL_VMIMAGE`] label every import Job
/// carries. Feeds `Controller::watches` so a Job's status change (created,
/// completed, failed, deleted-and-recreated by a forced reimport)
/// re-triggers that `VMImage`'s reconciliation immediately — event-driven,
/// not the previous behavior of waiting on the next poll interval (found
/// live: after deleting a Job to force a rebuild, the owning `VMImage` sat
/// unreconciled for up to `REQUEUE_LONG_SECS` with nothing watching the Job
/// at all). `VMImage` is cluster-scoped, so a bare name is a complete ref —
/// no namespace to carry over from the (namespaced) Job.
///
/// A Job missing the label (from a stray unrelated Job in the build
/// namespace, or a version predating this label) maps to nothing, never
/// panics.
#[must_use]
pub fn vmimage_ref_from_job(job: Job) -> Option<ObjectRef<VMImage>> {
    let name = job
        .labels()
        .get(crate::reconciler::vmimage::LABEL_VMIMAGE)?;
    Some(ObjectRef::new(name))
}

/// Run the vSphere provider to completion (until a shutdown signal or a
/// controller stream ends).
///
/// # Arguments
/// * `cli` - parsed `banlieue provider vsphere` arguments.
///
/// # Errors
/// Returns an error if logging init, kube client construction, or leader-lease
/// acquisition fails.
pub async fn run(cli: Cli) -> Result<()> {
    // Install the rustls ring provider as the process default before ANY TLS use
    // — the kube client below and the BYOC vCenter client both need it, and
    // reqwest 0.13 (rustls-no-provider) panics without it (ADR-0009).
    install_default_crypto_provider();

    init_tracing(&cli.log_format, cli.log_level.as_deref(), LOG_DIRECTIVES)
        .context("initialising tracing")?;

    // One-shot roles exit when done; only the controller path below needs a
    // health server, a leader lease, or a watch.
    if let Some(VsphereCommand::ImageImport(args)) = cli.command {
        return crate::import::run(args).await;
    }

    info!(
        version = env!("CARGO_PKG_VERSION"),
        namespace = ?cli.namespace,
        leader_elect = !cli.no_leader_elect,
        "banlieue-provider-vsphere starting"
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

    // Parsed once at startup: a malformed toleration must fail the process
    // rather than surface later as an unschedulable Job.
    let import_tolerations =
        banlieue_provider_sdk::scheduling::parse_tolerations(&cli.build_toleration)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

    let vsphere_factory = Arc::new(VimClientFactory::new());
    let ctx = Arc::new(Context::new(
        client.clone(),
        cli.namespace.clone(),
        vsphere_factory,
        cli.build_namespace.clone(),
        cli.import_image.clone(),
        cli.import_service_account.clone(),
        import_tolerations,
    ));

    let provider_api: Api<Provider> = match cli.namespace.as_deref() {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };
    // VMImage is cluster-scoped; the per-Provider readiness check needs to
    // see every image and every Provider regardless of --namespace.
    let image_api: Api<VMImage> = Api::all(client.clone());
    // VSphereMachine is namespaced, same scoping rule as Provider.
    let machine_api: Api<VSphereMachine> = match cli.namespace.as_deref() {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };

    info!(
        provider_name = ?cli.provider_name,
        "starting Provider + VMImage + VSphereMachine controllers (class=vsphere)"
    );
    let provider_ctrl = Controller::new(
        provider_api,
        provider_watch_config(cli.provider_name.as_deref()),
    )
    .run(provider::reconcile, provider::error_policy, ctx.clone())
    .for_each(|res| async move {
        match res {
            Ok((obj, _)) => info!(kind = "Provider", ?obj, "reconciled"),
            Err(e) => error!(kind = "Provider", error = %e, "reconcile error"),
        }
    });

    // Import Jobs live in the (namespaced) build namespace, not wherever
    // --namespace scopes Provider/VSphereMachine — always Api::all's
    // cluster-wide equivalent restricted to one namespace, never affected
    // by cli.namespace.
    let import_job_api: Api<Job> = Api::namespaced(client.clone(), &cli.build_namespace);
    let image_ctrl = Controller::new(image_api, Config::default())
        .watches(import_job_api, Config::default(), vmimage_ref_from_job)
        .run(vmimage::reconcile, vmimage::error_policy, ctx.clone())
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => info!(kind = "VMImage", ?obj, "reconciled"),
                Err(e) => error!(kind = "VMImage", error = %e, "reconcile error"),
            }
        });

    // ADR-0024: clone-from-template create path only — see
    // reconciler::vspheremachine's module doc comment for scope.
    let machine_ctrl = Controller::new(machine_api, Config::default())
        .run(vspheremachine::reconcile, vspheremachine::error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => info!(kind = "VSphereMachine", ?obj, "reconciled"),
                Err(e) => error!(kind = "VSphereMachine", error = %e, "reconcile error"),
            }
        });

    tokio::select! {
        () = provider_ctrl => {
            info!("Provider controller stream ended");
        }
        () = image_ctrl => {
            info!("VMImage controller stream ended");
        }
        () = machine_ctrl => {
            info!("VSphereMachine controller stream ended");
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
