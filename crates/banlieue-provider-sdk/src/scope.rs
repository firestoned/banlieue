// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Which namespaces a banlieue binary serves (ADR-0130, ADR-0131).
//!
//! Shared by the controller and the operator, so there is one definition of
//! "which namespaces do we serve". The scope is resolved once at startup, in
//! a fixed precedence, and is **never cluster-wide by default**:
//!
//! 1. a namespace selector: every namespace whose labels match;
//! 2. else a namespace: exactly that namespace;
//! 3. else the namespace the binary itself runs in.
//!
//! With none of the three, resolution fails rather than widening.
//!
//! The API server cannot filter a watch by the *namespace's* labels, so in
//! selector mode the controllers watch every namespace and gate each
//! reconcile on a [`NamespaceGate`]: a reflector over `Namespace` objects,
//! filtered by the selector on the server side.

use futures::StreamExt;
use k8s_openapi::api::core::v1::Namespace;
use std::collections::BTreeSet;
use tracing::{info, warn};

use kube::{
    Api, Client, Resource,
    api::{ApiResource, DynamicObject, ListParams},
    runtime::{
        WatchStreamExt,
        controller::Action,
        reflector,
        reflector::{ObjectRef, Store},
        watcher::{Config, watcher},
    },
};

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
        "no namespace scope: set a namespace selector or a namespace, or run \
         inside a namespace (ADR-0130 never defaults to every namespace)"
    )]
    NoScope,

    /// The namespace selector is not a valid label selector.
    #[error("invalid namespace selector {selector:?}: {reason}")]
    InvalidSelector {
        /// The selector as given.
        selector: String,
        /// The API server's complaint.
        reason: String,
    },

    /// The probe that checks the selector failed for a reason that is not the
    /// selector's syntax (RBAC, network).
    #[error("could not check the namespace selector against the API server: {0}")]
    Probe(String),

    /// The selector-filtered `Namespace` watch never completed its first sync.
    #[error("waiting for the namespace selector watch to sync: {0}")]
    Sync(String),
}

/// HTTP status the API server uses for a malformed label selector.
const BAD_REQUEST: u16 = 400;

/// The namespaces a binary serves.
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

    /// [`Scope::api`] for a resource known only at runtime (a Cluster API
    /// `Machine`, which banlieue has no Rust type for), described by
    /// `resource`. The same rule, so the two never drift.
    #[must_use]
    pub fn api_with(&self, client: &Client, resource: &ApiResource) -> Api<DynamicObject> {
        match self.namespace() {
            Some(ns) => Api::namespaced_with(client.clone(), ns, resource),
            None => Api::all_with(client.clone(), resource),
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
/// * `selector` - the namespace selector flag.
/// * `namespace` - the single-namespace flag.
/// * `current` - the namespace the binary runs in (in-cluster service
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

/// The one namespace a provider binary serves (ADR-0131): `--namespace`, else
/// the namespace it runs in. A provider takes no selector: its credential
/// reads are confined to one namespace, so serving more would only widen a
/// watch it cannot act on.
///
/// # Arguments
/// * `namespace` - the `--namespace` flag.
/// * `current` - the namespace the provider runs in (in-cluster service
///   account namespace, or the kubeconfig context's namespace).
///
/// # Errors
/// [`ScopeError::NoScope`] if both are blank.
pub fn provider_namespace(namespace: Option<&str>, current: &str) -> Result<String, ScopeError> {
    match resolve_scope(None, namespace, Some(current))? {
        Scope::Namespace(ns) => Ok(ns),
        // `resolve_scope` returns a selector only when handed one.
        Scope::Selector(_) => Err(ScopeError::NoScope),
    }
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
        // A keyed lookup: this runs on every reconcile, and `state()` would
        // clone every selected namespace each time.
        store.get(&ObjectRef::new(namespace)).is_some()
    }

    /// `Some(action)` when a reconcile for `namespace` should stop before doing
    /// anything: the object is outside the served set, so it is left alone
    /// and woken only by a change (labelling the namespace in reconciles
    /// everything again, so no timer is needed).
    ///
    /// An object being deleted is never skipped (ADR-0130 decision 6): its
    /// finalizer must still run, or un-labelling a namespace would strand it.
    #[must_use]
    pub fn skip_unserved(&self, namespace: &str, deleting: bool) -> Option<Action> {
        (!deleting && !self.serves(namespace)).then(Action::await_change)
    }

    /// The names of the namespaces currently served in selector mode; empty
    /// for a gate that passes everything. Used to tell a real change of the
    /// served set from a relist or an unrelated edit to a namespace.
    #[must_use]
    pub fn names(&self) -> BTreeSet<String> {
        let Some(store) = &self.selected else {
            return BTreeSet::new();
        };
        store
            .state()
            .iter()
            .filter_map(|ns| ns.metadata.name.clone())
            .collect()
    }
}

/// Prefix of the namespaces Kubernetes itself owns (`kube-system`,
/// `kube-public`, `kube-node-lease`).
const SYSTEM_NAMESPACE_PREFIX: &str = "kube-";

/// The system namespaces among `served`, for a startup warning: a negated
/// selector such as `!foo` serves them, which is legal but rarely meant.
#[must_use]
pub fn system_namespaces(served: &BTreeSet<String>) -> Vec<String> {
    served
        .iter()
        .filter(|name| name.starts_with(SYSTEM_NAMESPACE_PREFIX))
        .cloned()
        .collect()
}

/// Whether no flag chose the scope, so it is the implicit fallback to the
/// binary's own namespace (ADR-0130).
#[must_use]
pub fn scope_is_implicit(selector: Option<&str>, namespace: Option<&str>) -> bool {
    non_blank(selector).is_none() && non_blank(namespace).is_none()
}

/// A stream that yields `()` each time the served set of namespaces may have
/// changed, for `Controller::reconcile_all_on`. It stays pending while the
/// sender lives and nothing changes.
pub fn scope_trigger(rx: tokio::sync::watch::Receiver<u64>) -> impl futures::Stream<Item = ()> {
    futures::stream::unfold(rx, |mut rx| async move {
        rx.changed().await.ok()?;
        Some(((), rx))
    })
}

/// Start the selector-filtered `Namespace` reflector, wait for its first
/// sync, and return the gate over it. `changed` is bumped when the served set
/// really changes, so the caller reconciles everything again.
///
/// Readiness is the caller's to hold back until this returns: an unsynced
/// store must not read as "serve nothing" and quietly drop work.
///
/// # Errors
/// [`ScopeError::Sync`] if the watch ends before it has synced.
pub async fn spawn_namespace_gate(
    client: &Client,
    selector: &str,
    changed: tokio::sync::watch::Sender<u64>,
) -> Result<NamespaceGate, ScopeError> {
    let (store, writer) = reflector::store::<Namespace>();
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let stream = watcher(namespaces, Config::default().labels(selector))
        .default_backoff()
        .reflect(writer);
    let watched = NamespaceGate::selected(store.clone());
    tokio::spawn(async move {
        // Bump only when the set of served names really changes. A relist
        // (every Init event after a watch restart) and an edit to some other
        // field of a selected namespace both leave it alone, and each bump
        // reconciles every object in every controller.
        let mut last = BTreeSet::new();
        stream
            .for_each(|event| {
                match event {
                    Ok(_) => {
                        let now = watched.names();
                        if now != last {
                            last = now;
                            changed.send_modify(|n| *n = n.wrapping_add(1));
                        }
                    }
                    Err(e) => warn!(error = %e, "Namespace selector watch error; retrying"),
                }
                futures::future::ready(())
            })
            .await;
    });
    store
        .wait_until_ready()
        .await
        .map_err(|e| ScopeError::Sync(e.to_string()))?;
    let gate = NamespaceGate::selected(store);
    let served = gate.names();
    info!(namespaces = served.len(), "namespace selector synced");
    let system = system_namespaces(&served);
    if !system.is_empty() {
        warn!(
            namespaces = ?system,
            "the namespace selector serves Kubernetes system namespaces; check it is not negated"
        );
    }
    Ok(gate)
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod scope_tests;
