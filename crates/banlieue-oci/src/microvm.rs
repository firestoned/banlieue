// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The microvm artifact (ADR-0142): a kernel, an optional initramfs and a root
//! filesystem as the three layers of one OCI manifest, identified by the
//! manifest digest.
//!
//! This module is the one implementation of the format (Decision 7): it
//! builds and strictly parses the manifest, pushes the three files, and pulls
//! them into a cache directory
//!
//! ```text
//! images/sha256-<manifest hex>/{kernel,initramfs,rootfs,boot.json}
//! ```
//!
//! through a temporary directory and one `rename`, so nothing partial is ever
//! visible. The pull fails closed (Decision 6): an unknown or misordered
//! layer, a missing required layer, a config that is not schema 1, an
//! architecture that is not the host's, a gzip layer that decompresses to
//! more or less than its annotated length, a layer over the caller's ceiling,
//! or a digest mismatch each refuse the image and leave nothing behind.
//!
//! The single-layer disk API in [`crate::client`] is unchanged.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use flate2::write::GzDecoder;
use futures::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tracing::{debug, info};

use crate::client::{Client, IO_BUF, check, compressed_copy_in, gzip_and_hash};
use crate::error::{Error, Result};
use crate::manifest::{
    Descriptor, MANIFEST_MEDIA_TYPE, Manifest, SCHEMA_VERSION, hex, sha256_digest,
};
use crate::reference::{Reference, SHA256_PREFIX, require_digest};
use crate::sparse::SparseWriter;

/// Artifact type of a microvm image.
pub const ARTIFACT_TYPE: &str = "application/vnd.banlieue.microvm.v1";
/// Config blob media type.
pub const CONFIG_MEDIA_TYPE: &str = "application/vnd.banlieue.microvm.config.v1+json";
/// Kernel layer media type: the uncompressed `vmlinux`, gzip-compressed.
pub const LAYER_KERNEL: &str = "application/vnd.banlieue.microvm.kernel.v1+gzip";
/// Initramfs layer media type: the cpio archive, gzip-compressed.
pub const LAYER_INITRAMFS: &str = "application/vnd.banlieue.microvm.initramfs.v1+gzip";
/// Root filesystem layer media type: the file as built, not recompressed.
pub const LAYER_ROOTFS: &str = "application/vnd.banlieue.microvm.rootfs.v1";
/// Layer annotation: the decompressed length of a gzip layer, in bytes.
pub const ANNOTATION_LAYER_SIZE: &str = "io.banlieue.layer.size";
/// The only config schema version.
pub const CONFIG_SCHEMA_VERSION: u32 = 1;
/// File name of the kernel in a cache entry.
pub const KERNEL_FILE: &str = "kernel";
/// File name of the initramfs in a cache entry.
pub const INITRAMFS_FILE: &str = "initramfs";
/// File name of the root filesystem in a cache entry.
pub const ROOTFS_FILE: &str = "rootfs";
/// File name of the config blob, verbatim, in a cache entry.
pub const BOOT_FILE: &str = "boot.json";

/// A config blob is a few short fields; a larger one is not ours.
const CONFIG_MAX_BYTES: u64 = 64 * 1024;
/// Mode of a cache entry's directory: traversable by the guest's uid
/// (ADR-0142 Decision 3).
const DIR_MODE: u32 = 0o755;
/// Mode of a cache entry's files: readable by the guest's uid.
const FILE_MODE: u32 = 0o644;
/// Prefix of the temporary directory a pull builds in.
const PULL_TMP_PREFIX: &str = ".pull-";
/// Arguments the provider composes itself and an image may never set, as
/// whole arguments (the part before any `=`). Decision 5.
const PLATFORM_ARGS: [&str; 8] = [
    "root", "ro", "rw", "ip", "reboot", "panic", "pci", "console",
];

/// A CPU architecture an image is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Architecture {
    /// x86-64.
    Amd64,
    /// aarch64.
    Arm64,
}

impl Architecture {
    /// The architecture this process runs on.
    ///
    /// # Errors
    /// [`Error::Manifest`] on an architecture the contract does not name.
    pub fn host() -> Result<Self> {
        match std::env::consts::ARCH {
            "x86_64" => Ok(Self::Amd64),
            "aarch64" => Ok(Self::Arm64),
            other => Err(Error::Manifest(format!(
                "host architecture {other} is neither amd64 nor arm64"
            ))),
        }
    }
}

/// The config blob (schema version 1). Nothing else may appear in it; a
/// field is added by amending ADR-0142.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MicrovmConfig {
    /// Always [`CONFIG_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// What the image is built for; must equal the host's.
    pub architecture: Architecture,
    /// The image's own kernel arguments; may not set a platform argument.
    pub cmdline: String,
    /// The least guest memory, in MiB, the image boots in.
    #[serde(rename = "minMemoryMiB")]
    pub min_memory_mib: u32,
}

impl MicrovmConfig {
    /// Parse and validate a config blob.
    ///
    /// # Errors
    /// [`Error::Manifest`] when it is not JSON of this exact shape, or fails
    /// [`Self::validate`].
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let config: Self = serde_json::from_slice(bytes)
            .map_err(|e| Error::Manifest(format!("microvm config: {e}")))?;
        config.validate()?;
        Ok(config)
    }

    /// Check the config is schema 1, has a usable memory floor and a
    /// `cmdline` that sets no platform argument.
    ///
    /// # Errors
    /// [`Error::Manifest`] naming what is wrong.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(Error::Manifest(format!(
                "config schemaVersion {} is not {CONFIG_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        if self.min_memory_mib == 0 {
            return Err(Error::Manifest(
                "config minMemoryMiB must be above 0".into(),
            ));
        }
        refuse_platform_args(&self.cmdline)
    }
}

/// Refuse an image `cmdline` that sets an argument the provider owns
/// (`root=`, `ro`, `rw`, `ip=`, `reboot=`, `panic=`, `pci=`, `console=`),
/// naming the first offender. Arguments are whole whitespace-separated
/// words, so `rootfstype=` or `rodata=` are not refused.
///
/// # Errors
/// [`Error::Manifest`] naming the offending argument.
pub fn refuse_platform_args(cmdline: &str) -> Result<()> {
    for arg in cmdline.split_whitespace() {
        let key = arg.trim_start_matches('"');
        let key = key.split_once('=').map_or(key, |(k, _)| k);
        if PLATFORM_ARGS.contains(&key) {
            return Err(Error::Manifest(format!(
                "cmdline sets the platform argument {arg:?}, which the provider owns"
            )));
        }
    }
    Ok(())
}

/// The descriptors of a well-formed microvm manifest, by role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicrovmLayout {
    /// The config blob.
    pub config: Descriptor,
    /// The kernel layer.
    pub kernel: Descriptor,
    /// The initramfs layer, when the image has one.
    pub initramfs: Option<Descriptor>,
    /// The root filesystem layer.
    pub rootfs: Descriptor,
}

/// Build the manifest for `layout`: layers in the fixed order kernel,
/// initramfs (if any), rootfs.
#[must_use]
pub fn build_manifest(layout: &MicrovmLayout) -> Manifest {
    let mut layers = vec![layout.kernel.clone()];
    layers.extend(layout.initramfs.clone());
    layers.push(layout.rootfs.clone());
    Manifest {
        schema_version: SCHEMA_VERSION,
        media_type: MANIFEST_MEDIA_TYPE.to_string(),
        artifact_type: Some(ARTIFACT_TYPE.to_string()),
        config: layout.config.clone(),
        layers,
        annotations: std::collections::BTreeMap::new(),
    }
}

/// The role a layer media type plays, if it is one of ours.
fn role(media_type: &str) -> Option<&'static str> {
    match media_type {
        LAYER_KERNEL => Some("kernel"),
        LAYER_INITRAMFS => Some("initramfs"),
        LAYER_ROOTFS => Some("rootfs"),
        _ => None,
    }
}

/// Take the next layer, requiring it to be `want`.
fn expect_layer<'a>(
    layers: &mut std::slice::Iter<'a, Descriptor>,
    want: &str,
    media_type: &str,
) -> Result<&'a Descriptor> {
    let layer = layers
        .next()
        .ok_or_else(|| Error::Manifest(format!("{want} layer is missing")))?;
    if layer.media_type == media_type {
        return Ok(layer);
    }
    Err(wrong_layer(want, &layer.media_type))
}

/// Why `found` is not the layer wanted at this position.
fn wrong_layer(want: &str, found: &str) -> Error {
    match role(found) {
        Some(r) => Error::Manifest(format!(
            "expected the {want} layer, found {r}: layers are out of order or repeated"
        )),
        None => Error::Manifest(format!("unknown layer media type {found}")),
    }
}

/// Strictly parse `manifest` as a microvm image.
///
/// # Errors
/// [`Error::Manifest`] unless it has this artifact type and config media
/// type, and layers exactly kernel, optional initramfs, rootfs, in that
/// order, each with a valid digest and (for gzip layers) a size annotation;
/// [`Error::Reference`] for a malformed digest.
pub fn parse_manifest(manifest: &Manifest) -> Result<MicrovmLayout> {
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(Error::Manifest(format!(
            "schema version {} is not {SCHEMA_VERSION}",
            manifest.schema_version
        )));
    }
    if manifest.media_type != MANIFEST_MEDIA_TYPE {
        return Err(Error::Manifest(format!(
            "manifest media type {} is not {MANIFEST_MEDIA_TYPE}",
            manifest.media_type
        )));
    }
    if manifest.artifact_type.as_deref() != Some(ARTIFACT_TYPE) {
        return Err(Error::Manifest(format!(
            "artifactType {:?} is not {ARTIFACT_TYPE}",
            manifest.artifact_type
        )));
    }
    if manifest.config.media_type != CONFIG_MEDIA_TYPE {
        return Err(Error::Manifest(format!(
            "config media type {} is not {CONFIG_MEDIA_TYPE}",
            manifest.config.media_type
        )));
    }
    let mut layers = manifest.layers.iter();
    let kernel = expect_layer(&mut layers, "kernel", LAYER_KERNEL)?.clone();
    let next = layers
        .next()
        .ok_or_else(|| Error::Manifest("rootfs layer is missing".into()))?;
    let (initramfs, rootfs) = if next.media_type == LAYER_INITRAMFS {
        let rootfs = expect_layer(&mut layers, "rootfs", LAYER_ROOTFS)?;
        (Some(next.clone()), rootfs.clone())
    } else if next.media_type == LAYER_ROOTFS {
        (None, next.clone())
    } else {
        return Err(wrong_layer("initramfs or rootfs", &next.media_type));
    };
    if let Some(extra) = layers.next() {
        return Err(Error::Manifest(format!(
            "layer {} follows the rootfs: layers are out of order or repeated",
            extra.media_type
        )));
    }
    let layout = MicrovmLayout {
        config: manifest.config.clone(),
        kernel,
        initramfs,
        rootfs,
    };
    check_descriptors(&layout)?;
    Ok(layout)
}

/// Digests well formed, and a size annotation on each gzip layer.
fn check_descriptors(layout: &MicrovmLayout) -> Result<()> {
    require_digest(&layout.config.digest)?;
    require_digest(&layout.rootfs.digest)?;
    for gz in std::iter::once(&layout.kernel).chain(layout.initramfs.as_ref()) {
        require_digest(&gz.digest)?;
        layer_size(gz)?;
    }
    Ok(())
}

/// The decompressed length a gzip layer declares.
fn layer_size(layer: &Descriptor) -> Result<u64> {
    layer
        .annotations
        .get(ANNOTATION_LAYER_SIZE)
        .ok_or_else(|| Error::Manifest(format!("layer has no {ANNOTATION_LAYER_SIZE}")))?
        .parse()
        .map_err(|_| Error::Manifest(format!("{ANNOTATION_LAYER_SIZE} is not a size")))
}

/// The three files of an image on local disk.
#[derive(Clone, Copy, Debug)]
pub struct MicrovmFiles<'a> {
    /// The uncompressed `vmlinux`.
    pub kernel: &'a Path,
    /// The cpio archive, if the image has one.
    pub initramfs: Option<&'a Path>,
    /// The root filesystem, as built.
    pub rootfs: &'a Path,
}

/// What a push produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicrovmPushed {
    /// The image, addressed by its manifest digest.
    pub reference: Reference,
}

/// What a pull produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicrovmPulled {
    /// The cache entry, `<images_dir>/sha256-<manifest hex>`.
    pub dir: PathBuf,
    /// The image's config.
    pub config: MicrovmConfig,
}

/// A layer ready to upload: its descriptor and the file holding its bytes.
struct Staged {
    descriptor: Descriptor,
    path: PathBuf,
    /// Keeps a compressed copy alive until the push ends, then removes it.
    _copy: Option<tempfile::NamedTempFile>,
}

fn blocking_failed(e: &tokio::task::JoinError) -> Error {
    Error::Io(std::io::Error::other(e.to_string()))
}

/// Gzip `path` into `scratch` and describe the compressed copy.
async fn stage_gzip(path: &Path, media_type: &str, scratch: &Path) -> Result<Staged> {
    let source_len = std::fs::metadata(path)?.len();
    let copy = compressed_copy_in(scratch, path)?;
    let out = copy.reopen()?;
    let src = path.to_path_buf();
    let (digest, size) = tokio::task::spawn_blocking(move || gzip_and_hash(&src, out))
        .await
        .map_err(|e| blocking_failed(&e))??;
    Ok(Staged {
        descriptor: Descriptor {
            media_type: media_type.to_string(),
            digest,
            size,
            annotations: [(ANNOTATION_LAYER_SIZE.to_string(), source_len.to_string())].into(),
        },
        path: copy.path().to_path_buf(),
        _copy: Some(copy),
    })
}

/// Describe `path` as it is, hashing it in one pass.
async fn stage_raw(path: &Path, media_type: &str) -> Result<Staged> {
    let src = path.to_path_buf();
    let (digest, size) = tokio::task::spawn_blocking(move || hash_file(&src))
        .await
        .map_err(|e| blocking_failed(&e))??;
    Ok(Staged {
        descriptor: Descriptor {
            media_type: media_type.to_string(),
            digest,
            size,
            annotations: std::collections::BTreeMap::new(),
        },
        path: path.to_path_buf(),
        _copy: None,
    })
}

/// `sha256:<hex>` and length of a file.
fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut reader = BufReader::with_capacity(IO_BUF, File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; IO_BUF];
    let mut len: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        len += n as u64;
    }
    Ok((format!("{SHA256_PREFIX}{}", hex(&hasher.finalize())), len))
}

/// How a layer's bytes become a file.
#[derive(Clone, Copy)]
enum Encoding {
    /// Gunzip to exactly this many bytes.
    Gzip(u64),
    /// Store as received.
    Raw,
}

/// The sink a layer's bytes are written to: a gunzipping sparse writer or a
/// plain file.
enum Sink {
    Gzip(Box<GzDecoder<SparseWriter>>, u64),
    Raw(File),
}

impl Sink {
    fn open(file: File, encoding: Encoding) -> Self {
        match encoding {
            Encoding::Gzip(len) => Self::Gzip(
                Box::new(GzDecoder::new(SparseWriter::with_limit(file, len))),
                len,
            ),
            Encoding::Raw => Self::Raw(file),
        }
    }

    /// Feed a chunk of the (compressed) blob.
    fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        match self {
            Self::Gzip(decoder, _) => decoder.write_all(chunk),
            Self::Raw(file) => file.write_all(chunk),
        }
    }

    /// Flush and check the length the layer declared; returns the file.
    fn finish(self, name: &str, received: u64, declared: u64) -> Result<File> {
        match self {
            Self::Gzip(decoder, expected) => {
                let writer = decoder.finish().map_err(|e| {
                    Error::Manifest(format!("{name} is not a complete gzip stream: {e}"))
                })?;
                let (file, len) = writer.finish()?;
                if len != expected {
                    return Err(Error::Manifest(format!(
                        "{name} decompressed to {len} bytes, annotated {expected}"
                    )));
                }
                Ok(file)
            }
            Self::Raw(file) => {
                if received != declared {
                    return Err(Error::Manifest(format!(
                        "{name} is {received} bytes, declared {declared}"
                    )));
                }
                Ok(file)
            }
        }
    }
}

/// Refuse a layer whose compressed or decompressed size is over `max`.
fn check_ceiling(name: &str, layer: &Descriptor, max: u64) -> Result<()> {
    if layer.size > max {
        return Err(Error::Manifest(format!(
            "{name} is {} bytes, over the {max} byte ceiling",
            layer.size
        )));
    }
    if layer.annotations.contains_key(ANNOTATION_LAYER_SIZE) {
        let plain = layer_size(layer)?;
        if plain > max {
            return Err(Error::Manifest(format!(
                "{name} decompresses to {plain} bytes, over the {max} byte ceiling"
            )));
        }
    }
    Ok(())
}

impl Client {
    /// PUT a manifest (by tag if the target has one, else by its digest) and
    /// return its digest.
    async fn put_manifest_body(&self, target: &Reference, body: Vec<u8>) -> Result<String> {
        let digest = sha256_digest(&body);
        let tag = target.tag.clone().unwrap_or_else(|| digest.clone());
        let url = format!("{}/manifests/{tag}", self.base(target));
        let scope = format!("repository:{}:pull,push", target.repository);
        let resp = self
            .send(&scope, |h| {
                Ok(h.put(&url)
                    .header(CONTENT_TYPE, MANIFEST_MEDIA_TYPE)
                    .body(body.clone()))
            })
            .await?;
        check(resp, &url).await?;
        Ok(digest)
    }

    /// Push an image: the config blob and the three layers by digest (gzip for
    /// kernel and initramfs, the root filesystem as built), then the manifest.
    /// Nothing is uploaded unless `config` validates.
    ///
    /// Compressed copies go in `scratch` and are removed on every path out.
    ///
    /// # Errors
    /// [`Error::Manifest`] for an invalid config; local I/O, transport, auth
    /// or registry errors.
    pub async fn push_microvm(
        &self,
        target: &Reference,
        files: &MicrovmFiles<'_>,
        config: &MicrovmConfig,
        scratch: &Path,
    ) -> Result<MicrovmPushed> {
        config.validate()?;
        let config_blob = serde_json::to_vec(config).map_err(|e| Error::Manifest(e.to_string()))?;
        let kernel = stage_gzip(files.kernel, LAYER_KERNEL, scratch).await?;
        let initramfs = match files.initramfs {
            Some(path) => Some(stage_gzip(path, LAYER_INITRAMFS, scratch).await?),
            None => None,
        };
        let rootfs = stage_raw(files.rootfs, LAYER_ROOTFS).await?;
        info!(kernel = %kernel.descriptor.digest, rootfs = %rootfs.descriptor.digest, "layers staged");

        let config_descriptor = Descriptor {
            media_type: CONFIG_MEDIA_TYPE.to_string(),
            digest: sha256_digest(&config_blob),
            size: config_blob.len() as u64,
            annotations: std::collections::BTreeMap::new(),
        };
        self.put_blob(
            target,
            &config_descriptor.digest,
            config_descriptor.size,
            || Ok(reqwest::Body::from(config_blob.clone())),
        )
        .await?;
        for staged in std::iter::once(&kernel)
            .chain(initramfs.as_ref())
            .chain(std::iter::once(&rootfs))
        {
            let path = staged.path.clone();
            self.put_blob(
                target,
                &staged.descriptor.digest,
                staged.descriptor.size,
                || {
                    let file = File::open(&path)?;
                    let stream = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
                    Ok(reqwest::Body::wrap_stream(stream))
                },
            )
            .await?;
        }

        let manifest = build_manifest(&MicrovmLayout {
            config: config_descriptor,
            kernel: kernel.descriptor.clone(),
            initramfs: initramfs.as_ref().map(|s| s.descriptor.clone()),
            rootfs: rootfs.descriptor.clone(),
        });
        let body = serde_json::to_vec(&manifest).map_err(|e| Error::Manifest(e.to_string()))?;
        let digest = self.put_manifest_body(target, body).await?;
        Ok(MicrovmPushed {
            reference: target.with_digest(&digest),
        })
    }

    /// GET a blob, capped at `cap` bytes.
    async fn fetch_capped(&self, r: &Reference, digest: &str, cap: u64) -> Result<Vec<u8>> {
        let resp = self.get_blob(r, digest).await?;
        let mut stream = resp.bytes_stream();
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk?);
            if out.len() as u64 > cap {
                return Err(Error::Manifest(format!(
                    "blob {digest} is over {cap} bytes"
                )));
            }
        }
        Ok(out)
    }

    async fn get_blob(&self, r: &Reference, digest: &str) -> Result<reqwest::Response> {
        let url = format!("{}/blobs/{digest}", self.base(r));
        let scope = format!("repository:{}:pull", r.repository);
        let resp = self
            .send(&scope, |h| Ok(h.get(&url).header(ACCEPT, "*/*")))
            .await?;
        check(resp, &url).await
    }

    /// Fetch the config blob, checking its size and digest.
    async fn fetch_config(&self, r: &Reference, descriptor: &Descriptor) -> Result<Vec<u8>> {
        if descriptor.size > CONFIG_MAX_BYTES {
            return Err(Error::Manifest(format!(
                "config is {} bytes, over {CONFIG_MAX_BYTES}",
                descriptor.size
            )));
        }
        let bytes = self
            .fetch_capped(r, &descriptor.digest, descriptor.size)
            .await?;
        let actual = sha256_digest(&bytes);
        if actual != descriptor.digest {
            return Err(Error::DigestMismatch {
                expected: descriptor.digest.clone(),
                actual,
            });
        }
        if bytes.len() as u64 != descriptor.size {
            return Err(Error::Manifest(format!(
                "config is {} bytes, declared {}",
                bytes.len(),
                descriptor.size
            )));
        }
        Ok(bytes)
    }

    /// Stream one layer into `dest`, hashing the bytes as received. A decode
    /// error does not pre-empt the digest check: a corrupted blob is reported
    /// as the digest mismatch it is.
    async fn pull_layer(
        &self,
        r: &Reference,
        name: &str,
        layer: &Descriptor,
        encoding: Encoding,
        dest: &Path,
    ) -> Result<()> {
        let resp = self.get_blob(r, &layer.digest).await?;
        let file = OpenOptions::new().write(true).create_new(true).open(dest)?;
        let mut sink = Sink::open(file, encoding);
        let mut hasher = Sha256::new();
        let mut received: u64 = 0;
        let mut deferred: Option<std::io::Error> = None;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            received += chunk.len() as u64;
            if received > layer.size {
                return Err(Error::Manifest(format!(
                    "{name} is larger than its declared {} bytes",
                    layer.size
                )));
            }
            hasher.update(&chunk);
            if deferred.is_none() {
                deferred = sink.write(&chunk).err();
            }
        }
        let actual = format!("{SHA256_PREFIX}{}", hex(&hasher.finalize()));
        if actual != layer.digest {
            return Err(Error::DigestMismatch {
                expected: layer.digest.clone(),
                actual,
            });
        }
        if let Some(e) = deferred {
            return Err(Error::Manifest(format!("{name}: {e}")));
        }
        sink.finish(name, received, layer.size)?.sync_all()?;
        Ok(())
    }

    /// Pull the image `r` (by digest) into `images_dir/sha256-<manifest hex>`.
    ///
    /// The directory is built in a temporary one beside it and renamed into
    /// place once every layer's digest and size has verified, so nothing
    /// partial is ever visible and a refusal leaves nothing behind. An entry
    /// already in place is left as it is (it was verified when it was put
    /// there); the caller decides whether to pull at all.
    ///
    /// `max_layer_bytes` has no default: every layer, compressed and
    /// decompressed, must fit in it. `host_arch` is the architecture the image
    /// must be built for.
    ///
    /// # Errors
    /// [`Error::Manifest`] for any condition ADR-0142 Decision 6 lists;
    /// [`Error::DigestMismatch`]; transport, auth, registry or local I/O errors.
    pub async fn pull_microvm(
        &self,
        r: &Reference,
        images_dir: &Path,
        host_arch: Architecture,
        max_layer_bytes: u64,
    ) -> Result<MicrovmPulled> {
        let manifest = self.manifest(r).await?;
        let layout = parse_manifest(&manifest)?;
        check_ceiling("kernel", &layout.kernel, max_layer_bytes)?;
        if let Some(initramfs) = &layout.initramfs {
            check_ceiling("initramfs", initramfs, max_layer_bytes)?;
        }
        check_ceiling("rootfs", &layout.rootfs, max_layer_bytes)?;

        let config_blob = self.fetch_config(r, &layout.config).await?;
        let config = MicrovmConfig::parse(&config_blob)?;
        if config.architecture != host_arch {
            return Err(Error::Manifest(format!(
                "image architecture {:?} is not the host's {host_arch:?}",
                config.architecture
            )));
        }

        let manifest_digest = r.digest.as_deref().unwrap_or_default();
        let hex_part = manifest_digest
            .strip_prefix(SHA256_PREFIX)
            .ok_or_else(|| Error::Reference(format!("{r}: pull is by digest only")))?;
        let final_dir = images_dir.join(format!("sha256-{hex_part}"));

        let tmp = tempfile::Builder::new()
            .prefix(PULL_TMP_PREFIX)
            .tempdir_in(images_dir)?;
        let kernel = layout.kernel.clone();
        let kernel_len = layer_size(&kernel)?;
        self.pull_layer(
            r,
            "kernel",
            &kernel,
            Encoding::Gzip(kernel_len),
            &tmp.path().join(KERNEL_FILE),
        )
        .await?;
        if let Some(initramfs) = &layout.initramfs {
            let len = layer_size(initramfs)?;
            self.pull_layer(
                r,
                "initramfs",
                initramfs,
                Encoding::Gzip(len),
                &tmp.path().join(INITRAMFS_FILE),
            )
            .await?;
        }
        self.pull_layer(
            r,
            "rootfs",
            &layout.rootfs,
            Encoding::Raw,
            &tmp.path().join(ROOTFS_FILE),
        )
        .await?;
        std::fs::write(tmp.path().join(BOOT_FILE), &config_blob)?;
        set_modes(tmp.path())?;

        if let Err(e) = std::fs::rename(tmp.path(), &final_dir) {
            // Another pull of the same digest won the race: the entry is
            // content-addressed and was verified before it appeared.
            if !final_dir.is_dir() {
                return Err(e.into());
            }
            debug!(dir = %final_dir.display(), "image already in place");
        }
        // The rename moved the directory; dropping `tmp` has nothing to remove.
        drop(tmp);
        let _ = File::open(images_dir).and_then(|d| d.sync_all());
        Ok(MicrovmPulled {
            dir: final_dir,
            config,
        })
    }
}

/// Directory `0755`, files `0644`: the guest reads them as its own uid.
fn set_modes(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(FILE_MODE))?;
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE))?;
    Ok(())
}

#[cfg(test)]
#[path = "microvm_tests.rs"]
mod microvm_tests;
