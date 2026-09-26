// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Errors from talking to a Cloud Hypervisor VMM.

use std::time::Duration;

/// HTTP status the VMM uses when the thing asked about does not exist, for
/// example `vm.info` before `vm.create`.
const HTTP_NOT_FOUND: u16 = 404;

/// Why a call to a VMM failed.
///
/// Typed, because the caller acts differently on each: a transport error
/// means the VMM process is gone or not up yet, an API error means it
/// answered and refused, and a socket error means something other than our
/// VMM may be listening.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Connecting to or talking over the Unix socket failed.
    #[error("VMM socket I/O: {0}")]
    Io(#[from] std::io::Error),

    /// The HTTP exchange itself failed.
    #[error("VMM HTTP: {0}")]
    Http(String),

    /// The VMM did not answer within the call's timeout.
    #[error("VMM did not answer within {0:?}")]
    Timeout(Duration),

    /// The VMM answered with an error status. `messages` is its error
    /// chain, outermost first, as it reports it.
    #[error("VMM returned {status}: {}", messages.join(": "))]
    Api {
        /// HTTP status code.
        status: u16,
        /// The VMM's messages, outermost first.
        messages: Vec<String>,
    },

    /// A response body did not decode as the expected type.
    #[error("VMM response did not decode: {0}")]
    Decode(#[from] serde_json::Error),

    /// The VMM is older than the release this client is written against
    /// (ADR-0061 Decision 5), or reported a version that does not parse.
    #[error("VMM version {found:?} is unsupported; need {minimum} or newer")]
    VersionUnsupported {
        /// What the VMM reported.
        found: String,
        /// The pinned minimum, as `major.minor`.
        minimum: String,
    },

    /// The API socket is not what the provider created: wrong type, owner,
    /// group or mode (ADR-0061 Decision 6).
    #[error("VMM socket refused: {0}")]
    Socket(String),
}

impl Error {
    /// Whether the VMM said the object does not exist (HTTP 404), which for
    /// `vm.info` means "no VM created yet" rather than a failure.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Api { status, .. } if *status == HTTP_NOT_FOUND)
    }
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
