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
//! banlieue operator [flags]              -> banlieue_operator::run
//! banlieue provider vsphere [flags]      -> banlieue_provider_vsphere::run
//! banlieue provider libvirt [flags]      -> banlieue_provider_libvirt::run
//! banlieue imagebuilder [flags]          -> banlieue_imagebuilder::run
//! banlieue bootstrap <target> [flags]    -> banlieue_operator::bootstrap::run
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
///
/// The variants differ substantially in size — a provider's flag set is much
/// larger than `completion <shell>`. Boxing is the usual remedy, but clap's
/// `Subcommand` derive requires each payload to implement `Args`, which `Box`
/// does not, and the enum is constructed exactly once per process from argv.
/// The indirection would cost a derive rewrite and buy nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the main banlieue controller (watches VirtualMachine CRs).
    Controller(banlieue_controller::Cli),

    /// Run the provider lifecycle operator.
    ///
    /// Watches `Provider` and `ProviderClass` CRs and creates one Deployment,
    /// ServiceAccount, Role, RoleBinding and ClusterRoleBinding per Provider,
    /// so applying a `Provider` is enough to bring a backend up.
    Operator(banlieue_operator::Cli),

    /// Run a backend provider controller.
    Provider(ProviderArgs),

    /// Install banlieue into a Kubernetes cluster.
    ///
    /// Builds CRDs from this binary's own Rust types, so the schemas applied
    /// are by construction the ones this binary implements. `--dry-run` prints
    /// the YAML instead and never contacts a cluster.
    Bootstrap(banlieue_operator::bootstrap::Cli),

    /// Run the provider-agnostic VMImage build pipeline: turns an
    /// OCI/Kairos-referenced VMImage source into a raw disk via
    /// kairos-operator, for providers to convert and import per zone.
    #[cfg(feature = "imagebuilder")]
    Imagebuilder(banlieue_imagebuilder::Cli),

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

/// Backends compiled into this binary.
///
/// Feature gating lives here, in the binary crate that owns the features, so a
/// slim build (`--no-default-features --features vsphere`) cannot offer to
/// bootstrap a backend it does not contain.
pub const COMPILED_BACKENDS: &[&str] = &[
    #[cfg(feature = "vsphere")]
    "vsphere",
    #[cfg(feature = "libvirt")]
    "libvirt",
];

/// `banlieue provider <backend>` — selects which backend provider to run.
#[derive(Debug, Args)]
pub struct ProviderArgs {
    #[command(subcommand)]
    pub backend: ProviderBackend,
}

/// Available backend providers. Each variant is gated behind its own Cargo
/// feature so disabled backends are not compiled or linked.
///
/// Size-skewed for the same reason as [`Command`], and boxed for the same
/// non-reason: parsed once, from argv.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub enum ProviderBackend {
    /// VMware vSphere / vCenter provider.
    #[cfg(feature = "vsphere")]
    Vsphere(banlieue_provider_vsphere::Cli),

    /// libvirt / KVM provider.
    #[cfg(feature = "libvirt")]
    Libvirt(banlieue_provider_libvirt::Cli),
}

/// Dispatch a parsed [`Cli`] to the selected role's `run` entry point.
///
/// # Errors
/// Propagates whatever error the selected role's `run` returns.
pub async fn dispatch(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Controller(args) => banlieue_controller::run(args).await,
        Command::Operator(args) => banlieue_operator::run(args).await,
        Command::Provider(provider) => dispatch_provider(provider.backend).await,
        Command::Bootstrap(args) => {
            banlieue_operator::bootstrap::run(args, COMPILED_BACKENDS).await
        }
        #[cfg(feature = "imagebuilder")]
        Command::Imagebuilder(args) => banlieue_imagebuilder::run(args).await,
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
        #[cfg(feature = "libvirt")]
        ProviderBackend::Libvirt(args) => banlieue_provider_libvirt::run(args).await,
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod cli_tests;
