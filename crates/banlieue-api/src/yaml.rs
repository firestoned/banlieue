// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! YAML output for everything banlieue writes (CRDs, bootstrap manifests,
//! kubeconfigs, merged cloud-config), formatted the way `serde_yaml` wrote
//! it: long single-line strings stay on one line, and blank lines inside a
//! block scalar are empty rather than indented.

use serde::Serialize;
use serde_saphyr::SerializerOptions;

/// Single-line strings shorter than this are written plain rather than
/// folded into a `>-` block. serde-saphyr folds past 80 characters by
/// default; nothing banlieue writes comes near this.
const UNFOLDED_LINE_CHARS: usize = 1 << 20;

/// Serialize `value` as YAML, with no trailing whitespace on any line.
///
/// serde-saphyr indents the blank lines of a block scalar, and has no option
/// not to. Those lines are emptied, which parses to the same value; the
/// result is kept only when it does, so a string that really contains a
/// whitespace-only line is written unchanged.
///
/// # Errors
/// Returns the serializer's error if `value` cannot be represented as YAML.
pub fn to_string<T: Serialize>(value: &T) -> Result<String, serde_saphyr::ser::Error> {
    let mut options = SerializerOptions::default();
    options.folded_wrap_chars = UNFOLDED_LINE_CHARS;
    let yaml = serde_saphyr::to_string_with_options(value, options)?;
    let trimmed = empty_blank_lines(&yaml);
    if trimmed == yaml || !same_document(value, &trimmed) {
        return Ok(yaml);
    }
    Ok(trimmed)
}

/// `yaml` with every whitespace-only line made empty.
fn empty_blank_lines(yaml: &str) -> String {
    let mut out = String::with_capacity(yaml.len());
    for line in yaml.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        if body.trim().is_empty() {
            out.push_str(&line[body.len()..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Whether `yaml` parses to the same document as `value`.
fn same_document<T: Serialize>(value: &T, yaml: &str) -> bool {
    match (
        serde_json::to_value(value),
        serde_saphyr::from_str::<serde_json::Value>(yaml),
    ) {
        (Ok(expected), Ok(parsed)) => expected == parsed,
        _ => false,
    }
}

#[cfg(test)]
#[path = "yaml_tests.rs"]
mod yaml_tests;
