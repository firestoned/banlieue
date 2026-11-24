// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Long-running operations (ADR-0164 Decision 2).
//!
//! A `PUT` or `DELETE` that ARM runs asynchronously answers 201 or 202 with
//! an `Azure-AsyncOperation` URL (preferred) or a `Location` URL. The caller
//! stores the [`OperationHandle`] on its object's status, returns, and polls
//! it on a later reconcile: no worker ever blocks on one. A gallery image
//! version took 7 to 22 minutes in the spike; nothing here assumes a duration.
//!
//! The handle is followed with the ARM bearer token, so a URL that is not on
//! the configured Resource Manager endpoint is refused.

use std::time::Duration;

use serde::Deserialize;

use crate::error::{Error, Result, truncate};
use crate::retry::parse_retry_after;

/// Header ARM prefers for operation status.
pub const AZURE_ASYNC_OPERATION_HEADER: &str = "Azure-AsyncOperation";
/// Fallback header.
pub const LOCATION_HEADER: &str = "Location";
/// Polling hint.
pub const RETRY_AFTER_HEADER: &str = "Retry-After";

/// Which header the handle came from; they are polled differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    /// `Azure-AsyncOperation`: the body carries `status`.
    AsyncOperation,
    /// `Location`: 202 while running, then the final resource or 200/204.
    Location,
}

/// An operation to observe later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationHandle {
    /// The URL to poll.
    pub url: String,
    /// Where it came from.
    pub kind: OperationKind,
    /// ARM's polling hint, if any.
    pub retry_after: Option<Duration>,
}

impl OperationHandle {
    /// Builds a handle from a response's headers. `header` looks a header up
    /// by name, case-insensitively. `None` when the response named no
    /// operation, meaning the call completed synchronously.
    ///
    /// # Errors
    /// [`Error::UntrustedUrl`] if the URL is not under `endpoint` (the
    /// Resource Manager base URL, such as `https://management.azure.com`).
    pub fn from_headers(
        header: impl Fn(&str) -> Option<String>,
        endpoint: &str,
    ) -> Result<Option<Self>> {
        let (url, kind) = match (
            header(AZURE_ASYNC_OPERATION_HEADER),
            header(LOCATION_HEADER),
        ) {
            (Some(url), _) => (url, OperationKind::AsyncOperation),
            (None, Some(url)) => (url, OperationKind::Location),
            (None, None) => return Ok(None),
        };
        ensure_under_endpoint(&url, endpoint)?;
        Ok(Some(Self {
            url,
            kind,
            retry_after: header(RETRY_AFTER_HEADER)
                .as_deref()
                .and_then(parse_retry_after),
        }))
    }
}

/// Refuses `url` unless it is `endpoint` itself or a path below it, over
/// HTTPS. A host that merely starts with the endpoint's host is refused.
///
/// # Errors
/// [`Error::UntrustedUrl`].
pub fn ensure_under_endpoint(url: &str, endpoint: &str) -> Result<()> {
    let base = endpoint.trim_end_matches('/');
    let trusted = base.starts_with("https://")
        && url
            .strip_prefix(base)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || rest.starts_with('?'));
    if trusted {
        return Ok(());
    }
    Err(Error::UntrustedUrl(url.to_string()))
}

/// The state of an `Azure-AsyncOperation`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationStatus {
    /// Not finished (any non-terminal status).
    InProgress,
    /// Done.
    Succeeded,
    /// Ended in error.
    Failed {
        /// ARM error code, such as `OSProvisioningTimedOut`.
        code: String,
        /// ARM message.
        message: String,
    },
    /// Canceled.
    Canceled,
}

#[derive(Deserialize)]
struct StatusBody {
    status: Option<String>,
    error: Option<StatusError>,
}

#[derive(Deserialize)]
struct StatusError {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

/// Parses an `Azure-AsyncOperation` status body.
///
/// # Errors
/// [`Error::Decode`] if the body is not JSON or has no `status`.
pub fn parse_operation_status(body: &[u8]) -> Result<OperationStatus> {
    let parsed: StatusBody = serde_json::from_slice(body)
        .map_err(|e| Error::Decode(format!("operation status: {e}")))?;
    let status = parsed
        .status
        .ok_or_else(|| Error::Decode("operation status without `status`".to_string()))?;
    Ok(match status.as_str() {
        "Succeeded" => OperationStatus::Succeeded,
        "Canceled" => OperationStatus::Canceled,
        "Failed" => {
            let error = parsed.error.unwrap_or(StatusError {
                code: String::new(),
                message: String::new(),
            });
            OperationStatus::Failed {
                code: error.code,
                message: truncate(&error.message),
            }
        }
        _ => OperationStatus::InProgress,
    })
}

#[cfg(test)]
#[path = "operation_tests.rs"]
mod operation_tests;
