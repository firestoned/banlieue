// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The one manifest shape this client pushes and pulls: an OCI image
//! manifest with an `artifactType`, the empty config, and one layer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::error::{Error, Result};
use crate::reference::SHA256_PREFIX;

/// OCI image manifest media type.
pub const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
/// The OCI empty descriptor's media type, for an artifact with no config.
pub const EMPTY_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";
/// The empty config blob: `{}`.
pub const EMPTY_CONFIG: &[u8] = b"{}";
/// Artifact type of a raw disk image (ADR-0064 Decision 2).
pub const ARTIFACT_TYPE_RAW: &str = "application/vnd.banlieue.disk.raw.v1";
/// Artifact type of an installer ISO.
pub const ARTIFACT_TYPE_ISO: &str = "application/vnd.banlieue.disk.iso.v1";
/// Layer media type: the file, gzip-compressed.
pub const LAYER_MEDIA_TYPE_GZIP: &str = "application/vnd.banlieue.disk.v1+gzip";
/// Layer annotation: the decompressed length in bytes. Part of the layer
/// descriptor, so covered by the manifest digest a host pins; a pull that
/// would decompress to anything else is refused.
pub const ANNOTATION_UNCOMPRESSED_SIZE: &str = "io.banlieue.disk.size";
/// OCI schema version.
const SCHEMA_VERSION: u32 = 2;

/// A content descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    /// Media type.
    pub media_type: String,
    /// `sha256:<hex>`.
    pub digest: String,
    /// Bytes.
    pub size: u64,
    /// Annotations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

/// An OCI image manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Always 2.
    pub schema_version: u32,
    /// [`MANIFEST_MEDIA_TYPE`].
    pub media_type: String,
    /// What the artifact is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,
    /// The empty config.
    pub config: Descriptor,
    /// Exactly one, for artifacts this client pushes.
    pub layers: Vec<Descriptor>,
    /// Manifest annotations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

impl Manifest {
    /// A single-layer artifact manifest.
    #[must_use]
    pub fn artifact(
        artifact_type: &str,
        layer: Descriptor,
        annotations: BTreeMap<String, String>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            media_type: MANIFEST_MEDIA_TYPE.to_string(),
            artifact_type: Some(artifact_type.to_string()),
            config: Descriptor {
                media_type: EMPTY_MEDIA_TYPE.to_string(),
                digest: sha256_digest(EMPTY_CONFIG),
                size: EMPTY_CONFIG.len() as u64,
                annotations: BTreeMap::new(),
            },
            layers: vec![layer],
            annotations,
        }
    }

    /// The one layer of an artifact this client understands.
    ///
    /// # Errors
    /// [`Error::Manifest`] unless there is exactly one gzip layer.
    pub fn single_layer(&self) -> Result<&Descriptor> {
        match self.layers.as_slice() {
            [layer] if layer.media_type == LAYER_MEDIA_TYPE_GZIP => Ok(layer),
            [layer] => Err(Error::Manifest(format!(
                "layer media type {} is not {LAYER_MEDIA_TYPE_GZIP}",
                layer.media_type
            ))),
            layers => Err(Error::Manifest(format!(
                "expected one layer, found {}",
                layers.len()
            ))),
        }
    }
}

/// `sha256:<hex>` of `bytes`.
#[must_use]
pub fn sha256_digest(bytes: &[u8]) -> String {
    format!("{SHA256_PREFIX}{}", hex(&Sha256::digest(bytes)))
}

/// Lowercase hex.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// The decompressed length `layer` declares.
///
/// # Errors
/// [`Error::Manifest`] when the annotation is absent or not a number: an
/// unbounded decompression is not accepted.
pub fn uncompressed_size(layer: &Descriptor) -> Result<u64> {
    layer
        .annotations
        .get(ANNOTATION_UNCOMPRESSED_SIZE)
        .ok_or_else(|| Error::Manifest(format!("layer has no {ANNOTATION_UNCOMPRESSED_SIZE}")))?
        .parse()
        .map_err(|_| Error::Manifest(format!("{ANNOTATION_UNCOMPRESSED_SIZE} is not a size")))
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod manifest_tests;
