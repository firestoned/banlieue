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

// Moved to the SDK so an External provider derives the same names
// (ADR-0060 Decision 3); re-exported so existing paths keep working.
pub use banlieue_provider_sdk::naming::{
    MAX_NAME_LEN, WORKLOAD_NAME_PREFIX, cluster_scoped_name, workload_name,
};

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

#[cfg(test)]
#[path = "naming_tests.rs"]
mod naming_tests;
