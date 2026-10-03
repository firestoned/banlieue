// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-host
//!
//! `banlieue host cloud-hypervisor`: prepare a KVM host for banlieue's
//! Cloud Hypervisor provider from the binary itself (ADR-0067, ADR-0084),
//! the way `k0s install` prepares a node. It replaces the body of
//! `scripts/bootstrap-cloud-hypervisor-host.sh`, which is now a thin
//! `--remote` wrapper around this.
//!
//! ```text
//! banlieue host cloud-hypervisor preflight   # changes nothing
//! banlieue host cloud-hypervisor status      # changes nothing
//! banlieue host cloud-hypervisor selftest    # changes nothing, boots nothing
//! banlieue host cloud-hypervisor install     # every stage, in order
//! banlieue host ch install --only tpm        # `ch` is a visible alias
//! banlieue host ch install --dry-run
//! ```
//!
//! This crate is the installer, not a controller. It runs once, as root, at
//! an operator's request, and may therefore run `useradd`, `systemctl` and
//! `swtpm_setup`: ADR-0011 forbids subprocesses in a
//! reconcile path, and no reconcile path can reach this crate, because the
//! provider crate does not depend on it (`boundary_tests.rs`).

pub mod dryrun;
pub mod error;
#[cfg(test)]
pub mod fake;
pub mod fetch;
pub mod ops;
pub mod paths;
pub mod pins;
pub mod real;
pub mod release;
pub mod render;
pub mod settings;
pub mod stages;

pub use error::Error;

use clap::{Args, Subcommand};
use std::path::PathBuf;

/// `banlieue host <backend> <verb>`.
#[derive(Debug, Args)]
pub struct Cli {
    /// Which backend to prepare this machine for.
    #[command(subcommand)]
    pub backend: Backend,
}

/// The backends a host can be prepared for (ADR-0084 Decision 7). Only
/// Cloud Hypervisor: it is the one backend whose hypervisor banlieue
/// manages itself.
#[derive(Debug, Subcommand)]
pub enum Backend {
    /// Prepare this machine as a Cloud Hypervisor host (ADR-0067).
    #[command(visible_alias = "ch")]
    CloudHypervisor(CloudHypervisorCli),
}

/// `banlieue host cloud-hypervisor <verb>`.
#[derive(Debug, Args)]
pub struct CloudHypervisorCli {
    /// What to do.
    #[command(subcommand)]
    pub verb: Verb,
}

/// The verbs. Only `install` changes the host.
#[derive(Debug, Subcommand)]
pub enum Verb {
    /// Is this host fit for guests: bare metal, /dev/kvm, bridges, a free
    /// guest uid range, NSS. Changes nothing.
    Preflight(settings::HostArgs),
    /// Report what is installed. Changes nothing.
    Status(settings::HostArgs),
    /// Prove the pieces work together: the VMM and firmware, guest uids,
    /// /dev/kvm, the provider's own host checks, a vTPM with the right EK
    /// CN. Boots nothing, leaves nothing. Needs root.
    Selftest(SelftestArgs),
    /// Prepare this host, every stage in order, or one with --only. Needs
    /// root, except with --dry-run.
    Install(InstallArgs),
}

/// `banlieue host cloud-hypervisor selftest`.
#[derive(Debug, Args)]
pub struct SelftestArgs {
    /// The host's settings.
    #[command(flatten)]
    pub settings: settings::HostArgs,
    /// The release `install` placed (default: the pinned one).
    #[command(flatten)]
    pub release: release::ReleaseArgs,
}

/// `banlieue host cloud-hypervisor install`.
#[derive(Debug, Args)]
pub struct InstallArgs {
    /// The host's settings.
    #[command(flatten)]
    pub settings: settings::HostArgs,
    /// The VMM and firmware: which release, from where, with which digests.
    #[command(flatten)]
    pub release: release::ReleaseArgs,
    /// Run one stage; its prerequisites must already hold.
    #[arg(long, value_enum)]
    pub only: Option<stages::Stage>,
    /// Regenerate the host config and ROTATE THE EK CA: every EK
    /// certificate this host issued stops verifying.
    #[arg(long)]
    pub force: bool,
    /// Take the release assets (`cloud-hypervisor-static`,
    /// `ch-remote-static`, `CLOUDHV.fd`) from this directory instead of
    /// downloading them. They are verified the same way.
    #[arg(
        long,
        env = "BANLIEUE_HOST_ARTIFACTS_DIR",
        conflicts_with_all = ["vmm_url", "ch_remote_url", "firmware_url"]
    )]
    pub artifacts_dir: Option<PathBuf>,
    /// Install this file as the provider binary (default: leave it).
    #[arg(long, env = "BANLIEUE_HOST_PROVIDER_BINARY")]
    pub provider_binary: Option<PathBuf>,
    /// Print what would change, change nothing.
    #[arg(long)]
    pub dry_run: bool,
}

/// Run a `banlieue host <backend>` verb on this host.
///
/// # Errors
/// The verb's [`Error`].
pub async fn run(cli: Cli) -> Result<(), Error> {
    let host = real::RealHost::new();
    let facts = |c: &banlieue_provider_cloud_hypervisor::host_config::HostConfig| {
        banlieue_provider_cloud_hypervisor::provider::gather_facts(c)
    };
    let Backend::CloudHypervisor(ch) = cli.backend;
    match ch.verb {
        Verb::Preflight(args) => stages::preflight(&host, &settings::resolve(&args, &host)?),
        Verb::Status(args) => {
            settings::resolve(&args, &host)?;
            stages::status(&host);
            Ok(())
        }
        Verb::Selftest(args) => {
            let s = settings::resolve(&args.settings, &host)?;
            if !ops::Probe::is_root(&host) {
                return Err(Error::NotRoot("selftest"));
            }
            let release = release::resolve(&args.release, &fetch::GitHubDigests::new()).await?;
            stages::selftest(&host, &release, &s, &facts)
        }
        Verb::Install(args) => {
            let s = settings::resolve(&args.settings, &host)?;
            let release = release::resolve(&args.release, &fetch::GitHubDigests::new()).await?;
            let o = stages::Options {
                only: args.only,
                force: args.force,
                provider_binary: args.provider_binary,
                dry_run: args.dry_run,
            };
            let fetch: Box<dyn fetch::Fetch> = match args.artifacts_dir {
                Some(dir) => Box::new(fetch::FromDir { dir }),
                None => Box::new(fetch::Download::new()),
            };
            if o.dry_run {
                let dry = dryrun::DryRun::new(&host);
                return stages::install(&dry, fetch.as_ref(), &release, &s, &o, &facts).await;
            }
            if !ops::Probe::is_root(&host) {
                return Err(Error::NotRoot("install"));
            }
            stages::install(&host, fetch.as_ref(), &release, &s, &o, &facts).await
        }
    }
}

#[cfg(test)]
#[path = "boundary_tests.rs"]
mod boundary_tests;
