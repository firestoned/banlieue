// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Which namespaces `banlieue controller` serves (ADR-0130).
//!
//! The scope is resolved once at startup, in a fixed precedence, and is
//! **never cluster-wide by default**:
//!
//! 1. `--namespace-selector`: every namespace whose labels match;
//! 2. else `--namespace`: exactly that namespace;
//! 3. else the namespace the controller itself runs in.
//!
//! With none of the three, resolution fails rather than widening.
//!
//! The API server cannot filter a watch by the *namespace's* labels, so in
//! selector mode the controllers watch every namespace and gate each
//! reconcile on a [`NamespaceGate`]: a reflector over `Namespace` objects,
//! filtered by the selector on the server side.

use k8s_openapi::api::core::v1::Namespace;
use kube::{Api, Client, Resource, api::ListParams, runtime::reflector::Store};

/// Where a resolved scope came from, for the startup log.
const DESCRIBE_SELECTOR: &str = "namespaces matching selector";
/// Where a single-namespace scope came from, for the startup log.
const DESCRIBE_NAMESPACE: &str = "namespace";

/// Why a scope could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    /// No selector, no `--namespace`, and no current namespace to fall back
    /// to. Serving every namespace instead would defeat the purpose.
    #[error(
        "no namespace scope: set --namespace-selector or --namespace, or run \
         the controller inside a namespace (ADR-0130 never defaults to every namespace)"
    )]
    NoScope,

    /// `--namespace-selector` is not a valid label selector.
    #[error("invalid --namespace-selector {selector:?}: {reason}")]
    InvalidSelector {
        /// The selector as given.
        selector: String,
        /// The API server's complaint.
        reason: String,
    },

    /// The probe that checks the selector failed for a reason that is not the
    /// selector's syntax (RBAC, network).
    #[error("could not check --namespace-selector against the API server: {0}")]
    Probe(String),
}

/// HTTP status the API server uses for a malformed label selector.
const BAD_REQUEST: u16 = 400;

/// The namespaces a controller serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every namespace whose labels match this label selector.
    Selector(String),
    /// Exactly one namespace.
    Namespace(String),
}

impl Scope {
    /// The single namespace to narrow a watch to, or `None` in selector mode
    /// (where the watch is cluster-wide and gated instead).
    #[must_use]
    pub fn namespace(&self) -> Option<&str> {
        match self {
            Self::Namespace(ns) => Some(ns),
            Self::Selector(_) => None,
        }
    }

    /// The label selector, in selector mode only.
    #[must_use]
    pub fn selector(&self) -> Option<&str> {
        match self {
            Self::Selector(selector) => Some(selector),
            Self::Namespace(_) => None,
        }
    }

    /// One line for the startup log naming the rule that applied.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Selector(selector) => format!("{DESCRIBE_SELECTOR} {selector:?}"),
            Self::Namespace(ns) => format!("{DESCRIBE_NAMESPACE} {ns:?}"),
        }
    }

    /// An [`Api`] for `K` honouring this scope: namespaced for a single
    /// namespace, cluster-wide (to be gated) in selector mode.
    #[must_use]
    pub fn api<K>(&self, client: &Client) -> Api<K>
    where
        K: Resource<Scope = kube::core::NamespaceResourceScope>,
        K::DynamicType: Default,
    {
        match self.namespace() {
            Some(ns) => Api::namespaced(client.clone(), ns),
            None => Api::all(client.clone()),
        }
    }
}

/// Treat a blank string as unset, so `BANLIEUE_NAMESPACE=""` in a manifest
/// does not select the empty namespace.
fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Resolve the scope from the three sources, in ADR-0130's precedence.
///
/// # Arguments
/// * `selector` - `--namespace-selector`.
/// * `namespace` - `--namespace`.
/// * `current` - the namespace the controller runs in (in-cluster service
///   account namespace, or the kubeconfig context's namespace).
///
/// Selector *syntax* is not checked here; [`check_selector`] asks the API
/// server, whose parser is the authority.
///
/// # Errors
/// [`ScopeError::NoScope`] if all three are absent or blank.
pub fn resolve_scope(
    selector: Option<&str>,
    namespace: Option<&str>,
    current: Option<&str>,
) -> Result<Scope, ScopeError> {
    if let Some(selector) = non_blank(selector) {
        return Ok(Scope::Selector(selector.to_string()));
    }
    if let Some(ns) = non_blank(namespace) {
        return Ok(Scope::Namespace(ns.to_string()));
    }
    non_blank(current)
        .map(|ns| Scope::Namespace(ns.to_string()))
        .ok_or(ScopeError::NoScope)
}

/// Classify a failed probe LIST: a 400 means the selector is malformed,
/// anything else is not the selector's fault.
#[must_use]
pub fn selector_probe_error(selector: &str, error: &kube::Error) -> ScopeError {
    match error {
        kube::Error::Api(status) if status.code == BAD_REQUEST => ScopeError::InvalidSelector {
            selector: selector.to_string(),
            reason: status.message.clone(),
        },
        other => ScopeError::Probe(other.to_string()),
    }
}

/// Validate a selector by asking the API server to apply it (a one-item
/// `Namespace` LIST), so the syntax accepted is exactly the syntax the
/// watch will later use.
///
/// # Errors
/// [`ScopeError::InvalidSelector`] on a 400; [`ScopeError::Probe`] for any
/// other failure.
pub async fn check_selector(client: &Client, selector: &str) -> Result<(), ScopeError> {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    namespaces
        .list(&ListParams::default().labels(selector).limit(1))
        .await
        .map(|_| ())
        .map_err(|e| selector_probe_error(selector, &e))
}

/// Decides, per object, whether its namespace is served.
///
/// In single-namespace modes the watch is already narrowed, so the gate
/// passes everything. In selector mode it holds the label-filtered
/// `Namespace` reflector, and an empty or unsynced store serves nothing.
#[derive(Clone, Default)]
pub struct NamespaceGate {
    selected: Option<Store<Namespace>>,
}

impl NamespaceGate {
    /// A gate for single-namespace modes: the watch already narrows.
    #[must_use]
    pub fn none() -> Self {
        Self { selected: None }
    }

    /// A gate over the selector-filtered `Namespace` store.
    #[must_use]
    pub fn selected(store: Store<Namespace>) -> Self {
        Self {
            selected: Some(store),
        }
    }

    /// Whether objects in `namespace` should be reconciled.
    #[must_use]
    pub fn serves(&self, namespace: &str) -> bool {
        let Some(store) = &self.selected else {
            return true;
        };
        store
            .state()
            .iter()
            .any(|ns| ns.metadata.name.as_deref() == Some(namespace))
    }
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod scope_tests;
