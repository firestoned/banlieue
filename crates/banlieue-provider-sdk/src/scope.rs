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

use banlieue_api::common::{LabelSelector, LabelSelectorOperator};
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

/// The provider backends that can serve tenant namespaces from one shared
/// provider (ADR-0133): each parses `--watch-namespace-selector` and reads its
/// own `Provider`, credentials and CA bundle from its home namespace.
///
/// One list for both readers: the operator renders the flag only for these (a
/// binary that does not parse it would fail at startup), and the controller
/// offers a `Provider` in the provider namespace only for these (a machine
/// nothing will reconcile must not be scheduled). A backend joins it in the
/// same change that makes its binary accept the flag.
pub const SHARED_PROVIDER_BACKENDS: &[&str] =
    &["vsphere", "libvirt", "proxmox", "cloud-hypervisor", "azure"];

/// Whether `backend` can serve tenant namespaces from a shared provider.
#[must_use]
pub fn supports_shared_provider(backend: &str) -> bool {
    SHARED_PROVIDER_BACKENDS.contains(&backend)
}

/// Render a label selector as the string form a `--watch-namespace-selector`
/// flag takes, deterministically (labels sorted, expressions in order).
///
/// Returns `None` for an empty selector: Kubernetes reads `{}` as "every
/// namespace", and a blank flag means "unset", so rendering it would turn into
/// "home only" on the provider while the controller read it as "all". It
/// serves nothing instead (ADR-0133 Decision 11).
#[must_use]
pub fn selector_to_string(selector: &LabelSelector) -> Option<String> {
    if selector.is_empty() {
        return None;
    }
    let labels = selector
        .match_labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"));
    let expressions = selector.match_expressions.iter().map(|r| {
        let values = r.values.join(",");
        match r.operator {
            LabelSelectorOperator::In => format!("{} in ({values})", r.key),
            LabelSelectorOperator::NotIn => format!("{} notin ({values})", r.key),
            LabelSelectorOperator::Exists => r.key.clone(),
            LabelSelectorOperator::DoesNotExist => format!("!{}", r.key),
        }
    });
    Some(labels.chain(expressions).collect::<Vec<_>>().join(","))
}

/// What a provider serves (ADR-0133 Decision 4): its **home** namespace, where
/// its `Provider`, credentials Secret, CA bundle and Lease live, plus, with a
/// selector, the namespaces whose labels match.
///
/// Unlike [`Scope`] the home namespace is always served and never replaced by
/// the selector: the provider reads its own `Provider` and credentials from
/// home whatever namespace a machine is in, so it holds no Secret access
/// anywhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderScope {
    home: String,
    selector: Option<String>,
}

impl ProviderScope {
    /// Resolve the scope.
    ///
    /// # Arguments
    /// * `namespace` - `--namespace`: the home namespace.
    /// * `current` - the namespace the provider runs in; the home when no flag is set.
    /// * `selector` - `--watch-namespace-selector`; blank means none.
    ///
    /// # Errors
    /// [`ScopeError::NoScope`] when no home namespace can be determined.
    pub fn resolve(
        namespace: Option<&str>,
        current: &str,
        selector: Option<&str>,
    ) -> Result<Self, ScopeError> {
        Ok(Self {
            home: provider_namespace(namespace, current)?,
            selector: non_blank(selector).map(str::to_string),
        })
    }

    /// The home namespace.
    #[must_use]
    pub fn home(&self) -> &str {
        &self.home
    }

    /// The namespace label selector, when the provider serves more than home.
    #[must_use]
    pub fn selector(&self) -> Option<&str> {
        self.selector.as_deref()
    }

    /// An [`Api`] for `K` honouring this scope: the home namespace without a
    /// selector, cluster-wide (to be gated) with one.
    #[must_use]
    pub fn api<K>(&self, client: &Client) -> Api<K>
    where
        K: Resource<Scope = kube::core::NamespaceResourceScope>,
        K::DynamicType: Default,
    {
        match self.selector {
            Some(_) => Api::all(client.clone()),
            None => Api::namespaced(client.clone(), &self.home),
        }
    }

    /// Whether objects in `namespace` are served: home always, otherwise only
    /// what `gate` selects.
    ///
    /// Without a selector the gate is [`NamespaceGate::none`], which passes
    /// everything because the watch is already narrowed; this must not defer to
    /// it, or a provider with no selector would serve every namespace.
    #[must_use]
    pub fn serves(&self, gate: &NamespaceGate, namespace: &str) -> bool {
        if namespace == self.home {
            return true;
        }
        self.selector.is_some() && gate.serves(namespace)
    }

    /// `Some(action)` when a reconcile for `namespace` should stop: it is not
    /// served. An object being deleted is never skipped (ADR-0130 Decision 6).
    #[must_use]
    pub fn skip_unserved(
        &self,
        gate: &NamespaceGate,
        namespace: &str,
        deleting: bool,
    ) -> Option<Action> {
        (!deleting && !self.serves(gate, namespace)).then(Action::await_change)
    }

    /// One line for the startup log.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.selector {
            Some(selector) => format!(
                "home namespace {:?} plus {DESCRIBE_SELECTOR} {selector:?}",
                self.home
            ),
            None => format!("home namespace {:?} only", self.home),
        }
    }
}

/// What every provider needs to serve tenant namespaces without holding a
/// credential there (ADR-0133): the [`ProviderScope`] and the gate over its
/// namespace selector, with the three questions a machine reconciler asks.
///
/// One type, so a provider's `Context` delegates and the rules cannot drift
/// between backends. `Default` is "no scope installed", which serves everything
/// the watch delivers and reads from the serving namespace (today's behaviour
/// for a unit test or a legacy cluster-wide provider).
#[derive(Clone, Default)]
pub struct SharedServing {
    scope: Option<ProviderScope>,
    gate: NamespaceGate,
}

impl SharedServing {
    /// Serving with `scope` and the selector `gate`.
    #[must_use]
    pub fn new(scope: ProviderScope, gate: NamespaceGate) -> Self {
        Self {
            scope: Some(scope),
            gate,
        }
    }

    /// The namespace a provider reads its own `Provider`, credentials Secret and
    /// CA bundle from, whatever namespace a machine is in (Decision 5).
    ///
    /// The scope's home; failing that `serving_namespace` (`--namespace`); and
    /// only when neither exists (cluster-wide, a legacy shape) the machine's own
    /// namespace.
    #[must_use]
    pub fn home_namespace<'a>(
        &'a self,
        serving_namespace: Option<&'a str>,
        machine_namespace: &'a str,
    ) -> &'a str {
        if let Some(scope) = &self.scope {
            return scope.home();
        }
        serving_namespace.unwrap_or(machine_namespace)
    }

    /// Whether this provider reconciles a machine: its `providerRef` must
    /// resolve to the `Provider` this workload serves, in its home namespace
    /// (Decision 5). See [`crate::reconciler::serves_machine`].
    #[must_use]
    pub fn serves_machine(
        &self,
        provider_name: Option<&str>,
        serving_namespace: Option<&str>,
        machine_namespace: &str,
        provider_ref: &banlieue_api::common::ProviderReference,
    ) -> bool {
        crate::reconciler::serves_machine(
            provider_name,
            self.home_namespace(serving_namespace, machine_namespace),
            machine_namespace,
            provider_ref,
        )
    }

    /// `Some(action)` when a reconcile for `namespace` must do nothing because
    /// it is not served. An object being deleted is never skipped (ADR-0130
    /// Decision 6): its finalizer must still run.
    #[must_use]
    pub fn skip_unserved(&self, namespace: &str, deleting: bool) -> Option<Action> {
        self.scope
            .as_ref()?
            .skip_unserved(&self.gate, namespace, deleting)
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

/// Why [`start_gate`] failed. `Display` is the stage alone and the
/// [`ScopeError`] is the `source`, so wrapped in `anyhow` it reads exactly as
/// the `.context(..)` each binary used to attach by hand.
#[derive(Debug, thiserror::Error)]
pub enum GateError {
    /// The selector did not pass [`check_selector`]. `what` names the flag
    /// or field the selector came from.
    #[error("validating {what}")]
    Check {
        /// The flag or field the selector came from.
        what: &'static str,
        /// The probe's failure.
        #[source]
        source: ScopeError,
    },
    /// The selector-filtered `Namespace` watch did not start.
    #[error("starting the namespace selector watch")]
    Watch(#[source] ScopeError),
}

/// The receiving half of the "served namespaces changed" signal that
/// [`start_gate`] sets up.
///
/// Without a selector nothing ever sends, and a trigger stream must then stay
/// pending instead of ending. So this holds the otherwise unused sender, and
/// every stream from [`ScopeChanges::trigger`] holds it too: the caller has
/// nothing to keep alive by hand.
#[derive(Clone, Debug)]
pub struct ScopeChanges {
    rx: tokio::sync::watch::Receiver<u64>,
    idle: Option<std::sync::Arc<tokio::sync::watch::Sender<u64>>>,
}

impl ScopeChanges {
    /// A stream for `Controller::reconcile_all_on`: one `()` each time the
    /// served set of namespaces changes (see [`scope_trigger`]).
    pub fn trigger(&self) -> impl futures::Stream<Item = ()> + Send + use<> {
        let idle = self.idle.clone();
        scope_trigger(self.rx.clone()).inspect(move |()| {
            // Owned by the stream so the sender outlives it.
            let _ = &idle;
        })
    }

    /// The raw receiver, for code that already takes one.
    #[must_use]
    pub fn receiver(&self) -> &tokio::sync::watch::Receiver<u64> {
        &self.rx
    }
}

/// Set up namespace gating for a binary: the gate each reconcile asks, and
/// the change signal that reconciles everything again.
///
/// With a selector this optionally validates it ([`check_selector`]) and
/// starts the filtered `Namespace` reflector ([`spawn_namespace_gate`]).
/// Without one the watch is already narrowed to a single namespace, so the
/// gate passes everything and the signal never fires.
///
/// # Arguments
/// * `client` - the client the `Namespace` watch runs on.
/// * `selector` - the label selector, or `None` in single-namespace mode.
/// * `check_as` - when set, validate the selector first and name it this way
///   in the error (`"--watch-namespace-selector"`). `None` for a binary that
///   has already validated it.
///
/// # Errors
/// [`GateError::Check`] for a selector the API server rejects,
/// [`GateError::Watch`] when the watch ends before its first sync.
pub async fn start_gate(
    client: &Client,
    selector: Option<&str>,
    check_as: Option<&'static str>,
) -> Result<(NamespaceGate, ScopeChanges), GateError> {
    let (tx, rx) = tokio::sync::watch::channel(0_u64);
    let Some(selector) = selector else {
        let idle = Some(std::sync::Arc::new(tx));
        return Ok((NamespaceGate::none(), ScopeChanges { rx, idle }));
    };
    if let Some(what) = check_as {
        check_selector(client, selector)
            .await
            .map_err(|source| GateError::Check { what, source })?;
    }
    let gate = spawn_namespace_gate(client, selector, tx)
        .await
        .map_err(GateError::Watch)?;
    Ok((gate, ScopeChanges { rx, idle: None }))
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod scope_tests;
