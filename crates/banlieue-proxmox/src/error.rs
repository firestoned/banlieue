// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Errors from talking to Proxmox VE.

use std::time::Duration;

/// HTTP status Proxmox uses for an unknown route or object.
const HTTP_NOT_FOUND: u16 = 404;
/// HTTP status Proxmox uses for most failures, including a missing VM config.
const HTTP_INTERNAL_SERVER_ERROR: u16 = 500;
/// HTTP status for a bad or expired credential.
const HTTP_UNAUTHORIZED: u16 = 401;
/// HTTP status for a token that authenticates but lacks the privilege.
const HTTP_FORBIDDEN: u16 = 403;

/// Why a call to Proxmox failed.
///
/// Typed because callers act differently on each: a transport error is
/// retryable, an unauthorized error needs an operator, a failed task means
/// Proxmox accepted the work and then could not do it.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Connecting or exchanging HTTP failed (DNS, TCP, TLS, reset).
    #[error("Proxmox transport: {0}")]
    Transport(String),

    /// The call did not finish within its timeout.
    #[error("Proxmox did not answer within {0:?}")]
    Timeout(Duration),

    /// Proxmox answered with an error status. `message` is its reason
    /// phrase (plus any per-parameter validation errors), not just the code.
    #[error("Proxmox returned {status}: {message}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Proxmox's own message.
        message: String,
    },

    /// A response body exceeded the client's cap: a hostile or broken
    /// endpoint must not be able to exhaust the provider's memory.
    #[error("Proxmox response exceeded the {limit}-byte limit")]
    ResponseTooLarge {
        /// The cap that was exceeded.
        limit: usize,
    },

    /// A response did not decode as the expected type.
    #[error("Proxmox response did not decode: {0}")]
    Decode(String),

    /// The API token is malformed (ADR-0074 Decision 3).
    #[error("invalid API token: {0}")]
    InvalidToken(String),

    /// A task id is not a UPID.
    #[error("invalid UPID: {0}")]
    InvalidUpid(String),

    /// The client could not be configured (bad endpoint, bad CA bundle).
    #[error("invalid Proxmox client configuration: {0}")]
    Config(String),

    /// A task ran and stopped with an exit status other than `OK`.
    #[error("Proxmox task {upid} failed: {exitstatus}")]
    TaskFailed {
        /// The task.
        upid: String,
        /// Its `exitstatus`.
        exitstatus: String,
    },

    /// A task was still running when the caller's deadline passed.
    #[error("Proxmox task {upid} still running after {waited:?}")]
    TaskTimeout {
        /// The task.
        upid: String,
        /// How long we waited.
        waited: Duration,
    },
}

impl Error {
    /// Whether Proxmox said the object does not exist: a 404, or the 500 it
    /// uses for a missing VM ("Configuration file ... does not exist").
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        match self {
            Self::Api { status, .. } if *status == HTTP_NOT_FOUND => true,
            Self::Api { status, message } if *status == HTTP_INTERNAL_SERVER_ERROR => {
                message.contains("does not exist")
            }
            _ => false,
        }
    }

    /// Whether the credential was refused or lacks the privilege (401/403).
    #[must_use]
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Api { status, .. } if *status == HTTP_UNAUTHORIZED || *status == HTTP_FORBIDDEN)
    }
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
