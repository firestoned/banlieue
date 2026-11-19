// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! File I/O both CLI binaries do the same way: read the Grype report,
//! default the timestamp, and write the OpenVEX document.

use std::path::Path;

use crate::auto_vex_presence::{Document, GrypeReport, read_file_capped};

/// `strftime` format of the RFC 3339 UTC timestamp a document carries.
const TIMESTAMP_FORMAT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// Read and parse the `--grype-json` report at `path`.
///
/// # Errors
/// A message naming the flag and the path when the file cannot be read
/// (missing, or over the input size cap) or is not a Grype report.
pub fn load_grype(path: &Path) -> anyhow::Result<GrypeReport> {
    let bytes = read_file_capped(path)
        .map_err(|e| anyhow::anyhow!("failed to read --grype-json {}: {}", path.display(), e))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("failed to parse --grype-json {}: {}", path.display(), e))
}

/// The `--timestamp` given, or the current time as RFC 3339 UTC.
#[must_use]
pub fn timestamp_or_now(timestamp: Option<String>) -> String {
    timestamp.unwrap_or_else(|| {
        jiff::Timestamp::now()
            .strftime(TIMESTAMP_FORMAT)
            .to_string()
    })
}

/// Write `doc` as pretty JSON with a trailing newline to `output`, or to
/// stdout when there is none.
///
/// # Errors
/// When the document does not serialize, or a message naming `--output` and
/// the path when the file cannot be written.
pub fn write_document(doc: &Document, output: Option<&Path>) -> anyhow::Result<()> {
    let rendered = serde_json::to_string_pretty(doc)? + "\n";
    match output {
        Some(path) => std::fs::write(path, &rendered)
            .map_err(|e| anyhow::anyhow!("failed to write --output {}: {}", path.display(), e))?,
        None => print!("{rendered}"),
    }
    Ok(())
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
