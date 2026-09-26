// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue provider cloud-hypervisor import`: put one registry image into
//! every storage class's cache, and exit (ADR-0064 Decision 3).
//!
//! Runs in the template unit instance [`crate::vmimage::import_unit`] names, as
//! the provider's user, able to write the image caches and nothing else.
//! It re-checks its arguments against the host config rather than trusting
//! them: the reference must be a digest in the host's own repository, and
//! the file must be that digest's cache name.
//!
//! The image is pulled once, into the first class that lacks it only when
//! no class holds it yet; every other class gets a reflink (or a copy)
//! through a temporary name and a rename. A digest mismatch fails the pull
//! and nothing is renamed into place. Nothing here writes status: the
//! reconciler reads the cache and the unit's result.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use banlieue_oci::{Client, Credentials, Reference};
use clap::Args;
use tracing::info;

use crate::host_config::HostConfig;
use crate::sys;
use crate::vmimage::{check_reference, image_dirs};

/// A cached image: the provider's user and group, nobody else, as for an
/// admin-placed one.
pub const CACHE_FILE_MODE: u32 = 0o640;

/// Suffix of a copy not yet renamed into place.
const PARTIAL_SUFFIX: &str = ".partial";

/// Arguments for `banlieue provider cloud-hypervisor import`.
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// `registry/repository@sha256:<hex>`.
    #[arg(long)]
    pub reference: String,

    /// Cache file name: `sha256-<hex>.raw`.
    #[arg(long)]
    pub file: String,
}

/// Where the image comes from, and where it must go.
#[derive(Debug, PartialEq, Eq)]
pub struct CopyPlan {
    /// A class that already holds the file, if any: copied from, not pulled.
    pub source: Option<PathBuf>,
    /// Cache paths still missing it, in class order.
    pub missing: Vec<PathBuf>,
}

/// Decide from the caches as they are now.
#[must_use]
pub fn copy_plan(dirs: &[PathBuf], file: &str) -> CopyPlan {
    let paths: Vec<PathBuf> = dirs.iter().map(|d| d.join(file)).collect();
    CopyPlan {
        source: paths.iter().find(|p| p.is_file()).cloned(),
        missing: paths.into_iter().filter(|p| !p.is_file()).collect(),
    }
}

/// Reflink (or copy) `src` to `dest` through a temporary name.
///
/// # Errors
/// The I/O error; the temporary file is removed.
pub fn place_copy(src: &Path, dest: &Path) -> std::io::Result<()> {
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(PARTIAL_SUFFIX);
    let tmp = PathBuf::from(tmp);
    let _ = std::fs::remove_file(&tmp);
    let placed = sys::clone_file(src, &tmp).and_then(|(file, _)| {
        file.set_permissions(std::fs::Permissions::from_mode(CACHE_FILE_MODE))?;
        file.sync_all()?;
        std::fs::rename(&tmp, dest)
    });
    if placed.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    placed
}

/// Import and exit.
///
/// # Errors
/// A refused reference, a registry or digest failure, or a copy failure.
/// The unit then fails, and the reconciler reports why.
pub async fn run(config_path: &Path, args: ImportArgs) -> Result<()> {
    let config = HostConfig::load(config_path)
        .with_context(|| format!("loading {}", config_path.display()))?;
    let target = check_reference(&config, &args.reference).map_err(|(_, m)| anyhow::anyhow!(m))?;
    if target.file != args.file {
        bail!(
            "--file {} is not the cache name of {} ({})",
            args.file,
            target.reference,
            target.file
        );
    }
    let Some(registry) = config.registry.as_ref() else {
        bail!("no [registry] in {}", config_path.display());
    };

    let plan = copy_plan(&image_dirs(&config), &target.file);
    let (source, rest) = match plan.source {
        Some(src) => (src, plan.missing),
        None => {
            let Some((first, rest)) = plan.missing.split_first() else {
                return Ok(());
            };
            banlieue_oci::install_crypto_provider();
            let credentials = match &registry.credentials_dir {
                Some(dir) => Credentials::from_dir(dir)
                    .with_context(|| format!("registry credentials in {}", dir.display()))?,
                None => Credentials::anonymous(),
            };
            let client = Client::new(reqwest::Client::new(), credentials, registry.plain_http);
            let reference = Reference::parse(&target.reference)?;
            info!(reference = %target.reference, dest = %first.display(), "pulling");
            let pulled = client
                .pull_file(&reference, first)
                .await
                .with_context(|| format!("pulling {}", target.reference))?;
            // The image directory is the provider's alone (0750), so a path
            // here cannot have been swapped by a guest.
            std::fs::set_permissions(first, std::fs::Permissions::from_mode(CACHE_FILE_MODE))
                .with_context(|| format!("setting the mode of {}", first.display()))?;
            info!(bytes = pulled.len, "pulled and verified");
            (first.clone(), rest.to_vec())
        }
    };
    for dest in rest {
        place_copy(&source, &dest).with_context(|| format!("copying to {}", dest.display()))?;
        info!(dest = %dest.display(), "copied");
    }
    Ok(())
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod import_tests;
