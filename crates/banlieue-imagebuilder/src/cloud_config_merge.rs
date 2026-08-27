// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Cloud-config YAML deep-merge (ADR-0037).
//!
//! Merges N cloud-config documents into one, used by
//! `banlieue-imagebuilder` to produce the single merged Secret that
//! `OSArtifact.spec.artifacts.cloudConfigRef` points at.
//!
//! **Merge semantics (banlieue's own contract):**
//! - Maps deep-merge; for a scalar or map value at the same key, the later
//!   (higher-index) source wins.
//! - Lists at the same key **concatenate in order** (base's entries first,
//!   each overlay's appended after).
//! - A type mismatch at the same key (e.g. one config has `users:` as a
//!   list, another as a scalar) is a hard merge error — "explicit over
//!   implicit."
//!
//! Implemented as a pure, unit-tested function on [`serde_yaml::Value`] —
//! no dependency on yip/mergo.

use serde_yaml::Value;

/// Errors from cloud-config merge.
#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    /// YAML parse failure on one of the input documents.
    #[error("cloud-config parse error (source index {index}): {source}")]
    Parse {
        index: usize,
        source: serde_yaml::Error,
    },

    /// Type mismatch at a key path across two sources.
    #[error("cloud-config merge conflict at key {key:?}: {detail}")]
    TypeMismatch { key: String, detail: String },
}

/// Deep-merge two [`Value`]s. `overlay` is layered on top of `base`,
/// modifying `base` in place.
///
/// - Maps: recursively merge each key.
/// - Sequences: concatenate (`base` entries first, then `overlay`'s).
/// - Scalars / nulls: `overlay` wins (replaces `base`).
/// - Type mismatch (e.g. map vs. sequence): returns [`MergeError::TypeMismatch`].
fn deep_merge(base: &mut Value, overlay: Value, path: &str) -> Result<(), MergeError> {
    match (base, overlay) {
        // Both mappings → recurse per key.
        (Value::Mapping(base_map), Value::Mapping(overlay_map)) => {
            for (key, overlay_val) in overlay_map {
                let key_str = match &key {
                    Value::String(s) => s.clone(),
                    other => format!("{other:?}"),
                };
                let child_path = if path.is_empty() {
                    key_str.clone()
                } else {
                    format!("{path}.{key_str}")
                };

                if let Some(base_val) = base_map.get_mut(&key) {
                    deep_merge(base_val, overlay_val, &child_path)?;
                } else {
                    base_map.insert(key, overlay_val);
                }
            }
            Ok(())
        }

        // Both sequences → concatenate.
        (Value::Sequence(base_seq), Value::Sequence(overlay_seq)) => {
            base_seq.extend(overlay_seq);
            Ok(())
        }

        // Both scalars / nulls / tagged → overlay wins.
        (base_val, overlay_val)
            if matches!(
                (&*base_val, &overlay_val),
                (Value::Null, _)
                    | (_, Value::Null)
                    | (Value::Bool(_), Value::Bool(_))
                    | (Value::Number(_), Value::Number(_))
                    | (Value::String(_), Value::String(_))
                    | (Value::Bool(_), Value::String(_))
                    | (Value::String(_), Value::Bool(_))
                    | (Value::Number(_), Value::String(_))
                    | (Value::String(_), Value::Number(_))
                    | (Value::Bool(_), Value::Number(_))
                    | (Value::Number(_), Value::Bool(_))
            ) =>
        {
            *base_val = overlay_val;
            Ok(())
        }

        // Type mismatch — hard error per ADR-0037 Decision #3.
        (base_val, overlay_val) => Err(MergeError::TypeMismatch {
            key: path.to_string(),
            detail: format!(
                "base is {}, overlay is {}",
                yaml_type_name(base_val),
                yaml_type_name(&overlay_val),
            ),
        }),
    }
}

fn yaml_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Sequence(_) => "sequence",
        Value::Mapping(_) => "mapping",
        Value::Tagged(_) => "tagged",
    }
}

/// Merge N cloud-config YAML strings into a single YAML document.
///
/// `sources` are processed in order: index 0 is the base, each subsequent
/// entry layers on top. Returns the merged YAML as a string. An empty
/// `sources` slice returns an empty mapping (`{}`).
///
/// # Errors
///
/// Returns [`MergeError::Parse`] if any source is not valid YAML, or
/// [`MergeError::TypeMismatch`] if two sources disagree on the type of a
/// value at the same key path.
pub fn merge_cloud_configs(sources: &[&str]) -> Result<String, MergeError> {
    let mut merged = Value::Mapping(serde_yaml::Mapping::new());

    for (i, src) in sources.iter().enumerate() {
        let doc: Value = serde_yaml::from_str(src).map_err(|e| MergeError::Parse {
            index: i,
            source: e,
        })?;
        deep_merge(&mut merged, doc, "")?;
    }

    // serde_yaml::to_string always starts with `---\n`; strip it and
    // prepend the `#cloud-config` magic header that kairos/yip requires to
    // recognise the file as cloud-config. The header is a YAML comment so
    // it is lost during parse → re-serialize; we must re-add it.
    let yaml = serde_yaml::to_string(&merged).unwrap_or_default();
    let body = yaml.strip_prefix("---\n").unwrap_or(&yaml);
    Ok(format!("#cloud-config\n{body}"))
}

#[cfg(test)]
#[path = "cloud_config_merge_tests.rs"]
mod cloud_config_merge_tests;
