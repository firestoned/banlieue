// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue
//!
//! The single banlieue executable. It packages every controller role behind a
//! subcommand tree (see ADR-0004) and does nothing but parse and dispatch:
//!
//! ```text
//! banlieue controller [flags]
//! banlieue provider vsphere [flags]
//! ```
//!
//! Each role lives in an independent library crate with its own dependency
//! graph; providers are gated behind per-provider Cargo features (default =
//! all available). The actual lifecycle (logging, health, leader election,
//! reconcilers) lives in those library crates, not here.

mod cli;

use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    cli::dispatch(cli).await
}
