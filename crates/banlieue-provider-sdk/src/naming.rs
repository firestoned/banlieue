// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Names of the per-Provider objects the operator creates (ADR-0003).
//!
//! Here, not in the operator, because an `External` provider (ADR-0060
//! Decision 3) runs outside the cluster and must derive the same names — its
//! ServiceAccount and its Lease — from its Provider alone. One definition, so
//! the two can never disagree.

/// Maximum length of a generated object name.
///
/// Kubernetes allows 253 characters for most object names, but a name is also
/// used as a label value (63-character limit), and a Deployment's name is
/// extended with ReplicaSet and pod suffixes. 63 is the safe common bound.
pub const MAX_NAME_LEN: usize = 63;

/// Prefix shared by every generated provider workload name.
pub const WORKLOAD_NAME_PREFIX: &str = "banlieue-provider";

/// Length of the hex hash appended when a derived name must be truncated.
const HASH_SUFFIX_LEN: usize = 8;

/// FNV-1a 32-bit offset basis (RFC-style constant, not a magic number).
const FNV_OFFSET_BASIS: u32 = 2_166_136_261;

/// FNV-1a 32-bit prime.
const FNV_PRIME: u32 = 16_777_619;

/// Derived name shared by every object created for one `Provider`.
///
/// Names longer than [`MAX_NAME_LEN`] are truncated and disambiguated with a
/// hash of the full name, so two long Provider names that share a prefix still
/// produce distinct — and stable — workload names.
///
/// # Arguments
/// * `class` - the `ProviderClass` name the Provider references.
/// * `provider` - the `Provider` object's name.
#[must_use]
pub fn workload_name(class: &str, provider: &str) -> String {
    truncate_with_hash(&format!("{WORKLOAD_NAME_PREFIX}-{class}-{provider}"))
}

/// Derived name for the **cluster-scoped** objects created for one `Provider`.
///
/// Includes the Provider's namespace, which [`workload_name`] deliberately
/// omits. A namespaced object is already disambiguated by its namespace; a
/// cluster-scoped one is not, so two Providers sharing a name and class in
/// different namespaces would collide on a single ClusterRoleBinding and fight
/// over its subject — last writer wins, and the loser silently loses its
/// permissions.
#[must_use]
pub fn cluster_scoped_name(class: &str, provider_namespace: &str, provider: &str) -> String {
    truncate_with_hash(&format!(
        "{WORKLOAD_NAME_PREFIX}-{class}-{provider_namespace}-{provider}"
    ))
}

/// Truncate `name` to [`MAX_NAME_LEN`], appending a hash of the full input when
/// truncation occurs so distinct inputs keep distinct outputs.
pub fn truncate_with_hash(name: &str) -> String {
    if name.len() <= MAX_NAME_LEN {
        return name.to_string();
    }

    // Reserve room for a separator plus the hex hash.
    let keep = MAX_NAME_LEN - HASH_SUFFIX_LEN - 1;
    let head = name[..keep].trim_end_matches('-');
    format!("{head}-{:08x}", fnv1a32(name))
}

/// FNV-1a 32-bit hash.
///
/// Chosen over [`std::collections::hash_map::DefaultHasher`] because the latter
/// is explicitly not stable across Rust releases — a workload name that changed
/// with the compiler would orphan the previous Deployment on every upgrade.
fn fnv1a32(value: &str) -> u32 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in value.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
#[path = "naming_tests.rs"]
mod naming_tests;
