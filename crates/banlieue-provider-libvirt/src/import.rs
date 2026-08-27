// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue provider libvirt import` — the data path of ADR-0010 / ADR-0011.
//!
//! The `VMImage` reconciler creates one Job per storage pool; each Job runs
//! this subcommand. It streams the raw disk `banlieue-imagebuilder` produced
//! into a libvirt storage volume and exits. Nothing here reconciles, and
//! nothing here writes status — the reconciler reads the Job's own
//! success/failure and translates it into `status.perProvider[].zones[]`.
//!
//! **Why the Job re-reads the `Provider` instead of taking the endpoint on the
//! command line.** The TLS material is a Secret and the CA bundle may be an
//! inline value, a ConfigMap, or another Secret ([`crate::credentials`]).
//! Flattening all three onto argv would mean either passing a private key as a
//! process argument — visible in `/proc` to anything sharing the namespace —
//! or teaching the reconciler to project three different shapes into a volume.
//! Reading the `Provider` reuses the resolver the controller already trusts,
//! under the *same* ServiceAccount, whose Role is already `resourceNames`-
//! scoped to exactly this Provider and its Secret. The Job gains no authority
//! the controller did not already have.
//!
//! The upload is idempotent: a volume that is already present is left alone.
//! That matters because `backoffLimit` is 1 — the one retry must be able to
//! finish the job rather than trip over its predecessor's work.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result as AnyhowResult};
use banlieue_api::banlieue::Provider;
use banlieue_libvirt::{
    StoragePool, StorageVol, connect_open, connect_tls, list_all_storage_pools, raw_volume_xml,
    storage_pool_list_all_volumes, storage_vol_create_xml, storage_vol_upload,
};
use banlieue_provider_sdk::client::build_client;
use clap::Args;
use kube::Api;
use tokio::fs::File;
use tracing::info;

use crate::client::{LOCAL_DRIVER_URI, parse_endpoint};
use crate::error::{Error, Result};

/// Suffix identifying the uploaded volume's on-disk format.
const RAW_SUFFIX: &str = ".raw";

/// Read size when hashing the artifact.
///
/// Large enough that a multi-gigabyte disk is not thousands of syscalls, small
/// enough that the buffer is irrelevant next to the image itself.
const HASH_CHUNK_BYTES: usize = 1024 * 1024;

/// Arguments for `banlieue provider libvirt import`.
///
/// Every one of these is set by [`crate::reconciler::vmimage::build_import_job`];
/// they are exposed as flags so the same import can be reproduced by hand when
/// diagnosing a failed Job.
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Name of the `VMImage` being imported. Names the volume by default.
    #[arg(long)]
    pub vmimage: String,

    /// Name of the `Provider` whose host to import into.
    #[arg(long)]
    pub provider: String,

    /// Namespace of that `Provider`. Not necessarily this Job's own namespace:
    /// the Job runs beside the artifacts PVC, which lives in the build
    /// namespace.
    #[arg(long)]
    pub provider_namespace: String,

    /// Storage pool to import into.
    #[arg(long)]
    pub pool: String,

    /// Path to the raw disk to upload, inside the mounted artifacts volume.
    #[arg(long)]
    pub source: PathBuf,

    /// Override the destination volume name. Defaults to `<vmimage>.raw`.
    #[arg(long)]
    pub volume_name: Option<String>,

    /// Expected checksum of the source artifact, `<alg>:<hex>` (sha256 or
    /// sha512). When set, the artifact is hashed before anything touches the
    /// host, and a mismatch — or an unsupported algorithm — fails the Job
    /// closed (SEC-004).
    #[arg(long)]
    pub checksum: Option<String>,
}

/// What the import needs to do, once the host has been inspected.
#[derive(Debug, PartialEq, Eq)]
pub enum ImportPlan {
    /// The volume is already there; this run is a no-op.
    AlreadyPresent {
        /// Backend key of the existing volume — for a directory pool, its path.
        key: String,
    },
    /// The volume must be created and streamed in.
    Upload,
}

/// Destination volume name for an import.
///
/// Deterministic: a retry must target what the previous attempt created, or
/// [`plan`] can never detect completed work.
#[must_use]
pub fn volume_name(vmimage: &str, explicit: Option<&str>) -> String {
    if let Some(name) = explicit {
        return name.to_string();
    }
    if vmimage.ends_with(RAW_SUFFIX) {
        return vmimage.to_string();
    }
    format!("{vmimage}{RAW_SUFFIX}")
}

/// Find the requested pool among those the host reports.
///
/// # Errors
/// [`Error::Invalid`] naming both the requested pool and the available ones —
/// the Job is unattended, so its error text is the whole diagnostic.
pub fn find_pool<'a>(pools: &'a [StoragePool], name: &str) -> Result<&'a StoragePool> {
    pools
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| Error::Invalid {
            what: "--pool",
            detail: format!(
                "pool {name:?} does not exist on this host; available: [{}]",
                pools
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })
}

/// Decide whether the volume still needs uploading.
#[must_use]
pub fn plan(volumes: &[StorageVol], volume: &str) -> ImportPlan {
    match volumes.iter().find(|v| v.name == volume) {
        Some(v) => ImportPlan::AlreadyPresent { key: v.key.clone() },
        None => ImportPlan::Upload,
    }
}

/// Size of the artifact to upload.
///
/// The volume is created with exactly this capacity, so a wrong answer either
/// truncates the guest disk or wastes pool space.
///
/// # Errors
/// [`Error::Invalid`] if the file cannot be read or is empty. A zero-byte
/// artifact means the build produced nothing; creating a zero-capacity volume
/// would report success and leave an unbootable image behind.
pub async fn source_length(path: &Path) -> Result<u64> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| Error::Invalid {
            what: "--source",
            detail: format!("cannot read {}: {e}", path.display()),
        })?;
    let len = meta.len();
    if len == 0 {
        return Err(Error::Invalid {
            what: "--source",
            detail: format!("{} is empty; the build produced no disk", path.display()),
        });
    }
    Ok(len)
}

/// Verify the artifact against an expected `<alg>:<hex>` checksum.
///
/// Streams the file so multi-gigabyte disks never sit in memory. Fails
/// **closed**: an unsupported algorithm is an error, not a skip — a declared
/// checksum that cannot be checked must block the import, because the whole
/// point of the field is that nothing unverified reaches a backend (SEC-004).
///
/// # Errors
/// [`Error::Invalid`] if the algorithm is unsupported, the file cannot be
/// read, or the digest does not match (the error names expected and actual).
pub async fn verify_checksum(path: &Path, expected: &str) -> Result<()> {
    use sha2::Digest;
    use tokio::io::AsyncReadExt;

    let (alg, expected_hex) = expected.split_once(':').ok_or_else(|| Error::Invalid {
        what: "--checksum",
        detail: format!("expected `<alg>:<hex>`, got {expected:?}"),
    })?;

    let mut file = File::open(path).await.map_err(|e| Error::Invalid {
        what: "--source",
        detail: format!("cannot read {}: {e}", path.display()),
    })?;

    // Hash in chunks; the digest type is picked once from the algorithm.
    let mut buf = vec![0u8; HASH_CHUNK_BYTES];
    let actual_hex = match alg {
        "sha256" => {
            let mut h = sha2::Sha256::new();
            loop {
                let n = file.read(&mut buf).await.map_err(|e| Error::Invalid {
                    what: "--source",
                    detail: format!("cannot read {}: {e}", path.display()),
                })?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            hex_encode(&h.finalize())
        }
        "sha512" => {
            let mut h = sha2::Sha512::new();
            loop {
                let n = file.read(&mut buf).await.map_err(|e| Error::Invalid {
                    what: "--source",
                    detail: format!("cannot read {}: {e}", path.display()),
                })?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            hex_encode(&h.finalize())
        }
        other => {
            return Err(Error::Invalid {
                what: "--checksum",
                detail: format!(
                    "unsupported algorithm {other:?} (supported: sha256, sha512); \
                     refusing to import an unverified artifact"
                ),
            });
        }
    };

    if !actual_hex.eq_ignore_ascii_case(expected_hex) {
        return Err(Error::Invalid {
            what: "--checksum",
            detail: format!(
                "checksum mismatch for {}: expected {expected}, computed {alg}:{actual_hex}; \
                 refusing to import a corrupted or substituted artifact",
                path.display()
            ),
        });
    }
    info!(path = %path.display(), algorithm = %alg, "artifact checksum verified");
    Ok(())
}

/// Lowercase hex encoding of a digest. `sha2`'s `Output` (a `hybrid_array::Array`,
/// as of `digest` 0.11) does not implement `LowerHex` the way the old
/// `GenericArray`-backed output did, so this replaces a `format!("{:x}", ...)`
/// that stopped compiling on the `sha2` 0.11 upgrade.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Run one import to completion.
///
/// # Errors
/// Any failure to read the `Provider`, resolve its TLS material, reach the
/// host, or stream the artifact. Every path here exits non-zero so the Job
/// records a failure the reconciler can surface.
pub async fn run(args: ImportArgs) -> AnyhowResult<()> {
    let volume = volume_name(&args.vmimage, args.volume_name.as_deref());
    info!(
        vmimage = %args.vmimage,
        provider = %args.provider,
        pool = %args.pool,
        volume = %volume,
        source = %args.source.display(),
        "starting libvirt image import"
    );

    // Sized before contacting the host: a bad artifact should fail without
    // having created a half-formed volume anyone has to clean up.
    let length = source_length(&args.source).await?;

    // SEC-004: verify before any side effect. A substituted or corrupted
    // artifact must never reach the backend — the Job fails here, before the
    // volume exists.
    if let Some(expected) = args.checksum.as_deref() {
        verify_checksum(&args.source, expected).await?;
    }

    let client = build_client().await.context("constructing kube client")?;
    let api: Api<Provider> = Api::namespaced(client.clone(), &args.provider_namespace);
    let provider = api
        .get(&args.provider)
        .await
        .with_context(|| format!("reading Provider {}", args.provider))?;

    let identity = crate::credentials::resolve(&client, &args.provider_namespace, &provider)
        .await
        .context("resolving libvirt TLS credentials")?;

    let (host, port) = parse_endpoint(&provider.spec.connection.endpoint)?;
    let mut session = connect_tls(&host, port, &identity)
        .await
        .map_err(Error::from)
        .with_context(|| format!("connecting to {host}:{port}"))?;
    connect_open(&mut session, Some(LOCAL_DRIVER_URI), false)
        .await
        .map_err(Error::from)
        .context("opening libvirt session")?;

    let pools = list_all_storage_pools(&mut session)
        .await
        .map_err(Error::from)
        .context("listing storage pools")?;
    let pool = find_pool(&pools, &args.pool)?;

    let existing = storage_pool_list_all_volumes(&mut session, pool)
        .await
        .map_err(Error::from)
        .with_context(|| format!("listing volumes in pool {}", pool.name))?;

    if let ImportPlan::AlreadyPresent { key } = plan(&existing, &volume) {
        info!(volume = %volume, key = %key, "volume already present; nothing to do");
        return Ok(());
    }

    info!(volume = %volume, bytes = length, "creating volume");
    let vol = storage_vol_create_xml(&mut session, pool, &raw_volume_xml(&volume, length))
        .await
        .map_err(Error::from)
        .with_context(|| format!("creating volume {volume} in pool {}", pool.name))?;

    let mut file = File::open(&args.source)
        .await
        .with_context(|| format!("opening {}", args.source.display()))?;
    storage_vol_upload(&mut session, &vol, &mut file, length)
        .await
        .map_err(Error::from)
        .with_context(|| format!("uploading {} bytes to {volume}", length))?;

    info!(volume = %volume, key = %vol.key, bytes = length, "import complete");
    Ok(())
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod import_tests;
