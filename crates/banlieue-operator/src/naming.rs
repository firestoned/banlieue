// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Derived names and labels for per-instance provider workloads.
//!
//! Every object the operator creates for a `Provider` — Deployment,
//! ServiceAccount, Role, RoleBinding, Lease — shares one derived name so the
//! whole set is discoverable from the Provider alone:
//!
//! ```text
//! banlieue-provider-<class>-<provider-name>
//! ```
//!
//! These functions are pure and total: the operator recomputes them on every
//! reconcile, so they must be deterministic or a rename would orphan the
//! previous object instead of updating it (ADR-0003).

use std::collections::BTreeMap;

/// Maximum length of a generated object name.
///
/// Kubernetes allows 253 characters for most object names, but a name is also
/// used as a label value (63-character limit), and a Deployment's name is
/// extended with ReplicaSet and pod suffixes. 63 is the safe common bound.
pub const MAX_NAME_LEN: usize = 63;

/// Prefix shared by every generated provider workload name.
pub const WORKLOAD_NAME_PREFIX: &str = "banlieue-provider";

/// Label naming the `Provider` a workload (or infra CR) belongs to.
///
/// This is the routing key of the per-instance topology: each provider pod
/// runs a server-side filtered watch on this selector, so its informer cache
/// holds only its own objects and one hung backend cannot stall another.
pub const LABEL_PROVIDER: &str = "banlieue.io/provider";

/// Label naming the namespace of the `Provider` a workload belongs to.
///
/// Needed because [`LABEL_PROVIDER`] alone is not unique cluster-wide: two
/// Providers can share a name in different namespaces. Any selector that
/// reaches cluster-scoped objects, or that crosses namespaces, must pin both.
pub const LABEL_PROVIDER_NAMESPACE: &str = "banlieue.io/provider-namespace";

/// Label naming the `ProviderClass` a workload was instantiated from.
pub const LABEL_PROVIDER_CLASS: &str = "banlieue.io/provider-class";

/// Standard Kubernetes application-name label.
pub const LABEL_NAME: &str = "app.kubernetes.io/name";

/// Standard Kubernetes component label.
pub const LABEL_COMPONENT: &str = "app.kubernetes.io/component";

/// Standard Kubernetes managed-by label.
pub const LABEL_MANAGED_BY: &str = "app.kubernetes.io/managed-by";

/// Standard Kubernetes instance label.
pub const LABEL_INSTANCE: &str = "app.kubernetes.io/instance";

/// Value of [`LABEL_NAME`] on every banlieue-managed object.
pub const APP_NAME: &str = "banlieue";

/// Value of [`LABEL_MANAGED_BY`] on objects this operator owns.
pub const MANAGED_BY: &str = "banlieue-operator";

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

/// Label selector matching every object created for one `Provider`.
///
/// Pins both name and namespace: pruning orphans after a class change selects
/// by provider identity, and an under-specified selector would let one tenant's
/// prune delete another tenant's workload.
#[must_use]
pub fn owned_by_selector(provider_namespace: &str, provider: &str) -> String {
    format!("{LABEL_PROVIDER}={provider},{LABEL_PROVIDER_NAMESPACE}={provider_namespace}")
}

/// Component label value for a backend, e.g. `provider-vsphere`.
#[must_use]
pub fn component(backend: &str) -> String {
    format!("provider-{backend}")
}

/// Full label set applied to every object created for a `Provider`.
///
/// Prefer [`workload_labels_for`], which also records the Provider's namespace;
/// this shorter form is kept for callers that genuinely have no namespace in
/// hand and never touch cluster-scoped objects.
#[must_use]
pub fn workload_labels(class: &str, provider: &str, backend: &str) -> BTreeMap<String, String> {
    let mut labels = selector_labels(provider);
    labels.insert(LABEL_COMPONENT.to_string(), component(backend));
    labels.insert(LABEL_MANAGED_BY.to_string(), MANAGED_BY.to_string());
    labels.insert(LABEL_PROVIDER_CLASS.to_string(), class.to_string());
    labels.insert(LABEL_INSTANCE.to_string(), workload_name(class, provider));
    labels
}

/// Full label set including the Provider's namespace, so cluster-scoped objects
/// and cross-namespace selectors can identify their owner exactly.
#[must_use]
pub fn workload_labels_for(
    class: &str,
    provider_namespace: &str,
    provider: &str,
    backend: &str,
) -> BTreeMap<String, String> {
    let mut labels = workload_labels(class, provider, backend);
    labels.insert(
        LABEL_PROVIDER_NAMESPACE.to_string(),
        provider_namespace.to_string(),
    );
    labels
}

/// Minimal label set used as a Deployment's `spec.selector`.
///
/// `spec.selector` is **immutable** once a Deployment exists, so it is built
/// only from values that cannot change for a given Provider: the application
/// name and the Provider's own name. Class and backend are deliberately
/// excluded — editing a ProviderClass must not strand an unpatchable
/// Deployment.
#[must_use]
pub fn selector_labels(provider: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (LABEL_NAME.to_string(), APP_NAME.to_string()),
        (LABEL_PROVIDER.to_string(), provider.to_string()),
    ])
}

/// Server-side watch selector a provider workload uses to see only its own
/// objects, e.g. `banlieue.io/provider=prod-vc`.
#[must_use]
pub fn provider_selector(provider: &str) -> String {
    format!("{LABEL_PROVIDER}={provider}")
}

/// Truncate `name` to [`MAX_NAME_LEN`], appending a hash of the full input when
/// truncation occurs so distinct inputs keep distinct outputs.
fn truncate_with_hash(name: &str) -> String {
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
