// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! One request per connection (ADR-0143 Decision 4).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

use super::ca::Ca;
use super::csr::{CsrError, MAX_CSR_BYTES, parse_csr};
use super::issue::{IssueError, Serial, issue};
use super::limiter::{Clock, RateLimiter};

/// How long a connection may take to send its request.
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Outcome counters; the only record the signer keeps of a request.
#[derive(Debug, Default)]
pub struct Counters {
    issued: AtomicU64,
    refused_size: AtomicU64,
    refused_key_type: AtomicU64,
    refused_signature: AtomicU64,
    refused_rate: AtomicU64,
    refused_parse: AtomicU64,
    refused_timeout: AtomicU64,
}

/// A point-in-time copy of [`Counters`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CounterSnapshot {
    /// Certificates issued.
    pub issued: u64,
    /// Refused for size.
    pub refused_size: u64,
    /// Refused for key type.
    pub refused_key_type: u64,
    /// Refused for a bad CSR signature.
    pub refused_signature: u64,
    /// Refused by the rate limit.
    pub refused_rate: u64,
    /// Refused for not parsing.
    pub refused_parse: u64,
    /// Refused because the request was not sent in time.
    pub refused_timeout: u64,
}

impl Counters {
    /// Counts one outcome.
    fn record(&self, outcome: Outcome) {
        let counter = match outcome {
            Outcome::Issued => &self.issued,
            Outcome::RefusedSize => &self.refused_size,
            Outcome::RefusedKeyType => &self.refused_key_type,
            Outcome::RefusedSignature => &self.refused_signature,
            Outcome::RefusedRate => &self.refused_rate,
            Outcome::RefusedParse => &self.refused_parse,
            Outcome::RefusedTimeout => &self.refused_timeout,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// A copy of the current values.
    pub fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            issued: self.issued.load(Ordering::Relaxed),
            refused_size: self.refused_size.load(Ordering::Relaxed),
            refused_key_type: self.refused_key_type.load(Ordering::Relaxed),
            refused_signature: self.refused_signature.load(Ordering::Relaxed),
            refused_rate: self.refused_rate.load(Ordering::Relaxed),
            refused_parse: self.refused_parse.load(Ordering::Relaxed),
            refused_timeout: self.refused_timeout.load(Ordering::Relaxed),
        }
    }
}

/// What happened to one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A certificate was sent.
    Issued,
    /// Too large.
    RefusedSize,
    /// Not EC P-256.
    RefusedKeyType,
    /// CSR signature bad.
    RefusedSignature,
    /// Rate limited.
    RefusedRate,
    /// Did not parse.
    RefusedParse,
    /// Not sent in time.
    RefusedTimeout,
}

/// Why a connection failed other than by refusal.
#[derive(Debug, Error)]
pub enum ServeError {
    /// Issuing failed (host configuration or RNG).
    #[error("issue: {0}")]
    Issue(#[from] IssueError),
    /// Writing the reply failed.
    #[error("write: {0}")]
    Io(#[from] std::io::Error),
}

/// What to do with a connection once its request is judged.
enum Decision {
    /// Send this certificate.
    Reply(String),
    /// Send nothing.
    Refuse(Outcome),
}

/// Closes the write side; a failure to close changes nothing for the caller.
async fn close<S: AsyncWrite + Unpin>(stream: &mut S) {
    let _ = stream.shutdown().await;
}

/// Reads at most [`MAX_CSR_BYTES`] plus one byte, so over-size is detectable
/// without buffering what the peer keeps sending.
async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    (&mut *stream)
        .take(MAX_CSR_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .await?;
    Ok(buf)
}

/// Rate limit, read, parse, issue: the whole judgement, with no I/O on reply.
async fn decide<S, C>(
    stream: &mut S,
    ca: &Ca,
    subject: &str,
    limiter: &RateLimiter<C>,
    validity: Duration,
) -> Result<Decision, ServeError>
where
    S: AsyncRead + Unpin,
    C: Clock,
{
    if !limiter.allow() {
        return Ok(Decision::Refuse(Outcome::RefusedRate));
    }
    let request = match timeout(READ_TIMEOUT, read_request(stream)).await {
        Err(_elapsed) => return Ok(Decision::Refuse(Outcome::RefusedTimeout)),
        // A read that fails is a request that did not arrive intact.
        Ok(Err(_)) => return Ok(Decision::Refuse(Outcome::RefusedParse)),
        Ok(Ok(bytes)) => bytes,
    };
    let key = match parse_csr(&request) {
        Ok(key) => key,
        Err(CsrError::TooLarge) => return Ok(Decision::Refuse(Outcome::RefusedSize)),
        Err(CsrError::Parse) => return Ok(Decision::Refuse(Outcome::RefusedParse)),
        Err(CsrError::KeyType) => return Ok(Decision::Refuse(Outcome::RefusedKeyType)),
        Err(CsrError::Signature) => return Ok(Decision::Refuse(Outcome::RefusedSignature)),
    };
    let pem = issue(
        ca,
        subject,
        &key,
        validity,
        limiter.clock().wall(),
        Serial::random()?,
    )?;
    Ok(Decision::Reply(pem))
}

/// Serves one request on `stream`, then closes it.
///
/// # Errors
/// [`ServeError`] for issuing or write failures; refusals are an [`Outcome`].
pub async fn serve_one<S, C>(
    stream: &mut S,
    ca: &Ca,
    subject: &str,
    limiter: &RateLimiter<C>,
    counters: &Counters,
    validity: Duration,
) -> Result<Outcome, ServeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: Clock,
{
    let decision = decide(stream, ca, subject, limiter, validity).await;
    let outcome = match decision {
        Ok(Decision::Reply(pem)) => {
            let sent = stream.write_all(pem.as_bytes()).await;
            close(stream).await;
            sent?;
            Outcome::Issued
        }
        Ok(Decision::Refuse(outcome)) => {
            close(stream).await;
            outcome
        }
        Err(e) => {
            close(stream).await;
            return Err(e);
        }
    };
    counters.record(outcome);
    Ok(outcome)
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod serve_tests;
