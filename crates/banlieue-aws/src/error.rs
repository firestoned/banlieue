// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Typed AWS errors (ADR-0174 Decision 2).
//!
//! EC2 reports failures as `<Response><Errors><Error><Code>` XML, STS as
//! `<ErrorResponse><Error>`, S3 as a bare `<Error>`, and the JSON 1.1
//! services as `{"__type": ..., "message": ...}`. Whatever the shape, the
//! provider has to tell apart: throttled (wait), no capacity or the type not
//! offered in this zone (re-place, ADR-0173), quota exhausted
//! (unschedulable), a reused client token with different parameters (a bug),
//! not found (already gone, or not visible yet), refused by policy, a
//! credential AWS rejects, and invalid (a bug or a bad spec).
//! [`classify`] makes that decision once, from the code and the HTTP status.

/// Error messages are cut to this many characters so a large body (an
/// encoded authorization message, an HTML error page) cannot flood logs or a
/// condition message.
pub const MAX_MESSAGE_LEN: usize = 512;

const HTTP_CLIENT_ERROR_MIN: u16 = 400;
const HTTP_TOO_MANY_REQUESTS: u16 = 429;
const HTTP_SERVER_ERROR_MIN: u16 = 500;
const HTTP_SERVICE_UNAVAILABLE: u16 = 503;

/// Codes meaning "slow down" on any of the services banlieue calls.
const THROTTLED_CODES: [&str; 7] = [
    "RequestLimitExceeded",
    "Throttling",
    "ThrottlingException",
    "RequestThrottled",
    "RequestThrottledException",
    "TooManyRequestsException",
    "SlowDown",
];
/// Codes meaning the zone has no capacity for the type right now.
const INSUFFICIENT_CAPACITY_CODES: [&str; 2] =
    ["InsufficientInstanceCapacity", "InsufficientCapacity"];
/// EC2's code for an instance type (or another feature) not offered in the
/// requested zone.
const UNSUPPORTED_CODE: &str = "Unsupported";
/// Codes meaning an account limit would be exceeded.
const QUOTA_EXCEEDED_CODES: [&str; 2] = ["VcpuLimitExceeded", "InstanceLimitExceeded"];
/// A reused `ClientToken` with different parameters (seen live).
const IDEMPOTENT_MISMATCH_CODE: &str = "IdempotentParameterMismatch";
/// EC2's not-found codes all end with this (`InvalidInstanceID.NotFound`,
/// `InvalidAMIID.NotFound`, `InvalidSnapshot.NotFound`, ...).
const NOT_FOUND_SUFFIX: &str = ".NotFound";
/// Not-found codes that do not follow EC2's suffix.
const NOT_FOUND_CODES: [&str; 5] = [
    "NoSuchKey",
    "NoSuchUpload",
    "NoSuchBucket",
    "NoSuchEntity",
    "ResourceNotFoundException",
];
/// The identity is known, but policy does not allow the action.
const FORBIDDEN_CODES: [&str; 3] = [
    "UnauthorizedOperation",
    "AccessDenied",
    "AccessDeniedException",
];
/// The credential itself is refused: expired, unknown, or badly signed.
const UNAUTHENTICATED_CODES: [&str; 7] = [
    "AuthFailure",
    "ExpiredToken",
    "ExpiredTokenException",
    "InvalidClientTokenId",
    "SignatureDoesNotMatch",
    "InvalidIdentityToken",
    "IDPRejectedClaim",
];

/// What AWS said about a failed request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AwsFault {
    /// HTTP status.
    pub status: u16,
    /// AWS error code; empty when the body had none.
    pub code: String,
    /// AWS message, cut to [`MAX_MESSAGE_LEN`].
    pub message: String,
    /// AWS request ID, for support cases and audit; empty when absent.
    pub request_id: String,
}

impl AwsFault {
    /// A fault with the message cut to [`MAX_MESSAGE_LEN`] characters.
    #[must_use]
    pub fn new(status: u16, code: &str, message: &str, request_id: &str) -> Self {
        Self {
            status,
            code: code.to_string(),
            message: truncate(message),
            request_id: request_id.to_string(),
        }
    }
}

impl std::fmt::Display for AwsFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {} {}: {}", self.status, self.code, self.message)?;
        if !self.request_id.is_empty() {
            write!(f, " (request {})", self.request_id)?;
        }
        Ok(())
    }
}

/// Everything that can go wrong talking to AWS.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Throttled: wait and retry ([`crate::retry`]).
    #[error("throttled by AWS: {0}")]
    Throttled(AwsFault),
    /// The zone has no capacity for the instance type right now (ADR-0173).
    #[error("insufficient capacity: {0}")]
    InsufficientCapacity(AwsFault),
    /// The instance type is not offered in the requested zone (ADR-0173).
    #[error("not offered here: {0}")]
    NotOffered(AwsFault),
    /// An account limit (vCPU quota, instance count) would be exceeded.
    #[error("quota exceeded: {0}")]
    QuotaExceeded(AwsFault),
    /// The same client token was used with different parameters.
    #[error("idempotent parameter mismatch: {0}")]
    IdempotentParameterMismatch(AwsFault),
    /// The resource does not exist, or (just after a create) is not visible
    /// yet: EC2 is eventually consistent (ADR-0174 Decision 2).
    #[error("not found: {0}")]
    NotFound(AwsFault),
    /// The identity is valid but policy refuses the action.
    #[error("forbidden: {0}")]
    Forbidden(AwsFault),
    /// The credential is refused: expired, unknown, or the signature does
    /// not match.
    #[error("unauthenticated: {0}")]
    Unauthenticated(AwsFault),
    /// Any other 4xx.
    #[error("invalid request: {0}")]
    InvalidRequest(AwsFault),
    /// Any other 5xx.
    #[error("AWS server error: {0}")]
    Server(AwsFault),
    /// A request banlieue refused to send: it would break a rule AWS or
    /// ADR-0174 sets (user data too large, a bad tag, a missing field).
    #[error("refused before sending: {0}")]
    Invalid(String),
    /// A response that could not be read (malformed XML or JSON, a missing
    /// element).
    #[error("could not decode the AWS response: {0}")]
    Decode(String),
    /// The request never got an HTTP answer (connect, TLS, timeout).
    #[error("transport: {0}")]
    Transport(String),
}

/// `Result` with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// True for an error a retry may fix by waiting ([`crate::retry`]).
    #[must_use]
    pub fn is_throttled(&self) -> bool {
        matches!(self, Self::Throttled(_))
    }

    /// True when the machine should be re-placed in another zone
    /// (ADR-0173): no capacity, or the type is not offered there.
    #[must_use]
    pub fn is_zone_unavailable(&self) -> bool {
        matches!(self, Self::InsufficientCapacity(_) | Self::NotOffered(_))
    }

    /// True when the resource is gone (or not visible yet).
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound(_))
    }

    /// The AWS fault, when AWS answered.
    #[must_use]
    pub fn fault(&self) -> Option<&AwsFault> {
        match self {
            Self::Throttled(f)
            | Self::InsufficientCapacity(f)
            | Self::NotOffered(f)
            | Self::QuotaExceeded(f)
            | Self::IdempotentParameterMismatch(f)
            | Self::NotFound(f)
            | Self::Forbidden(f)
            | Self::Unauthenticated(f)
            | Self::InvalidRequest(f)
            | Self::Server(f) => Some(f),
            Self::Invalid(_) | Self::Decode(_) | Self::Transport(_) => None,
        }
    }
}

/// Classifies an AWS fault by its code first, then its HTTP status.
///
/// A known code wins over the status (EC2 answers `RequestLimitExceeded`
/// with 503, `Unsupported` with 400); an unknown code falls back to the
/// status class.
#[must_use]
pub fn classify(fault: AwsFault) -> Error {
    let code = fault.code.as_str();
    if THROTTLED_CODES.contains(&code) {
        return Error::Throttled(fault);
    }
    if INSUFFICIENT_CAPACITY_CODES.contains(&code) {
        return Error::InsufficientCapacity(fault);
    }
    if code == UNSUPPORTED_CODE {
        return Error::NotOffered(fault);
    }
    if QUOTA_EXCEEDED_CODES.contains(&code) {
        return Error::QuotaExceeded(fault);
    }
    if code == IDEMPOTENT_MISMATCH_CODE {
        return Error::IdempotentParameterMismatch(fault);
    }
    if code.ends_with(NOT_FOUND_SUFFIX) || NOT_FOUND_CODES.contains(&code) {
        return Error::NotFound(fault);
    }
    if FORBIDDEN_CODES.contains(&code) {
        return Error::Forbidden(fault);
    }
    if UNAUTHENTICATED_CODES.contains(&code) {
        return Error::Unauthenticated(fault);
    }
    if fault.status == HTTP_TOO_MANY_REQUESTS || fault.status == HTTP_SERVICE_UNAVAILABLE {
        return Error::Throttled(fault);
    }
    if fault.status >= HTTP_SERVER_ERROR_MIN {
        return Error::Server(fault);
    }
    if fault.status >= HTTP_CLIENT_ERROR_MIN {
        return Error::InvalidRequest(fault);
    }
    // A non-error status with an error body: treat as a server fault rather
    // than silently as success.
    Error::Server(fault)
}

/// Cuts `message` to [`MAX_MESSAGE_LEN`] characters, on a character
/// boundary.
fn truncate(message: &str) -> String {
    message.chars().take(MAX_MESSAGE_LEN).collect()
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
