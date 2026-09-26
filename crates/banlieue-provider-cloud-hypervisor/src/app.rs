// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # `banlieue provider cloud-hypervisor` entry point
//!
//! Runs **on the KVM host**, not in the cluster (ADR-0060). Everything that
//! names the host — which `Provider` it is, where guests' disks live, which
//! bridges exist, which kubeconfig to use — comes from the host-local config
//! file the bootstrap script writes (ADR-0062 Decision 4), so the flags here
//! are only process concerns: the config path, logging, health, the lease.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{Provider, VMImage};
use banlieue_api::infrastructure::CloudHypervisorMachine;
use banlieue_provider_sdk::bootstrap::{init_tracing, serve_health, shutdown_signal};
use banlieue_provider_sdk::client::build_client_with;
use banlieue_provider_sdk::leader::{
    DEFAULT_LEASE_DURATION_SECS, DEFAULT_RENEW_PERIOD_SECS, DEFAULT_RETRY_PERIOD_SECS,
    LeaderConfig, acquire_or_wait, renew_forever,
};
use banlieue_provider_sdk::naming::workload_name;
use clap::{Args, Subcommand, ValueEnum};
use futures::StreamExt;
use kube::runtime::{Controller, watcher::Config as WatchConfig};
use kube::{Api, Client};
use tracing::{error, info, warn};

use crate::host::RealHost;
use crate::host_config::HostConfig;
use crate::reconciler::{self, Context};
use crate::{provider, sys, systemd, token, vmimage};

/// Where the bootstrap script writes the host config.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/banlieue/cloud-hypervisor.toml";
const DEFAULT_HEALTH_PORT: u16 = 8081;
/// Per-crate `tracing` directives layered on top of the base log level.
const LOG_DIRECTIVES: &[&str] = &["kube=warn", "zbus=warn"];

/// Which systemd manager guests are started under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum BusArg {
    /// The system manager. Production (ADR-0063).
    System,
    /// The calling user's manager. Development without root only: guests
    /// then run as that user, not as per-guest uids.
    Session,
}

impl From<BusArg> for systemd::Bus {
    fn from(b: BusArg) -> Self {
        match b {
            BusArg::System => Self::System,
            BusArg::Session => Self::Session,
        }
    }
}

/// Command-line arguments for `banlieue provider cloud-hypervisor`.
#[derive(Debug, Args)]
pub struct Cli {
    /// One-shot subcommand. Without one, this runs the provider.
    #[command(subcommand)]
    pub command: Option<ChCommand>,

    /// The host-local config (ADR-0062 Decision 4).
    #[arg(long, env = "BANLIEUE_CH_CONFIG", default_value = DEFAULT_CONFIG_PATH)]
    pub config: PathBuf,

    /// systemd manager to start guest units under.
    #[arg(long, env = "BANLIEUE_CH_BUS", value_enum, default_value_t = BusArg::System)]
    pub bus: BusArg,

    /// Health server bind port.
    #[arg(long, env = "BANLIEUE_HEALTH_PORT", default_value_t = DEFAULT_HEALTH_PORT)]
    pub health_port: u16,

    /// Log format: `json` for SIEM-friendly output, `text` for local dev.
    #[arg(long, env = "RUST_LOG_FORMAT", default_value = "text")]
    pub log_format: String,

    /// Log level. Overrides `RUST_LOG`.
    #[arg(long, env = "BANLIEUE_LOG_LEVEL")]
    pub log_level: Option<String>,

    /// Disable leader election.
    #[arg(long, env = "BANLIEUE_NO_LEADER_ELECT", default_value_t = false)]
    pub no_leader_elect: bool,

    /// Holder identity. Falls back to `POD_NAME` / `HOSTNAME`.
    #[arg(long, env = "BANLIEUE_LEADER_ELECTION_IDENTITY")]
    pub leader_election_identity: Option<String>,
}

/// Subcommands of `banlieue provider cloud-hypervisor`.
#[derive(Debug, Subcommand)]
pub enum ChCommand {
    /// Pull one registry image into every storage class's cache, and exit.
    ///
    /// Runs in the import unit the `VMImage` reconciler starts (ADR-0064);
    /// the flags are stable so a failed import can be reproduced by hand.
    Import(crate::import::ImportArgs),
}

/// The Lease this host's provider holds: one host, one lease, so two copies
/// started on the same host by mistake do not both drive guests. Named as
/// the operator names this Provider's workload, because that is the Lease
/// its Role grants by name (ADR-0060 Decision 5); `class` is the Provider's
/// `providerClassRef`.
#[must_use]
pub fn leader_config(config: &HostConfig, class: &str, identity: Option<String>) -> LeaderConfig {
    LeaderConfig {
        namespace: config.provider.namespace.clone(),
        lease_name: workload_name(class, &config.provider.name),
        identity: identity.unwrap_or_else(LeaderConfig::default_identity),
        lease_duration: Duration::from_secs(DEFAULT_LEASE_DURATION_SECS),
        renew_period: Duration::from_secs(DEFAULT_RENEW_PERIOD_SECS),
        retry_period: Duration::from_secs(DEFAULT_RETRY_PERIOD_SECS),
    }
}

/// Watch only this host's `Provider`, narrowed server-side.
#[must_use]
pub fn provider_watch_config(config: &HostConfig) -> WatchConfig {
    WatchConfig::default().fields(&format!("metadata.name={}", config.provider.name))
}

/// A client from the kubeconfig the host config names — never an ambient
/// `KUBECONFIG` or `~/.kube/config`, so the identity this process acts as
/// is exactly the one the bootstrap provisioned (ADR-0060 Decision 5).
async fn build_client(config: &HostConfig) -> Result<Client> {
    let path = &config.provider.kubeconfig;
    build_client_with(Some(path.as_os_str()))
        .await
        .with_context(|| format!("loading kubeconfig {}", path.display()))
}

/// Run the Cloud Hypervisor provider to completion.
///
/// # Errors
/// Returns an error if the host config is unusable, or if logging init,
/// the kube client, the systemd connection or the leader lease fails.
pub async fn run(cli: Cli) -> Result<()> {
    init_tracing(&cli.log_format, cli.log_level.as_deref(), LOG_DIRECTIVES)
        .context("initialising tracing")?;

    // One-shot roles exit when their work is done.
    if let Some(ChCommand::Import(args)) = cli.command {
        return crate::import::run(&cli.config, args).await;
    }

    // Fail at startup on a bad config, not on the first machine.
    let config = Arc::new(
        HostConfig::load(&cli.config)
            .with_context(|| format!("loading {}", cli.config.display()))?,
    );
    info!(
        version = env!("CARGO_PKG_VERSION"),
        provider = %config.provider.name,
        namespace = %config.provider.namespace,
        bus = ?cli.bus,
        "banlieue-provider-cloud-hypervisor starting"
    );

    let client = build_client(&config).await?;
    // Its own Provider names its class, from which the operator derived the
    // names it granted: the Lease below and the ServiceAccount the token
    // belongs to. A missing Provider is a configuration error; systemd
    // restarts the service to try again.
    let provider = Api::<Provider>::namespaced(client.clone(), &config.provider.namespace)
        .get(&config.provider.name)
        .await
        .with_context(|| {
            format!(
                "reading Provider {}/{}",
                config.provider.namespace, config.provider.name
            )
        })?;
    let class = provider.spec.provider_class_ref.name.clone();

    // Keep the credential alive (ADR-0060 Decision 5). Only a token FILE can
    // be renewed in place; a kubeconfig with the token inline keeps working
    // until it expires.
    match token::token_file_of(&config.provider.kubeconfig) {
        Ok(Some(path)) => {
            info!(path = %path.display(), "renewing the provider's token at half-life");
            tokio::spawn(token::renew_forever(
                client.clone(),
                path,
                token::DEFAULT_TOKEN_LIFETIME,
            ));
        }
        Ok(None) => warn!(
            kubeconfig = %config.provider.kubeconfig.display(),
            "kubeconfig embeds its token, so it cannot be renewed and will expire; \
             re-issue it with `banlieue bootstrap cloud-hypervisor-host`"
        ),
        Err(e) => warn!(error = %e, "cannot read the kubeconfig to find its token file"),
    }
    // Guests share the provider's gid so it can manage their files
    // (ADR-0063 Decision 5).
    let group = sys::effective_gid();
    let host = RealHost::connect(cli.bus.into(), group)
        .await
        .context("connecting to systemd")?;
    tokio::spawn(serve_health(cli.health_port));

    if cli.no_leader_elect {
        info!("leader election disabled by --no-leader-elect");
    } else {
        let lc = leader_config(&config, &class, cli.leader_election_identity.clone());
        info!(lease = %lc.lease_name, "waiting for leader election");
        acquire_or_wait(client.clone(), &lc)
            .await
            .context("acquiring leader lease")?;
        let renewer = client.clone();
        tokio::spawn(async move {
            if let Err(e) = renew_forever(renewer, lc).await {
                error!(error = %e, "leader lease renewer terminated — exiting");
                std::process::exit(1);
            }
        });
    }

    let ctx = Arc::new(Context {
        client: client.clone(),
        config: config.clone(),
        host: Arc::new(host),
        group,
    });

    let provider_api: Api<Provider> = Api::namespaced(client.clone(), &config.provider.namespace);
    let provider_ctrl = Controller::new(provider_api, provider_watch_config(&config))
        .run(provider::reconcile, provider::error_policy, ctx.clone())
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => info!(kind = "Provider", ?obj, "reconciled"),
                Err(e) => error!(kind = "Provider", error = %e, "reconcile error"),
            }
        });

    // Machines live beside their Provider (ADR-0060); the reconciler skips
    // any whose providerRef names another host.
    let machine_api: Api<CloudHypervisorMachine> =
        Api::namespaced(client.clone(), &config.provider.namespace);
    let machine_ctrl = Controller::new(machine_api, WatchConfig::default())
        .run(reconciler::reconcile, reconciler::error_policy, ctx.clone())
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => info!(kind = "CloudHypervisorMachine", ?obj, "reconciled"),
                Err(e) => error!(kind = "CloudHypervisorMachine", error = %e, "reconcile error"),
            }
        });

    // VMImage is cluster-scoped: every image may name this provider class.
    let image_ctrl = Controller::new(Api::<VMImage>::all(client.clone()), WatchConfig::default())
        .run(vmimage::reconcile, vmimage::error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => info!(kind = "VMImage", ?obj, "reconciled"),
                Err(e) => error!(kind = "VMImage", error = %e, "reconcile error"),
            }
        });

    // Guests are systemd units, not children of this process: stopping the
    // provider leaves them running (ADR-0063).
    tokio::select! {
        () = provider_ctrl => info!("Provider controller stream ended"),
        () = machine_ctrl => info!("CloudHypervisorMachine controller stream ended"),
        () = image_ctrl => info!("VMImage controller stream ended"),
        _ = shutdown_signal() => info!("shutdown signal received; guests keep running"),
    }
    Ok(())
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod app_tests;
