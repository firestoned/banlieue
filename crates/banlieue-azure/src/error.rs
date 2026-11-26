// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Typed ARM errors (ADR-0164 Decision 2).
//!
//! ARM reports failures as `{"error":{"code":"...","message":"..."}}` with an
//! HTTP status. The provider needs to tell apart: throttled (wait), size not
//! offered or no capacity in this zone (try another zone), quota exhausted
//! (unschedulable), not found (already gone), and invalid (a bug or bad spec).
//! [`classify`] makes that decision once.

use std::time::Duration;

use serde::Deserialize;

use crate::retry::DEFAULT_RETRY_AFTER;

/// Error messages are cut to this many characters so a large body cannot
/// flood logs or a condition message.
pub const MAX_MESSAGE_LEN: usize = 512;

const HTTP_BAD_REQUEST: u16 = 400;
const HTTP_UNAUTHORIZED: u16 = 401;
const HTTP_FORBIDDEN: u16 = 403;
const HTTP_NOT_FOUND: u16 = 404;
const HTTP_CONFLICT: u16 = 409;
const HTTP_TOO_MANY_REQUESTS: u16 = 429;
const HTTP_SERVER_ERROR_MIN: u16 = 500;

/// Codes meaning the size is not offered, or restricted for this
/// subscription, in the requested place.
const SKU_NOT_AVAILABLE_CODES: [&str; 2] = ["SkuNotAvailable", "NotAvailableForSubscription"];
/// Codes meaning the zone has no capacity for the size right now.
const ALLOCATION_FAILED_CODES: [&str; 4] = [
    "ZonalAllocationFailed",
    "AllocationFailed",
    "OverconstrainedAllocationRequest",
    "OverconstrainedZonalAllocationRequest",
];
const QUOTA_EXCEEDED_CODE: &str = "QuotaExceeded";

/// Everything that can go wrong talking to Azure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 429: wait and retry.
    #[error("throttled by Azure; retry after {retry_after:?}")]
    Throttled {
        /// How long to wait.
        retry_after: Duration,
    },
    /// 404.
    #[error("not found ({code}): {message}")]
    NotFound {
        /// ARM error code.
        code: String,
        /// ARM message.
        message: String,
    },
    /// The size is not offered, or restricted for this subscription, here.
    #[error("size not available here ({code}): {message}")]
    SkuNotAvailable {
        /// ARM error code.
        code: String,
        /// ARM message.
        message: String,
    },
    /// The zone has no capacity for the size right now.
    #[error("allocation failed ({code}): {message}")]
    AllocationFailed {
        /// ARM error code.
        code: String,
        /// ARM message.
        message: String,
    },
    /// The subscription's quota would be exceeded.
    #[error("quota exceeded ({code}): {message}")]
    QuotaExceeded {
        /// ARM error code.
        code: String,
        /// ARM message.
        message: String,
    },
    /// 409 with any other code.
    #[error("conflict ({code}): {message}")]
    Conflict {
        /// ARM error code.
        code: String,
        /// ARM message.
        message: String,
    },
    /// 401: the access token was refused.
    #[error("unauthorized ({code}): {message}")]
    Unauthorized {
        /// ARM error code.
        code: String,
        /// ARM message.
        message: String,
    },
    /// 403: the identity lacks the RBAC action.
    #[error("forbidden ({code}): {message}")]
    Forbidden {
        /// ARM error code.
        code: String,
        /// ARM message.
        message: String,
    },
    /// 400 and any other 4xx.
    #[error("invalid request ({code}): {message}")]
    InvalidRequest {
        /// ARM error code; empty when the body had none.
        code: String,
        /// ARM message.
        message: String,
    },
    /// 5xx.
    #[error("Azure returned {status} ({code}): {message}")]
    Server {
        /// HTTP status.
        status: u16,
        /// ARM error code.
        code: String,
        /// ARM message.
        message: String,
    },
    /// A long-running operation ended `Failed`.
    #[error("operation failed ({code}): {message}")]
    OperationFailed {
        /// ARM error code.
        code: String,
        /// ARM message.
        message: String,
    },
    /// Entra ID refused the token exchange.
    #[error("token exchange refused ({code}): {message}")]
    TokenExchange {
        /// `AADSTSnnnnnn` when Entra ID gave one, else its `error` field.
        code: String,
        /// Entra ID's description.
        message: String,
    },
    /// A tag Azure would refuse.
    #[error("invalid tag: {0}")]
    InvalidTag(String),
    /// A response that is not what ARM documents.
    #[error("unexpected response: {0}")]
    Decode(String),
    /// A URL ARM returned that banlieue must not follow.
    #[error("refusing to follow {0}: not the configured Resource Manager endpoint")]
    UntrustedUrl(String),
    /// A disk upload SAS whose host is not the cloud's managed-disk upload
    /// host. Carries the host only, never the SAS.
    #[error("refusing to upload to {0}: not the cloud's managed-disk upload host")]
    UntrustedUploadHost(String),
    /// The local source of an upload could not be read, or was not the
    /// length it was declared to be.
    #[error("upload source: {0}")]
    UploadSource(String),
    /// Network or TLS failure.
    #[error("transport: {0}")]
    Transport(String),
}

/// Shorthand.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

/// Cuts `message` to [`MAX_MESSAGE_LEN`] characters.
pub(crate) fn truncate(message: &str) -> String {
    message.chars().take(MAX_MESSAGE_LEN).collect()
}

/// Reads ARM's `{"error":{"code","message"}}`; empty strings when absent or
/// unparseable.
pub(crate) fn error_code_and_message(body: &[u8]) -> (String, String) {
    serde_json::from_slice::<ErrorEnvelope>(body)
        .ok()
        .and_then(|e| e.error)
        .map(|e| (e.code, truncate(&e.message)))
        .unwrap_or_default()
}

/// Turns a non-success ARM response into an [`Error`].
///
/// The code decides first, where it names a placement problem (size,
/// capacity, quota), because ARM reports those under more than one status;
/// otherwise the status decides.
#[must_use]
pub fn classify(status: u16, retry_after: Option<Duration>, body: &[u8]) -> Error {
    let (code, message) = error_code_and_message(body);

    if status == HTTP_TOO_MANY_REQUESTS {
        return Error::Throttled {
            retry_after: retry_after.unwrap_or(DEFAULT_RETRY_AFTER),
        };
    }
    if SKU_NOT_AVAILABLE_CODES.contains(&code.as_str()) {
        return Error::SkuNotAvailable { code, message };
    }
    if ALLOCATION_FAILED_CODES.contains(&code.as_str()) {
        return Error::AllocationFailed { code, message };
    }
    if code == QUOTA_EXCEEDED_CODE {
        return Error::QuotaExceeded { code, message };
    }
    match status {
        HTTP_NOT_FOUND => Error::NotFound { code, message },
        HTTP_CONFLICT => Error::Conflict { code, message },
        HTTP_UNAUTHORIZED => Error::Unauthorized { code, message },
        HTTP_FORBIDDEN => Error::Forbidden { code, message },
        s if s >= HTTP_SERVER_ERROR_MIN => Error::Server {
            status: s,
            code,
            message,
        },
        s if s >= HTTP_BAD_REQUEST => Error::InvalidRequest { code, message },
        s => Error::Decode(format!("status {s} is not an error")),
    }
}

/// Classifies a long-running operation that ended `Failed` by its code
/// alone: a placement code (size, capacity, quota) reads as it would on a
/// synchronous refusal, so the caller decides the same way whichever path
/// ARM took; any other code is [`Error::OperationFailed`].
#[must_use]
pub fn operation_failure(code: &str, message: &str) -> Error {
    let (code, message) = (code.to_string(), truncate(message));
    if SKU_NOT_AVAILABLE_CODES.contains(&code.as_str()) {
        return Error::SkuNotAvailable { code, message };
    }
    if ALLOCATION_FAILED_CODES.contains(&code.as_str()) {
        return Error::AllocationFailed { code, message };
    }
    if code == QUOTA_EXCEEDED_CODE {
        return Error::QuotaExceeded { code, message };
    }
    Error::OperationFailed { code, message }
}

impl Error {
    /// Whether the same call may succeed if repeated later, unchanged.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Throttled { .. } | Self::Server { .. } | Self::Transport(_)
        )
    }

    /// Whether the failure belongs to the zone that was asked for, so the
    /// same request may succeed in another zone (ADR-0161 Decision 2).
    #[must_use]
    pub fn is_zone_specific(&self) -> bool {
        matches!(
            self,
            Self::SkuNotAvailable { .. } | Self::AllocationFailed { .. }
        )
    }

    /// Whether the resource does not exist. A delete treats this as success.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }

    /// The ARM or Entra ID error code, when there is one.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::NotFound { code, .. }
            | Self::SkuNotAvailable { code, .. }
            | Self::AllocationFailed { code, .. }
            | Self::QuotaExceeded { code, .. }
            | Self::Conflict { code, .. }
            | Self::Unauthorized { code, .. }
            | Self::Forbidden { code, .. }
            | Self::InvalidRequest { code, .. }
            | Self::Server { code, .. }
            | Self::OperationFailed { code, .. }
            | Self::TokenExchange { code, .. } => Some(code),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
