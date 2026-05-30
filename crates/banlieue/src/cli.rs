// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Top-level command-line interface for the unified `banlieue` binary.
//!
//! This module owns *only* dispatch (see ADR-0004): it parses the subcommand
//! tree and forwards each role's flags to the matching library crate's `run`
//! entry point. No reconcile logic lives here.
//!
//! Shape:
//!
//! ```text
//! banlieue controller [flags]            -> banlieue_controller::run
//! banlieue provider vsphere [flags]      -> banlieue_provider_vsphere::run
//! banlieue completion <shell>            -> print a shell completion script
//! ```
//!
//! Each provider backend is a nested subcommand gated behind a per-provider
//! Cargo feature (default = all available), so a slim build can drop a
//! backend's dependency graph entirely.

use std::io::Write;

use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::Shell;

/// `banlieue` — one binary that packages every controller role.
#[derive(Debug, Parser)]
#[command(
    name = "banlieue",
    version,
    about = "Kubernetes-native abstract virtualization API — controller + providers in one binary",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level roles.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the main banlieue controller (watches VirtualMachine CRs).
    Controller(banlieue_controller::Cli),

    /// Run a backend provider controller.
    Provider(ProviderArgs),

    /// Print a shell completion script to stdout.
    ///
    /// Example (zsh): `banlieue completion zsh > "${fpath[1]}/_banlieue"`.
    Completion(CompletionArgs),
}

/// `banlieue completion <shell>` — emit a completion script for the whole
/// command tree (controller, provider backends, this command).
#[derive(Debug, Args)]
pub struct CompletionArgs {
    /// Target shell. One of: bash, zsh, fish, elvish, powershell.
    #[arg(value_name = "SHELL")]
    pub shell: Shell,
}

/// `banlieue provider <backend>` — selects which backend provider to run.
#[derive(Debug, Args)]
pub struct ProviderArgs {
    #[command(subcommand)]
    pub backend: ProviderBackend,
}

/// Available backend providers. Each variant is gated behind its own Cargo
/// feature so disabled backends are not compiled or linked.
#[derive(Debug, Subcommand)]
pub enum ProviderBackend {
    /// VMware vSphere / vCenter provider.
    #[cfg(feature = "vsphere")]
    Vsphere(banlieue_provider_vsphere::Cli),
}

/// Dispatch a parsed [`Cli`] to the selected role's `run` entry point.
///
/// # Errors
/// Propagates whatever error the selected role's `run` returns.
pub async fn dispatch(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Controller(args) => banlieue_controller::run(args).await,
        Command::Provider(provider) => dispatch_provider(provider.backend).await,
        Command::Completion(args) => {
            write_completion(args.shell, &mut std::io::stdout().lock());
            Ok(())
        }
    }
}

/// Write a completion script for the full `banlieue` command tree to `out`.
///
/// Pure with respect to its `out` argument — the reconcile dispatch writes to
/// stdout, the unit tests write to an in-memory buffer.
pub fn write_completion(shell: Shell, out: &mut impl Write) {
    let mut cmd = Cli::command();
    let bin = cmd.get_name().to_string();
    clap_complete::generate(shell, &mut cmd, bin, out);
}

/// Dispatch the `provider <backend>` subcommand to the chosen backend.
async fn dispatch_provider(backend: ProviderBackend) -> anyhow::Result<()> {
    match backend {
        #[cfg(feature = "vsphere")]
        ProviderBackend::Vsphere(args) => banlieue_provider_vsphere::run(args).await,
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod cli_tests;
