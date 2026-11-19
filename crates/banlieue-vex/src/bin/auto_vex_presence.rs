// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # Presence-based auto-VEX generator (CI tool)
//!
//! Reads a Grype JSON report and one or more CycloneDX SBOMs, cross-checks
//! the set of hand-authored statements in `.vex/*.json`, and emits an
//! OpenVEX document containing `not_affected + component_not_present`
//! statements for every Grype finding whose affected `purl` is absent from
//! every SBOM and whose CVE identifier is not already triaged.
//!
//! ## Usage
//!
//! ```bash
//! cargo run --bin auto-vex-presence -- \
//!     --grype-json grype.json \
//!     --sbom docker-sbom-Chainguard.json \
//!     --sbom docker-sbom-Distroless.json \
//!     --vex-dir .vex \
//!     --product-purl pkg:oci/banlieue \
//!     --id "https://github.com/firestoned/banlieue/actions/runs/123/auto-vex-presence" \
//!     --author auto-vex-presence \
//!     --output vex.auto-presence.json
//! ```
//!
//! All logic lives in [`banlieue_vex::auto_vex_presence`]; this binary is
//! strictly a clap-driven CLI + file I/O wrapper.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;

use banlieue_vex::auto_vex_presence::{
    build_document, compute_presence_vex, load_sbom_from_path, load_triaged_from_vex_dir,
};
use banlieue_vex::cli::{load_grype, timestamp_or_now, write_document};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "auto-vex-presence",
    about = "Emit OpenVEX component_not_present statements for Grype findings absent from SBOMs"
)]
struct Cli {
    /// Grype JSON report (produced by `grype --output json`).
    #[arg(long)]
    grype_json: PathBuf,

    /// One or more CycloneDX SBOM JSON files. A purl present in ANY SBOM
    /// counts as "component present" and excludes the finding.
    #[arg(long, required = true)]
    sbom: Vec<PathBuf>,

    /// Directory of hand-authored `.vex/*.json` statements; CVEs already
    /// covered by one of these are skipped. Missing directory is OK.
    #[arg(long)]
    vex_dir: PathBuf,

    /// Product purl to attach to every emitted statement (e.g.
    /// `pkg:oci/banlieue`).
    #[arg(long)]
    product_purl: String,

    /// Canonical `@id` for the document (typically a URL of the CI run).
    #[arg(long)]
    id: String,

    /// Document-level author (typically `auto-vex-presence`).
    #[arg(long, default_value = "auto-vex-presence")]
    author: String,

    /// RFC-3339 UTC timestamp. Defaults to now() at process start.
    #[arg(long)]
    timestamp: Option<String>,

    /// Output path. Defaults to stdout.
    #[arg(long)]
    output: Option<PathBuf>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("auto-vex-presence: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let grype = load_grype(&cli.grype_json)?;

    let mut sboms = Vec::with_capacity(cli.sbom.len());
    for path in &cli.sbom {
        let sbom = load_sbom_from_path(path)
            .map_err(|e| anyhow::anyhow!("failed to load --sbom {}: {}", path.display(), e))?;
        sboms.push(sbom);
    }

    let triaged: HashSet<String> = load_triaged_from_vex_dir(&cli.vex_dir).map_err(|e| {
        anyhow::anyhow!("failed to load --vex-dir {}: {}", cli.vex_dir.display(), e)
    })?;

    let timestamp = timestamp_or_now(cli.timestamp);

    let statements = compute_presence_vex(&grype, &sboms, &triaged, &cli.product_purl, &timestamp);
    let doc = build_document(statements, &cli.id, &cli.author, &timestamp);
    write_document(&doc, cli.output.as_deref())?;

    eprintln!(
        "auto-vex-presence: {} grype match(es) -> {} statement(s) emitted ({} already triaged in {})",
        grype.matches.len(),
        doc.statements.len(),
        triaged.len(),
        cli.vex_dir.display(),
    );
    Ok(())
}
