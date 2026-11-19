// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The request loop on a Unix socket (ADR-0143 Decision 4).
//!
//! One connection is one request: read at most [`MAX_CSR_BYTES`] + 1 bytes
//! under a deadline, answer one PEM certificate and close; a refusal gets no
//! reply, only a counter increment. Nothing here logs a CSR or a certificate.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::signer::{MAX_CSR_BYTES, SignError, Signer};

/// A request is one short write; a connection that dawdles is dropped.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// A guest asks a handful of times per certificate lifetime.
const DEFAULT_MAX_CONCURRENT: usize = 8;
/// Pause after a failed `accept` so a persistent error cannot spin the loop.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);

/// Counters of what the signer did, readable while it runs.
#[derive(Debug, Default)]
pub struct Outcomes {
    issued: AtomicU64,
    too_large: AtomicU64,
    bad_key_type: AtomicU64,
    bad_signature: AtomicU64,
    rate_limited: AtomicU64,
    malformed: AtomicU64,
    internal: AtomicU64,
}

/// A snapshot of [`Outcomes`]: numbers only, no request content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutcomeCounts {
    /// Certificates issued.
    pub issued: u64,
    /// Refused: over the size cap.
    pub too_large: u64,
    /// Refused: not an EC P-256 key.
    pub bad_key_type: u64,
    /// Refused: the CSR's own signature did not verify.
    pub bad_signature: u64,
    /// Refused: rate limit (also connections dropped for want of a free slot).
    pub rate_limited: u64,
    /// Refused: not a CSR, empty, or the client timed out.
    pub malformed: u64,
    /// Failed for a reason that is not the request's (invalid subject, issuing error).
    pub internal: u64,
}

impl Outcomes {
    /// New zeroed counters.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the counters.
    #[must_use]
    pub fn snapshot(&self) -> OutcomeCounts {
        let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
        OutcomeCounts {
            issued: get(&self.issued),
            too_large: get(&self.too_large),
            bad_key_type: get(&self.bad_key_type),
            bad_signature: get(&self.bad_signature),
            rate_limited: get(&self.rate_limited),
            malformed: get(&self.malformed),
            internal: get(&self.internal),
        }
    }

    fn record(&self, result: &Result<String, SignError>) {
        let counter = match result {
            Ok(_) => &self.issued,
            Err(SignError::TooLarge) => &self.too_large,
            Err(SignError::BadKeyType) => &self.bad_key_type,
            Err(SignError::BadSignature) => &self.bad_signature,
            Err(SignError::RateLimited) => &self.rate_limited,
            Err(SignError::Malformed) => &self.malformed,
            Err(SignError::InvalidSubject | SignError::Internal) => &self.internal,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// Tunables for [`serve`].
#[derive(Clone, Copy, Debug)]
pub struct ServeConfig {
    /// Per-connection read deadline.
    pub read_timeout: Duration,
    /// Connections served at once; further ones are closed unanswered.
    pub max_concurrent: usize,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            read_timeout: DEFAULT_READ_TIMEOUT,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }
}

/// Serve certificate requests on `listener` until `cancel` fires.
///
/// Every certificate is issued for `subject`; the peer of a per-guest socket
/// is that guest, so the subject is never taken from the request. The caller
/// creates the socket (and hands it to the guest's uid) and removes it after
/// this returns. Returns when `cancel` is cancelled; requests already being
/// served finish.
pub async fn serve(
    listener: UnixListener,
    signer: Arc<Signer>,
    subject: String,
    outcomes: Arc<Outcomes>,
    config: ServeConfig,
    cancel: CancellationToken,
) {
    let slots = Arc::new(Semaphore::new(config.max_concurrent.max(1)));
    let subject: Arc<str> = subject.into();
    loop {
        let accepted = tokio::select! {
            () = cancel.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(error) => {
                warn!(kind = ?error.kind(), "attestation signer: accept failed");
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(ACCEPT_ERROR_BACKOFF) => {}
                }
                continue;
            }
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            outcomes.rate_limited.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let (signer, subject, outcomes) = (signer.clone(), subject.clone(), outcomes.clone());
        tokio::spawn(async move {
            handle(stream, &signer, &subject, &outcomes, config.read_timeout).await;
            drop(permit);
        });
    }
}

/// Serve one connection: read, sign, answer or just close.
async fn handle(
    mut stream: UnixStream,
    signer: &Signer,
    subject: &str,
    outcomes: &Outcomes,
    read_timeout: Duration,
) {
    let mut request = Vec::new();
    // One byte past the cap is enough to know it is too large; never read more.
    let limit = u64::try_from(MAX_CSR_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let read = timeout(
        read_timeout,
        (&mut stream).take(limit).read_to_end(&mut request),
    )
    .await;
    if !matches!(read, Ok(Ok(_))) {
        outcomes.malformed.fetch_add(1, Ordering::Relaxed);
        debug!("attestation signer: request not received");
        return;
    }
    let result = signer.sign(&request, subject, SystemTime::now());
    outcomes.record(&result);
    match result {
        Ok(certificate) => {
            let answered = timeout(read_timeout, async {
                stream.write_all(certificate.as_bytes()).await?;
                stream.shutdown().await
            })
            .await;
            if !matches!(answered, Ok(Ok(()))) {
                debug!("attestation signer: reply not delivered");
            }
        }
        Err(refusal) => debug!(%refusal, "attestation signer: request refused"),
    }
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod serve_tests;
