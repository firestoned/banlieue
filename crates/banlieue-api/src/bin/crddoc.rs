// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Generate the Markdown API reference for every banlieue CRD.
//!
//! Walks the same code-first CRD schemas as `crdgen` (with the same root
//! description promotion) and emits a single Markdown page documenting every
//! kind, field, type, and description — the source for the docs site's
//! `Reference → API Reference` page.
//!
//! Build and run with:
//!     cargo run --bin crddoc --features crdgen
//!     cargo run --bin crddoc --features crdgen -- --out-file docs/src/reference/api.md
//!
//! With no `--out-file`, the page is written to stdout.
//!
//! CLI parsing is delegated to `clap` (consistent with `crdgen`, and dodges the
//! Semgrep `rust.lang.security.args.args` rule).

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use banlieue_api::banlieue::{Provider, VMClass, VMImage, VirtualMachine};
use banlieue_api::crddoc::render_reference;
use banlieue_api::crdgen_support::prepared;
use banlieue_api::infrastructure::{VSphereCluster, VSphereMachine, VSphereMachineTemplate};
use clap::Parser;
use kube::CustomResourceExt;

/// Generate the banlieue CRD API reference as Markdown.
#[derive(Debug, Parser)]
#[command(
    name = "crddoc",
    about = "Generate the Markdown API reference for every banlieue CRD",
    version
)]
struct Cli {
    /// File to write the Markdown page to. When omitted, writes to stdout.
    #[arg(long)]
    out_file: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Order defines the on-page order: user-facing group first, then infra.
    let crds = vec![
        prepared(Provider::crd()),
        prepared(VMClass::crd()),
        prepared(VMImage::crd()),
        prepared(VirtualMachine::crd()),
        prepared(VSphereCluster::crd()),
        prepared(VSphereMachine::crd()),
        prepared(VSphereMachineTemplate::crd()),
    ];

    let markdown = render_reference(&crds);

    match cli.out_file {
        Some(path) => {
            if let Some(parent) = path.parent()
                && let Err(e) = fs::create_dir_all(parent)
            {
                eprintln!("crddoc: failed to create {}: {e}", parent.display());
                return ExitCode::FAILURE;
            }
            if let Err(e) = fs::write(&path, markdown) {
                eprintln!("crddoc: failed to write {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
            eprintln!("✓ wrote {}", path.display());
        }
        None => print!("{markdown}"),
    }
    ExitCode::SUCCESS
}
