// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Emit every banlieue CRD as YAML.
//!
//! Default behaviour: stream every CRD to stdout as a multi-document YAML
//! file (useful for piping into `kubectl apply -f -`).
//!
//! When invoked with `--out-dir <DIR>`, writes one file per CRD into the
//! directory using the kubebuilder convention `<group>_<plural>.yaml`. This is
//! how `make crds` populates `deploy/crds/`.
//!
//! Build and run with:
//!     cargo run --bin crdgen --features crdgen
//!     cargo run --bin crdgen --features crdgen -- --out-dir deploy/crds
//!
//! CLI parsing is delegated to `clap` rather than reading `std::env::args()`
//! directly — keeps `--help` / `--version` consistent with the rest of the
//! workspace and dodges the Semgrep `rust.lang.security.args.args` rule that
//! flags any direct use of `args()` even when only used for non-security
//! flag parsing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use banlieue_api::banlieue::{Provider, VMClass, VMImage, VirtualMachine};
use banlieue_api::infrastructure::{VSphereMachine, VSphereMachineTemplate};
use clap::Parser;
use kube::CustomResourceExt;

/// Emit banlieue CRDs as YAML.
#[derive(Debug, Parser)]
#[command(
    name = "crdgen",
    about = "Generate CRD YAML for every banlieue type",
    version
)]
struct Cli {
    /// Directory to write one file per CRD into (kubebuilder convention).
    /// When omitted, every CRD is streamed to stdout as a multi-document YAML.
    #[arg(long)]
    out_dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let crds: Vec<(&str, String)> = vec![
        ("banlieue.io_providers.yaml", render(&Provider::crd())),
        (
            "banlieue.io_virtualmachines.yaml",
            render(&VirtualMachine::crd()),
        ),
        ("banlieue.io_vmclasses.yaml", render(&VMClass::crd())),
        ("banlieue.io_vmimages.yaml", render(&VMImage::crd())),
        (
            "infrastructure.banlieue.io_vspheremachines.yaml",
            render(&VSphereMachine::crd()),
        ),
        (
            "infrastructure.banlieue.io_vspheremachinetemplates.yaml",
            render(&VSphereMachineTemplate::crd()),
        ),
    ];

    match cli.out_dir {
        Some(dir) => write_per_file(&dir, &crds),
        None => write_stdout(&crds),
    }
}

fn render<T: serde::Serialize>(crd: &T) -> String {
    serde_yaml::to_string(crd).expect("serialize CRD to YAML")
}

fn write_stdout(crds: &[(&str, String)]) -> ExitCode {
    for (i, (_, doc)) in crds.iter().enumerate() {
        if i > 0 {
            println!("---");
        }
        print!("{doc}");
    }
    ExitCode::SUCCESS
}

fn write_per_file(dir: &Path, crds: &[(&str, String)]) -> ExitCode {
    if let Err(e) = fs::create_dir_all(dir) {
        eprintln!("crdgen: failed to create {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    for (filename, doc) in crds {
        let path = dir.join(filename);
        if let Err(e) = fs::write(&path, doc) {
            eprintln!("crdgen: failed to write {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        eprintln!("✓ wrote {}", path.display());
    }
    ExitCode::SUCCESS
}
