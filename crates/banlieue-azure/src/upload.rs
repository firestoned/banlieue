// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Direct upload to a managed disk with Put Page (ADR-0162 Decision 2).
//!
//! A managed disk created with `createOption: Upload` and opened with
//! `beginGetAccess` takes plain page-blob `Put Page` requests through a
//! write SAS. The spike found 52 to 53% of a 2.5 GB image was zeros; an
//! upload disk starts zeroed, so [`upload_pages`] skips every all-zero
//! range and writes the rest a few at a time.
//!
//! The SAS is a bearer credential for the disk's data, so:
//!
//! - it lives in a [`SasUrl`], whose `Debug` and `Display` never print it,
//!   and every transport error is stripped of its URL;
//! - it is only ever sent to the cloud's managed-disk upload host, checked
//!   by [`validate_upload_url`] before the first write;
//! - **no ARM bearer token is ever sent with it**: [`SasPageWriter`] has
//!   its own HTTP client and sets no `Authorization` header.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Method;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::task::JoinSet;

use crate::client::read_capped;
use crate::error::{Error, Result, classify};
use crate::operation::RETRY_AFTER_HEADER;
use crate::retry::parse_retry_after;

/// Page blobs are written in whole 512-byte pages.
pub const PAGE_ALIGNMENT: u64 = 512;
/// The largest range one Put Page may write.
pub const MAX_PAGE_WRITE_BYTES: usize = 4 * 1024 * 1024;
/// Writes in flight at once. The spike measured 2.5 MiB/s on one stream and
/// 3.1 MiB/s on eight, bound by its uplink; eight is enough to fill one.
pub const PARALLEL_PAGE_WRITES: usize = 8;
/// Attempts per range before the upload fails.
pub const MAX_WRITE_ATTEMPTS: u32 = 6;
/// First wait after a retryable failure; doubled each time.
pub const INITIAL_WRITE_BACKOFF: Duration = Duration::from_secs(1);
/// Longest wait between attempts.
pub const MAX_WRITE_BACKOFF: Duration = Duration::from_secs(30);
/// The Blob service version every request is pinned to.
pub const BLOB_API_VERSION: &str = "2023-11-03";

const HEADER_PAGE_WRITE: &str = "x-ms-page-write";
const HEADER_RANGE: &str = "x-ms-range";
const HEADER_VERSION: &str = "x-ms-version";
const PAGE_WRITE_UPDATE: &str = "update";
const COMP_PAGE: &str = "comp=page";
const HTTPS: &str = "https";
const HTTP: &str = "http";
const LOOPBACK_HOSTS: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];
const HTTP_CREATED: u16 = 201;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// One 4 MiB write over a slow uplink: generous.
const WRITE_TIMEOUT: Duration = Duration::from_secs(300);
/// Blob error bodies are small XML documents.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const CODE_INVALID_RANGE: &str = "InvalidPageRange";
const REDACTED: &str = "<redacted SAS>";
const USER_AGENT: &str = concat!("banlieue-azure/", env!("CARGO_PKG_VERSION"));

/// A write SAS URL for an upload disk. Never printed: `Debug` and
/// `Display` show only that it is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct SasUrl(String);

impl SasUrl {
    /// Wraps a SAS URL ARM returned.
    #[must_use]
    pub fn new(url: String) -> Self {
        Self(url)
    }

    /// The URL itself, to send a request to. Never log it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The URL's host, safe to log; `None` if it does not parse.
    #[must_use]
    pub fn host(&self) -> Option<String> {
        reqwest::Url::parse(&self.0)
            .ok()
            .and_then(|u| u.host_str().map(ToString::to_string))
    }
}

impl fmt::Debug for SasUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl fmt::Display for SasUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// Refuses a SAS unless it points at `host_suffix` (the cloud's
/// managed-disk upload domain, such as `blob.storage.azure.net`, from a
/// fixed allowlist, never from configuration): HTTPS on the default port,
/// no userinfo, a host equal to the suffix or ending in `.` + suffix, and a
/// query (the signature). Plain HTTP is accepted only to a loopback host
/// that is itself the suffix: the unit tests' local server.
///
/// # Errors
/// [`Error::UntrustedUploadHost`], naming the host and never the SAS.
pub fn validate_upload_url(sas: &SasUrl, host_suffix: &str) -> Result<()> {
    let refuse = |what: String| Err(Error::UntrustedUploadHost(what));
    let Ok(url) = reqwest::Url::parse(sas.expose()) else {
        return refuse("an unparseable URL".to_string());
    };
    let host = url.host_str().unwrap_or_default().to_string();
    let suffix = host_suffix.trim_matches('.');
    let loopback_test =
        url.scheme() == HTTP && LOOPBACK_HOSTS.contains(&host.as_str()) && host == suffix;
    if url.scheme() != HTTPS && !loopback_test {
        return refuse(format!("{host} over {}", url.scheme()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return refuse(format!("{host} with userinfo"));
    }
    if url.port().is_some() && !loopback_test {
        return refuse(format!("{host} on a non-default port"));
    }
    let host_ok = !suffix.is_empty()
        && (host.eq_ignore_ascii_case(suffix)
            || host
                .to_ascii_lowercase()
                .ends_with(&format!(".{}", suffix.to_ascii_lowercase())));
    if !host_ok {
        return refuse(host);
    }
    if url.query().is_none_or(str::is_empty) {
        return refuse(format!("{host} without a signature"));
    }
    Ok(())
}

/// Checks a page range the way the Blob service does.
///
/// # Errors
/// [`Error::InvalidRequest`] for an empty, unaligned or over-long range.
pub fn check_page_range(offset: u64, len: usize) -> Result<()> {
    let len64 = len as u64;
    if len == 0
        || !offset.is_multiple_of(PAGE_ALIGNMENT)
        || !len64.is_multiple_of(PAGE_ALIGNMENT)
        || len > MAX_PAGE_WRITE_BYTES
    {
        return Err(Error::InvalidRequest {
            code: CODE_INVALID_RANGE.to_string(),
            message: format!(
                "page write at {offset} of {len} bytes must be {PAGE_ALIGNMENT}-aligned and at most {MAX_PAGE_WRITE_BYTES} bytes"
            ),
        });
    }
    Ok(())
}

/// Writes one range of a page blob.
#[async_trait]
pub trait PageWriter: Send + Sync {
    /// `Put Page` of `data` at `offset`.
    ///
    /// # Errors
    /// A retryable [`Error`] (throttled, 5xx, transport) or a refusal.
    async fn put_page(&self, offset: u64, data: Vec<u8>) -> Result<()>;
}

/// Opens a [`PageWriter`] for a SAS.
pub trait PageWriterFactory: Send + Sync {
    /// A writer for `sas`, after [`validate_upload_url`].
    ///
    /// # Errors
    /// [`Error::UntrustedUploadHost`] for a SAS on any other host.
    fn writer(&self, sas: &SasUrl) -> Result<Arc<dyn PageWriter>>;
}

/// The real factory: Put Page over HTTPS to the cloud's upload host.
#[derive(Clone, Debug)]
pub struct HttpPageWriterFactory {
    host_suffix: String,
}

impl HttpPageWriterFactory {
    /// A factory that accepts only SAS URLs under `host_suffix`.
    #[must_use]
    pub fn new(host_suffix: &str) -> Self {
        Self {
            host_suffix: host_suffix.to_string(),
        }
    }
}

impl PageWriterFactory for HttpPageWriterFactory {
    fn writer(&self, sas: &SasUrl) -> Result<Arc<dyn PageWriter>> {
        validate_upload_url(sas, &self.host_suffix)?;
        Ok(Arc::new(SasPageWriter::new(sas.clone())?))
    }
}

/// Put Page through a SAS, with its own HTTP client: no bearer token, no
/// redirects.
pub struct SasPageWriter {
    http: reqwest::Client,
    url: String,
}

impl fmt::Debug for SasPageWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SasPageWriter").finish_non_exhaustive()
    }
}

impl SasPageWriter {
    /// A writer for `sas`. The caller has validated it
    /// ([`validate_upload_url`]).
    ///
    /// # Errors
    /// [`Error::Transport`] if the HTTP client cannot be built.
    pub fn new(sas: SasUrl) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(WRITE_TIMEOUT)
            .user_agent(USER_AGENT)
            // A redirect would carry the SAS to another host.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(Self {
            http,
            url: format!("{}&{COMP_PAGE}", sas.expose()),
        })
    }
}

#[async_trait]
impl PageWriter for SasPageWriter {
    async fn put_page(&self, offset: u64, data: Vec<u8>) -> Result<()> {
        check_page_range(offset, data.len())?;
        let end = offset + data.len() as u64 - 1;
        let response = self
            .http
            .request(Method::PUT, &self.url)
            .header(HEADER_PAGE_WRITE, PAGE_WRITE_UPDATE)
            .header(HEADER_RANGE, format!("bytes={offset}-{end}"))
            .header(HEADER_VERSION, BLOB_API_VERSION)
            .header(reqwest::header::CONTENT_LENGTH, data.len().to_string())
            .body(data)
            .send()
            .await
            // Never the URL: it holds the SAS.
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        if status == HTTP_CREATED {
            return Ok(());
        }
        let retry_after = response
            .headers()
            .get(RETRY_AFTER_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after);
        let body = read_capped(response, MAX_ERROR_BODY_BYTES)
            .await
            .unwrap_or_default();
        Err(classify(status, retry_after, &body))
    }
}

/// How [`upload_pages`] paces itself.
#[derive(Clone, Debug)]
pub struct UploadOptions {
    /// Bytes per range; at most [`MAX_PAGE_WRITE_BYTES`], a multiple of
    /// [`PAGE_ALIGNMENT`].
    pub chunk_bytes: usize,
    /// Writes in flight at once.
    pub parallelism: usize,
    /// Attempts per range.
    pub max_attempts: u32,
    /// First back-off.
    pub initial_backoff: Duration,
    /// Longest back-off.
    pub max_backoff: Duration,
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self {
            chunk_bytes: MAX_PAGE_WRITE_BYTES,
            parallelism: PARALLEL_PAGE_WRITES,
            max_attempts: MAX_WRITE_ATTEMPTS,
            initial_backoff: INITIAL_WRITE_BACKOFF,
            max_backoff: MAX_WRITE_BACKOFF,
        }
    }
}

/// What an upload did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UploadStats {
    /// Bytes read from the source.
    pub bytes_total: u64,
    /// Bytes written.
    pub bytes_written: u64,
    /// Bytes skipped because they were zero.
    pub bytes_skipped: u64,
    /// Ranges written.
    pub ranges_written: u64,
}

async fn write_with_retry(
    writer: Arc<dyn PageWriter>,
    offset: u64,
    data: Vec<u8>,
    options: UploadOptions,
) -> Result<()> {
    let mut backoff = options.initial_backoff;
    let mut attempt = 1;
    loop {
        match writer.put_page(offset, data.clone()).await {
            Ok(()) => return Ok(()),
            Err(e) if e.is_retryable() && attempt < options.max_attempts => {
                let wait = match e {
                    Error::Throttled { retry_after } => retry_after,
                    _ => backoff,
                };
                tokio::time::sleep(wait).await;
                backoff = (backoff * 2).min(options.max_backoff);
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn join_one(tasks: &mut JoinSet<Result<()>>) -> Result<()> {
    match tasks.join_next().await {
        Some(Ok(result)) => result,
        Some(Err(e)) => Err(Error::Transport(format!("page write task: {e}"))),
        None => Ok(()),
    }
}

/// Uploads `total_len` bytes read from `source` as a page blob through
/// `writer`, skipping all-zero ranges, with up to `options.parallelism`
/// writes in flight. Reads sequentially, so at most `parallelism + 1`
/// chunks are in memory at once; re-running it over the same blob is safe
/// (each range is written whole).
///
/// # Errors
/// [`Error::InvalidRequest`] for options that break page alignment;
/// [`Error::UploadSource`] if the source is shorter or longer than
/// `total_len` or unreadable; the first write that fails after its retries.
pub async fn upload_pages<R: AsyncRead + Unpin>(
    mut source: R,
    total_len: u64,
    writer: Arc<dyn PageWriter>,
    options: &UploadOptions,
) -> Result<UploadStats> {
    check_page_range(0, options.chunk_bytes)?;
    if !total_len.is_multiple_of(PAGE_ALIGNMENT) {
        return Err(Error::InvalidRequest {
            code: CODE_INVALID_RANGE.to_string(),
            message: format!("an upload of {total_len} bytes is not whole pages"),
        });
    }
    let parallelism = options.parallelism.max(1);
    let mut stats = UploadStats::default();
    let mut tasks: JoinSet<Result<()>> = JoinSet::new();
    let mut offset = 0u64;
    while offset < total_len {
        let len = usize::try_from(total_len - offset)
            .unwrap_or(usize::MAX)
            .min(options.chunk_bytes);
        let mut buf = vec![0u8; len];
        source.read_exact(&mut buf).await.map_err(|e| {
            Error::UploadSource(format!("reading {len} bytes at offset {offset}: {e}"))
        })?;
        stats.bytes_total += len as u64;
        if buf.iter().all(|b| *b == 0) {
            stats.bytes_skipped += len as u64;
            offset += len as u64;
            continue;
        }
        while tasks.len() >= parallelism {
            join_one(&mut tasks).await?;
        }
        stats.bytes_written += len as u64;
        stats.ranges_written += 1;
        tasks.spawn(write_with_retry(
            writer.clone(),
            offset,
            buf,
            options.clone(),
        ));
        offset += len as u64;
    }
    let mut probe = [0u8; 1];
    let extra = source
        .read(&mut probe)
        .await
        .map_err(|e| Error::UploadSource(e.to_string()))?;
    if extra != 0 {
        return Err(Error::UploadSource(format!(
            "the source is longer than {total_len} bytes"
        )));
    }
    while !tasks.is_empty() {
        join_one(&mut tasks).await?;
    }
    Ok(stats)
}

#[cfg(test)]
#[path = "upload_tests.rs"]
mod upload_tests;
