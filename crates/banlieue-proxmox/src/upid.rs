// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unique process ids: the handle every asynchronous Proxmox call returns.

use crate::error::{Error, Result};

/// Fields in `UPID:node:pid:pstart:starttime:type:id:user:` split on `:`,
/// counting the empty string after the trailing colon.
const UPID_FIELDS: usize = 9;
const HEX_RADIX: u32 = 16;
/// Positions within the split UPID.
const IDX_NODE: usize = 1;
const IDX_HEX_FIRST: usize = 2;
const IDX_HEX_END: usize = 5;
const IDX_TYPE: usize = 5;
const IDX_ID: usize = 6;

/// A parsed Proxmox task id.
///
/// The node is needed to poll the task (`/nodes/{node}/tasks/{upid}`), and is
/// validated as a path-safe token because it is spliced into a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upid {
    raw: String,
    node: String,
    task_type: String,
    id: String,
}

impl Upid {
    /// Parse and validate a UPID string.
    ///
    /// # Errors
    /// [`Error::InvalidUpid`] if the prefix, field count, node or hex fields
    /// are wrong.
    pub fn parse(raw: &str) -> Result<Self> {
        let bad = |why: &str| Error::InvalidUpid(format!("{why}: {raw:?}"));
        let parts: Vec<&str> = raw.split(':').collect();
        if parts.len() != UPID_FIELDS || parts[0] != "UPID" || !parts[UPID_FIELDS - 1].is_empty() {
            return Err(bad("expected UPID:node:pid:pstart:starttime:type:id:user:"));
        }
        let node = parts[IDX_NODE];
        if node.is_empty()
            || !node
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
            || node.starts_with('.')
        {
            return Err(bad("node is not a plain host name"));
        }
        for hex in &parts[IDX_HEX_FIRST..IDX_HEX_END] {
            if hex.is_empty() || !hex.chars().all(|c| c.is_digit(HEX_RADIX)) {
                return Err(bad("pid/pstart/starttime must be hex"));
            }
        }
        Ok(Self {
            raw: raw.to_string(),
            node: node.to_string(),
            task_type: parts[IDX_TYPE].to_string(),
            id: parts[IDX_ID].to_string(),
        })
    }

    /// The node the task runs on.
    #[must_use]
    pub fn node(&self) -> &str {
        &self.node
    }

    /// The task type, e.g. `qmclone`, `qmstart`, `qmdestroy`.
    #[must_use]
    pub fn task_type(&self) -> &str {
        &self.task_type
    }

    /// The object id the task acts on (a VMID), possibly empty.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The whole UPID string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

impl std::fmt::Display for Upid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

#[cfg(test)]
#[path = "upid_tests.rs"]
mod upid_tests;
