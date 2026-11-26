// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # `banlieue provider azure` entry point
//!
//! Library form of the Azure provider, invoked by the unified `banlieue`
//! binary (ADR-0004). [`run`] owns the full lifecycle: tracing, the rustls
//! crypto provider, kube client, health and metrics servers (ADR-0093,
//! ADR-0091), leader election, then the `Provider`, `VMImage` and
//! `AzureVirtualMachine` controllers through the SDK's instrumented
//! [`run_controller`]. All three are watch-driven: nothing here polls. The
//! `VMImage` controller also watches its publish Jobs (ADR-0162).
//!
//! There is no credentials Secret (ADR-0159). The provider exchanges its
//! projected ServiceAccount token, read from `--federated-token-file`
//! (`AZURE_FEDERATED_TOKEN_FILE`), for an ARM token; the tenant and client
//! come from each `Provider`'s `spec.connection.azure`.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{Provider, VMImage};
use banlieue_api::infrastructure::AzureVirtualMachine;
use banlieue_provider_sdk::bootstrap::{
    Observability, init_tracing, shutdown_signal, start_observability,
};
use banlieue_provider_sdk::client::build_client_with;
use banlieue_provider_sdk::defaults::{
    DEFAULT_BUILD_NAMESPACE, DEFAULT_HEALTH_PORT, DEFAULT_IMPORT_IMAGE,
    DEFAULT_LEADER_ELECTION_NAMESPACE, DEFAULT_METRICS_PORT,
};
use banlieue_provider_sdk::health::Election;
use banlieue_provider_sdk::leader::{LeaderConfig, elect};
use banlieue_provider_sdk::reconciler::{provider_watch_config, requeue_error_policy};
use banlieue_provider_sdk::runner::run_controller;
use banlieue_provider_sdk::scope::{ProviderScope, start_gate};
use clap::{Args, Subcommand};
use k8s_openapi::api::batch::v1::Job;
use kube::{
    Api, ResourceExt,
    runtime::{Controller, reflector::ObjectRef, watcher::Config},
};
use tracing::info;

use crate::client::{
    DEFAULT_FEDERATED_TOKEN_FILE, HttpClientFactory, install_default_crypto_provider,
};
use crate::context::{Context, PublishSettings};
use crate::metrics::SweepMetrics;
use crate::reconciler::publish_job::{
    DEFAULT_PUBLISHER_SERVICE_ACCOUNT, LABEL_VMIMAGE, job_watch_selector,
};
use crate::reconciler::{azurevirtualmachine, image, provider};

const DEFAULT_LEADER_ELECTION_ID: &str = "banlieue-provider-azure";

/// This role's name: the OTLP `service.name` (ADR-0092) and the `role`
/// label of `banlieue_leader` (ADR-0091).
pub const ROLE: &str = "banlieue-provider-azure";
/// Per-crate `tracing` directives layered on top of the base log level.
const LOG_DIRECTIVES: &[&str] = &["kube=warn"];

/// Command-line arguments for `banlieue provider azure`.
#[derive(Debug, Args)]
pub struct Cli {
    /// Kubeconfig path, or a `KUBECONFIG`-style list. When set it wins over
    /// every other source; otherwise in-cluster config, then `~/.kube/config`.
    #[arg(long, env = "KUBECONFIG")]
    pub kubeconfig: Option<String>,

    /// Home namespace: where the provider reads its own `Provider`. Defaults to the namespace it runs in; never
    /// every namespace (ADR-0131).
    #[arg(long, env = "BANLIEUE_NAMESPACE")]
    pub namespace: Option<String>,

    /// Also serve `AzureVirtualMachine`s in every namespace whose labels match this
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

    /// Image the image publish Jobs run (ADR-0162 Decision 1): the banlieue
    /// image itself. The operator passes the image it runs this provider on.
    #[arg(long, env = "BANLIEUE_IMPORT_IMAGE", default_value = DEFAULT_IMPORT_IMAGE)]
    pub import_image: String,

    /// Taints the publish Jobs may tolerate (`key[=value]:Effect`,
    /// repeatable). Placement follows the artifacts PVC they mount.
    #[arg(long = "build-toleration", value_name = "KEY[=VALUE]:EFFECT")]
    pub build_toleration: Vec<String>,

    /// Namespace holding the artifacts PVC and the publish Jobs. Must match
    /// banlieue-imagebuilder's `--build-namespace` (ADR-0016).
    #[arg(
        long,
        env = "BANLIEUE_BUILD_NAMESPACE",
        default_value = DEFAULT_BUILD_NAMESPACE,
    )]
    pub build_namespace: String,

    /// ServiceAccount the publish Jobs run as, in `--build-namespace`. Its
    /// projected token federates to the `Provider`'s
    /// `spec.connection.azure.publisher` identity (ADR-0159 Decision 3); it
    /// holds no Kubernetes RBAC.
    #[arg(
        long,
        env = "BANLIEUE_PUBLISHER_SERVICE_ACCOUNT",
        default_value = DEFAULT_PUBLISHER_SERVICE_ACCOUNT,
    )]
    pub publisher_service_account: String,

    /// The projected ServiceAccount token (audience
    /// `api://AzureADTokenExchange`) exchanged at Entra ID for an ARM token
    /// (ADR-0159). The operator mounts it and sets the variable.
    #[arg(
        long,
        env = "AZURE_FEDERATED_TOKEN_FILE",
        default_value = DEFAULT_FEDERATED_TOKEN_FILE,
    )]
    pub federated_token_file: std::path::PathBuf,

    /// Optional subcommand. When absent, `run` starts the controllers; when
    /// `image-publish`, it runs one publish or delete and exits.
    #[command(subcommand)]
    pub command: Option<AzureCommand>,
}

/// Subcommands of `banlieue provider azure`.
#[derive(Debug, Subcommand)]
pub enum AzureCommand {
    /// Publish a raw disk as an Azure Compute Gallery image version, or
    /// delete one banlieue published, then exit (ADR-0162).
    ///
    /// Runs inside the Job the `VMImage` reconciler creates, under the
    /// publisher's own identity; the flags are stable so a failed publish
    /// can be reproduced by hand.
    ImagePublish(crate::publish::PublishArgs),
}

/// Run the Azure provider to completion.
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
    // Before any TLS: the kube client's and every ARM client's.
    install_default_crypto_provider();

    // One-shot roles exit when done; only the controller path below needs a
    // health server, a leader lease, or a watch.
    if let Some(AzureCommand::ImagePublish(args)) = cli.command {
        let result = crate::publish::run(args).await;
        telemetry.shutdown();
        return result;
    }

    info!(
        version = env!("CARGO_PKG_VERSION"),
        namespace = ?cli.namespace,
        provider_name = ?cli.provider_name,
        leader_elect = !cli.no_leader_elect,
        token_file = %cli.federated_token_file.display(),
        "banlieue-provider-azure starting"
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

    // Parsed once at startup: a malformed toleration must fail the process
    // rather than surface later as an unschedulable Job.
    let tolerations = banlieue_provider_sdk::scheduling::parse_tolerations(&cli.build_toleration)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // The orphan sweep's counters (ADR-0158 Decision 5), served beside the
    // shared reconcile series.
    let sweep_metrics = SweepMetrics::default();
    sweep_metrics.register(&metrics);
    let ctx = Arc::new(
        Context::new(
            client.clone(),
            Some(namespace.clone()),
            Arc::new(HttpClientFactory::new(cli.federated_token_file.clone())),
            cli.provider_name.clone(),
        )
        .with_scope(scope.clone(), gate)
        .with_sweep_metrics(sweep_metrics)
        .with_publishing(PublishSettings {
            build_namespace: cli.build_namespace.clone(),
            image: cli.import_image.clone(),
            service_account: cli.publisher_service_account.clone(),
            tolerations,
        }),
    );

    info!("starting Provider controller (class=azure)");
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

    // VMImage is cluster-scoped: always watch every namespace. The publish
    // Jobs (ADR-0162) live in the build namespace; a Job's change re-runs
    // its VMImage at once instead of on the next requeue.
    let image_api: Api<VMImage> = Api::all(client.clone());
    let publish_jobs: Api<Job> = Api::namespaced(client.clone(), &cli.build_namespace);
    let image_ctrl = run_controller(
        Controller::new(image_api, Config::default()).watches(
            publish_jobs,
            Config::default().labels(&job_watch_selector()),
            vmimage_ref_from_job,
        ),
        "VMImage",
        metrics.clone(),
        image::reconcile,
        requeue_error_policy,
        ctx.clone(),
    );

    info!("starting AzureVirtualMachine controller");
    // AzureVirtualMachine follows the scope: the home namespace, or every namespace
    // behind the selector gate (ADR-0133). The Provider above never does.
    let machine_api: Api<AzureVirtualMachine> = scope.api(&client);
    let machine_ctrl = run_controller(
        Controller::new(machine_api, Config::default()).reconcile_all_on(scope_changes.trigger()),
        "AzureVirtualMachine",
        metrics.clone(),
        azurevirtualmachine::reconcile,
        requeue_error_policy,
        ctx,
    );

    readiness.controllers_started();

    tokio::select! {
        () = provider_ctrl => info!("Provider controller stream ended"),
        () = image_ctrl => info!("VMImage controller stream ended"),
        () = machine_ctrl => info!("AzureVirtualMachine controller stream ended"),
        _ = shutdown_signal() => info!("shutdown signal received; releasing controllers"),
    }
    telemetry.shutdown();
    Ok(())
}

/// Map a publish or delete `Job` to the `VMImage` it was created for, via
/// its [`LABEL_VMIMAGE`] label. `VMImage` is cluster-scoped, so the name is a
/// complete reference. A Job without the label maps to nothing.
#[must_use]
pub fn vmimage_ref_from_job(job: Job) -> Option<ObjectRef<VMImage>> {
    job.labels()
        .get(LABEL_VMIMAGE)
        .map(|name| ObjectRef::new(name))
}

/// Build a [`LeaderConfig`] from parsed CLI flags.
fn build_leader_config(cli: &Cli) -> LeaderConfig {
    LeaderConfig::new(&cli.leader_election_namespace, &cli.leader_election_id)
        .with_identity(cli.leader_election_identity.clone())
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod app_tests;
