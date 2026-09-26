// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue imagebuilder push`: push one build artifact to an OCI registry
//! and exit (ADR-0064 Decision 2).
//!
//! Runs inside the Job [`crate::reconciler::push::build_push_job`] creates.
//! It never talks to the Kubernetes API: its only output is the
//! digest-pinned reference, written to the termination-message file for the
//! reconciler to read and check. The flags are stable, so a failed push can
//! be reproduced by hand.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use banlieue_oci::{Client, Credentials, Reference};
use clap::Args;
use tracing::info;

/// Kubernetes' default `terminationMessagePath`.
const TERMINATION_LOG: &str = "/dev/termination-log";

/// Arguments for `banlieue imagebuilder push`.
#[derive(Debug, Args)]
pub struct PushArgs {
    /// The artifact to push.
    #[arg(long)]
    pub source: PathBuf,

    /// `registry/repository:tag` to push to.
    #[arg(long)]
    pub target: String,

    /// Directory for the compressed copy, which can be as large as the
    /// artifact's data. Named explicitly (the push Job passes its
    /// emptyDir); `TMPDIR` is only a default.
    #[arg(long, env = "TMPDIR")]
    pub scratch_dir: PathBuf,

    /// OCI `artifactType` of the manifest.
    #[arg(long)]
    pub artifact_type: String,

    /// Manifest annotation, `key=value` (repeatable).
    #[arg(long = "annotation", value_name = "KEY=VALUE")]
    pub annotations: Vec<String>,

    /// Directory holding `username` and `password`: a mounted
    /// `kubernetes.io/basic-auth` Secret. Unset pushes anonymously.
    #[arg(long)]
    pub credentials_dir: Option<PathBuf>,

    /// Use `http://`. Only for a registry on a private network or a test.
    #[arg(long, default_value_t = false)]
    pub plain_http: bool,

    /// Where to write the pushed reference.
    #[arg(long, default_value = TERMINATION_LOG)]
    pub termination_log: PathBuf,
}

/// Parse repeated `key=value` flags.
///
/// # Errors
/// The first value without `=`, or with an empty key.
pub fn parse_annotations(values: &[String]) -> Result<BTreeMap<String, String>, String> {
    values
        .iter()
        .map(|v| match v.split_once('=') {
            Some((k, val)) if !k.is_empty() => Ok((k.to_string(), val.to_string())),
            _ => Err(format!("annotation {v:?} must be key=value")),
        })
        .collect()
}

/// Push and record the result.
///
/// # Errors
/// Any argument, credential, registry or I/O failure; the Job then fails.
pub async fn run(args: PushArgs) -> Result<()> {
    banlieue_oci::install_crypto_provider();
    let target = Reference::parse(&args.target).context("--target")?;
    let annotations = parse_annotations(&args.annotations).map_err(anyhow::Error::msg)?;
    let credentials = match &args.credentials_dir {
        Some(dir) => Credentials::from_dir(dir).context("--credentials-dir")?,
        None => Credentials::anonymous(),
    };
    let client = Client::new(reqwest::Client::new(), credentials, args.plain_http);

    info!(source = %args.source.display(), %target, "pushing build artifact");
    let pushed = client
        .push_file(
            &target,
            &args.source,
            &args.scratch_dir,
            &args.artifact_type,
            annotations,
        )
        .await
        .with_context(|| format!("pushing {} to {target}", args.source.display()))?;
    let reference = pushed.reference.to_string();
    info!(%reference, layer_size = pushed.layer_size, "pushed");

    std::fs::write(&args.termination_log, &reference)
        .with_context(|| format!("writing {}", args.termination_log.display()))?;
    println!("{reference}");
    Ok(())
}

#[cfg(test)]
#[path = "oci_push_tests.rs"]
mod oci_push_tests;
