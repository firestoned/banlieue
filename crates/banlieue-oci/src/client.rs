// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The OCI distribution calls this crate needs, over `reqwest`.
//!
//! - **push**: gzip a file to a temporary copy while hashing it, upload it
//!   and the empty config as blobs (skipping either if the registry already
//!   has it), then PUT the manifest.
//! - **pull**: fetch a manifest *by digest* and check it hashes to that
//!   digest, then stream its one layer, hashing the compressed bytes and
//!   gunzipping them into a sparse temporary file. Only when the layer's
//!   digest matches is the file renamed into place; on a mismatch nothing
//!   is left behind (fail closed, SEC-004).
//!
//! Authentication follows the registry: anonymous first, then whatever a
//! `401` challenge asks for — HTTP Basic, or a Bearer token from the
//! challenge's realm, cached per scope.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine as _;
use flate2::Compression;
use flate2::write::{GzDecoder, GzEncoder};
use futures::StreamExt;
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, LOCATION, WWW_AUTHENTICATE,
};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tracing::{debug, info};

use crate::auth::{Challenge, Credentials, parse_challenge};
use crate::error::{Error, Result};
use crate::manifest::{
    ANNOTATION_UNCOMPRESSED_SIZE, Descriptor, EMPTY_CONFIG, LAYER_MEDIA_TYPE_GZIP,
    MANIFEST_MEDIA_TYPE, Manifest, hex, sha256_digest, uncompressed_size,
};
use crate::reference::{Reference, SHA256_PREFIX, require_digest};
use crate::sparse::SparseWriter;

/// How much of an error body to keep for the message.
const ERROR_BODY_MAX: usize = 512;
/// Read buffer for hashing and compressing.
const IO_BUF: usize = 1024 * 1024;
/// Manifests are small; anything bigger is not ours.
const MANIFEST_MAX: usize = 4 * 1024 * 1024;
/// Suffix of in-progress files.
const PARTIAL_SUFFIX: &str = ".partial";

/// What a push produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pushed {
    /// The artifact, addressed by its manifest digest.
    pub reference: Reference,
    /// The layer's digest (compressed bytes).
    pub layer_digest: String,
    /// The layer's size in bytes (compressed).
    pub layer_size: u64,
}

/// What a pull produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pulled {
    /// Where the file was written.
    pub path: PathBuf,
    /// Its decompressed length.
    pub len: u64,
    /// The manifest's annotations.
    pub annotations: BTreeMap<String, String>,
}

/// A registry client.
pub struct Client {
    http: reqwest::Client,
    credentials: Credentials,
    /// `http://` instead of `https://` — only for a registry on this host
    /// or a test; never the default.
    plain_http: bool,
    /// Bearer tokens by scope.
    tokens: Mutex<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
}

impl Client {
    /// A client using `http` for transport.
    #[must_use]
    pub fn new(http: reqwest::Client, credentials: Credentials, plain_http: bool) -> Self {
        Self {
            http,
            credentials,
            plain_http,
            tokens: Mutex::new(HashMap::new()),
        }
    }

    fn base(&self, r: &Reference) -> String {
        let scheme = if self.plain_http { "http" } else { "https" };
        format!("{scheme}://{}/v2/{}", r.registry, r.repository)
    }

    fn origin(&self, r: &Reference) -> String {
        let scheme = if self.plain_http { "http" } else { "https" };
        format!("{scheme}://{}", r.registry)
    }

    fn cached_token(&self, scope: &str) -> Option<String> {
        self.tokens.lock().ok()?.get(scope).cloned()
    }

    fn basic(&self) -> Option<String> {
        let user = self.credentials.username.as_deref().unwrap_or_default();
        let pass = self.credentials.password.as_deref()?;
        Some(format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
        ))
    }

    /// Answer a challenge: the Authorization header to retry with.
    async fn authorize(&self, challenge: Challenge, scope: &str) -> Result<String> {
        match challenge {
            Challenge::Basic => self.basic().ok_or_else(|| {
                Error::Auth("registry wants Basic auth; no credentials configured".into())
            }),
            Challenge::Bearer {
                realm,
                service,
                scope: challenged,
            } => {
                let scope = challenged.unwrap_or_else(|| scope.to_string());
                let mut url = format!(
                    "{realm}{}scope={}",
                    if realm.contains('?') { '&' } else { '?' },
                    percent_encode(&scope)
                );
                if let Some(svc) = &service {
                    url.push_str(&format!("&service={}", percent_encode(svc)));
                }
                let mut req = self.http.get(&url);
                if let Some(b) = self.basic() {
                    req = req.header(AUTHORIZATION, b);
                }
                let resp = req.send().await?;
                let resp = check(resp, &realm).await?;
                let t: TokenResponse = serde_json::from_slice(&resp.bytes().await?)
                    .map_err(|e| Error::Auth(format!("token response: {e}")))?;
                let token = t
                    .token
                    .or(t.access_token)
                    .ok_or_else(|| Error::Auth(format!("{realm} returned no token")))?;
                let header = format!("Bearer {token}");
                if let Ok(mut cache) = self.tokens.lock() {
                    cache.insert(scope, header.clone());
                }
                Ok(header)
            }
        }
    }

    /// Send a request built by `build`, answering one auth challenge.
    /// `build` is called again for the retry, so a streamed body is
    /// re-created rather than replayed.
    async fn send<F>(&self, scope: &str, build: F) -> Result<Response>
    where
        F: Fn(&reqwest::Client) -> Result<RequestBuilder>,
    {
        let mut req = build(&self.http)?;
        if let Some(t) = self.cached_token(scope) {
            req = req.header(AUTHORIZATION, t);
        }
        let resp = req.send().await?;
        if resp.status() != StatusCode::UNAUTHORIZED {
            return Ok(resp);
        }
        let challenge = resp
            .headers()
            .get(WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::Auth("401 without WWW-Authenticate".into()))
            .and_then(parse_challenge)?;
        let header = self.authorize(challenge, scope).await?;
        Ok(build(&self.http)?
            .header(AUTHORIZATION, header)
            .send()
            .await?)
    }

    async fn blob_exists(&self, r: &Reference, digest: &str, scope: &str) -> Result<bool> {
        let url = format!("{}/blobs/{digest}", self.base(r));
        let resp = self.send(scope, |h| Ok(h.head(&url))).await?;
        Ok(resp.status().is_success())
    }

    /// Upload one blob (`len` bytes from `open`) unless the registry has it.
    async fn put_blob<O>(&self, r: &Reference, digest: &str, len: u64, open: O) -> Result<()>
    where
        O: Fn() -> Result<reqwest::Body>,
    {
        let scope = format!("repository:{}:pull,push", r.repository);
        if self.blob_exists(r, digest, &scope).await? {
            debug!(%digest, "blob already present");
            return Ok(());
        }
        let start_url = format!("{}/blobs/uploads/", self.base(r));
        let started = self
            .send(&scope, |h| Ok(h.post(&start_url).header(CONTENT_LENGTH, 0)))
            .await?;
        let started = check(started, &start_url).await?;
        let location = started
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::Manifest("upload start returned no Location".into()))?
            .to_string();
        let mut url = if location.starts_with("http") {
            location
        } else {
            format!("{}{location}", self.origin(r))
        };
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&format!("digest={digest}"));
        let resp = self
            .send(&scope, |h| {
                Ok(h.request(Method::PUT, &url)
                    .header(CONTENT_TYPE, "application/octet-stream")
                    .header(CONTENT_LENGTH, len)
                    .body(open()?))
            })
            .await?;
        check(resp, &url).await?;
        Ok(())
    }

    /// Push `path` as a single-layer OCI artifact to `target` (a tag or a
    /// bare repository). Returns the artifact addressed by digest.
    ///
    /// # Errors
    /// Local I/O, transport, auth or registry errors.
    pub async fn push_file(
        &self,
        target: &Reference,
        path: &Path,
        scratch: &Path,
        artifact_type: &str,
        annotations: BTreeMap<String, String>,
    ) -> Result<Pushed> {
        // Compress to a temporary copy, hashing as it is written. The copy
        // is what gets uploaded, so the digest describes exactly those bytes.
        // `tmp` is removed when it drops, on every path out of here.
        let source_len = std::fs::metadata(path)?.len();
        let src = path.to_path_buf();
        // The compressed copy goes in `scratch`, which the caller names: in the
        // push Job the source is on a read-only PVC and `scratch` is its
        // emptyDir. Never an implicit, possibly shared, temp directory.
        let tmp = compressed_copy_in(scratch, path)?;
        let out = tmp.reopen()?;
        let (layer_digest, layer_size) =
            tokio::task::spawn_blocking(move || gzip_and_hash(&src, out))
                .await
                .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))??;
        info!(%layer_digest, layer_size, "layer compressed");

        let result: Result<Pushed> = async {
            let config_digest = sha256_digest(EMPTY_CONFIG);
            self.put_blob(target, &config_digest, EMPTY_CONFIG.len() as u64, || {
                Ok(reqwest::Body::from(EMPTY_CONFIG))
            })
            .await?;
            let tmp_for_body = tmp.path().to_path_buf();
            self.put_blob(target, &layer_digest, layer_size, || {
                let file = std::fs::File::open(&tmp_for_body)?;
                let stream = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
                Ok(reqwest::Body::wrap_stream(stream))
            })
            .await?;

            let manifest = Manifest::artifact(
                artifact_type,
                Descriptor {
                    media_type: LAYER_MEDIA_TYPE_GZIP.to_string(),
                    digest: layer_digest.clone(),
                    size: layer_size,
                    annotations: BTreeMap::from([(
                        ANNOTATION_UNCOMPRESSED_SIZE.to_string(),
                        source_len.to_string(),
                    )]),
                },
                annotations,
            );
            let body = serde_json::to_vec(&manifest).map_err(|e| Error::Manifest(e.to_string()))?;
            let manifest_digest = sha256_digest(&body);
            let tag = target
                .tag
                .clone()
                .unwrap_or_else(|| manifest_digest.clone());
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
            Ok(Pushed {
                reference: target.with_digest(&manifest_digest),
                layer_digest: layer_digest.clone(),
                layer_size,
            })
        }
        .await;
        drop(tmp);
        result
    }

    /// Fetch and verify the manifest `r` (which must carry a digest).
    ///
    /// # Errors
    /// [`Error::DigestMismatch`] if it does not hash to its digest.
    pub async fn manifest(&self, r: &Reference) -> Result<Manifest> {
        let digest = r
            .digest
            .as_deref()
            .ok_or_else(|| Error::Reference(format!("{r}: pull is by digest only")))?;
        require_digest(digest)?;
        let url = format!("{}/manifests/{digest}", self.base(r));
        let scope = format!("repository:{}:pull", r.repository);
        let resp = self
            .send(&scope, |h| {
                Ok(h.get(&url).header(ACCEPT, MANIFEST_MEDIA_TYPE))
            })
            .await?;
        let resp = check(resp, &url).await?;
        let bytes = resp.bytes().await?;
        if bytes.len() > MANIFEST_MAX {
            return Err(Error::Manifest(format!(
                "{} bytes is too large",
                bytes.len()
            )));
        }
        let actual = sha256_digest(&bytes);
        if actual != digest {
            return Err(Error::DigestMismatch {
                expected: digest.to_string(),
                actual,
            });
        }
        serde_json::from_slice(&bytes).map_err(|e| Error::Manifest(e.to_string()))
    }

    /// Pull the artifact `r` (by digest) into `dest`, sparse.
    ///
    /// `dest` must not exist. Nothing is left at `dest` unless the layer
    /// verified.
    ///
    /// # Errors
    /// Transport, auth, registry, digest or local I/O errors.
    pub async fn pull_file(&self, r: &Reference, dest: &Path) -> Result<Pulled> {
        let manifest = self.manifest(r).await?;
        let layer = manifest.single_layer()?.clone();
        require_digest(&layer.digest)?;
        // Covered by the manifest digest: the decompressed stream may not
        // be longer, and must not end shorter.
        let expected_len = uncompressed_size(&layer)?;
        let url = format!("{}/blobs/{}", self.base(r), layer.digest);
        let scope = format!("repository:{}:pull", r.repository);
        let resp = self.send(&scope, |h| Ok(h.get(&url))).await?;
        let resp = check(resp, &url).await?;

        let partial = with_suffix(dest, PARTIAL_SUFFIX);
        let _ = std::fs::remove_file(&partial);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)?;
        let outcome: Result<u64> = async {
            let mut hasher = Sha256::new();
            let mut received: u64 = 0;
            let mut decoder = GzDecoder::new(SparseWriter::with_limit(file, expected_len));
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                received += chunk.len() as u64;
                if received > layer.size {
                    return Err(Error::Manifest(format!(
                        "layer is larger than its declared {} bytes",
                        layer.size
                    )));
                }
                hasher.update(&chunk);
                decoder.write_all(&chunk)?;
            }
            let actual = format!("{SHA256_PREFIX}{}", hex(&hasher.finalize()));
            if actual != layer.digest {
                return Err(Error::DigestMismatch {
                    expected: layer.digest.clone(),
                    actual,
                });
            }
            let (file, len) = decoder.finish()?.finish()?;
            if len != expected_len {
                return Err(Error::Manifest(format!(
                    "layer decompressed to {len} bytes, declared {expected_len}"
                )));
            }
            file.sync_all()?;
            Ok(len)
        }
        .await;
        match outcome {
            Ok(len) => {
                std::fs::rename(&partial, dest)?;
                Ok(Pulled {
                    path: dest.to_path_buf(),
                    len,
                    annotations: manifest.annotations,
                })
            }
            Err(e) => {
                let _ = std::fs::remove_file(&partial);
                Err(e)
            }
        }
    }
}

/// Percent-encode a query value: everything but RFC 3986 unreserved bytes.
fn percent_encode(v: &str) -> String {
    use std::fmt::Write as _;
    v.bytes()
        .fold(String::with_capacity(v.len()), |mut out, b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                out.push(char::from(b));
            } else {
                let _ = write!(out, "%{b:02X}");
            }
            out
        })
}

/// Fail on a non-success status, keeping the start of the body.
async fn check(resp: Response, url: &str) -> Result<Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    Err(Error::Registry {
        status,
        url: url.to_string(),
        body: body.chars().take(ERROR_BODY_MAX).collect(),
    })
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    s.into()
}

/// A new, empty file in `dir` for the compressed copy of `p`.
///
/// The name is random (so concurrent pushes of one file cannot collide and
/// nothing can pre-create it), the file is created exclusively with mode
/// 0600, and it is removed when the handle drops — on every exit path,
/// including a failed compression.
///
/// # Errors
/// The OS error from creating the file.
fn compressed_copy_in(dir: &Path, p: &Path) -> Result<tempfile::NamedTempFile> {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "artifact".into());
    Ok(tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(".gz")
        .tempfile_in(dir)?)
}

/// Gzip `src` into `out`, returning the digest and size of what was written.
fn gzip_and_hash(src: &Path, out: File) -> Result<(String, u64)> {
    struct Hashing<W: Write> {
        inner: W,
        hasher: Sha256,
        len: u64,
    }
    impl<W: Write> Write for Hashing<W> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let n = self.inner.write(buf)?;
            self.hasher.update(&buf[..n]);
            self.len += n as u64;
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }
    let hashing = Hashing {
        inner: std::io::BufWriter::with_capacity(IO_BUF, out),
        hasher: Sha256::new(),
        len: 0,
    };
    let mut encoder = GzEncoder::new(hashing, Compression::default());
    let mut reader = BufReader::with_capacity(IO_BUF, File::open(src)?);
    let mut buf = vec![0u8; IO_BUF];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        encoder.write_all(&buf[..n])?;
    }
    let mut hashing = encoder.finish()?;
    hashing.flush()?;
    Ok((
        format!("{SHA256_PREFIX}{}", hex(&hashing.hasher.finalize())),
        hashing.len,
    ))
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
