// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context — the only value that all reconcilers receive.
//!
//! Keeping the [`kube::Client`] and any cached lister state here means the
//! reconcile functions stay testable with a synthesized `Context` and a fake
//! client.

use banlieue_api::banlieue::VirtualMachine;
use kube::Client;
use kube::runtime::{controller::Action, reflector::Store};

use crate::scope::NamespaceGate;

/// Context passed into every reconcile call.
#[derive(Clone)]
pub struct Context {
    /// Kubernetes API client.
    pub client: Client,
    /// Optional namespace scope — when `Some`, the controller watches only
    /// this namespace. When `None`, it watches cluster-wide.
    pub namespace: Option<String>,
    /// The `VirtualMachine` controller's reflector store, when running under
    /// it. Read by the duplicate-address check (ADR-0083) instead of listing
    /// every VM from the API on each reconcile. `None` outside the running
    /// controller (e.g. tests driving `reconcile` directly), where the check
    /// falls back to a LIST.
    pub vm_store: Option<Store<VirtualMachine>>,
    /// Whether `VMImage.spec.firstBootSealing` is honoured (ADR-0088
    /// Decision 7), from `--enable-first-boot-sealing`. On by default since
    /// ADR-0088 was accepted; when off, a `VirtualMachine` whose image
    /// declares it is refused.
    pub first_boot_sealing: bool,
    /// Which namespaces are served (ADR-0130). The default passes everything
    /// the watch delivers, which is right whenever the watch is already
    /// narrowed to one namespace; selector mode installs a gate over the
    /// label-filtered `Namespace` reflector.
    pub namespace_gate: NamespaceGate,
}

impl Context {
    /// Construct a new [`Context`].
    pub fn new(client: Client, namespace: Option<String>) -> Self {
        Self {
            client,
            namespace,
            vm_store: None,
            first_boot_sealing: true,
            namespace_gate: NamespaceGate::none(),
        }
    }

    /// Attach the `VirtualMachine` reflector store (ADR-0083).
    #[must_use]
    pub fn with_vm_store(mut self, store: Store<VirtualMachine>) -> Self {
        self.vm_store = Some(store);
        self
    }

    /// Install the namespace gate for selector mode (ADR-0130).
    #[must_use]
    pub fn with_namespace_gate(mut self, gate: NamespaceGate) -> Self {
        self.namespace_gate = gate;
        self
    }

    /// Whether objects in `namespace` are served.
    #[must_use]
    pub fn serves(&self, namespace: &str) -> bool {
        self.namespace_gate.serves(namespace)
    }

    /// The early return every reconciler starts with: `Some(action)` when
    /// `namespace` is not served, and the reconciler must do nothing and
    /// write nothing. `await_change` because a namespace joining the
    /// selection reconciles everything again (ADR-0130), so no timer is
    /// needed and an un-served object is not polled.
    ///
    /// An object being deleted is never skipped (ADR-0130 decision 6): its
    /// finalizer must still run, or un-labelling a namespace would strand it.
    #[must_use]
    pub fn skip_unserved(&self, namespace: &str, deleting: bool) -> Option<Action> {
        (!deleting && !self.serves(namespace)).then(Action::await_change)
    }

    /// Honour, or refuse, `VMImage.spec.firstBootSealing` (ADR-0088), from
    /// `--enable-first-boot-sealing`.
    #[must_use]
    pub fn with_first_boot_sealing(mut self, enabled: bool) -> Self {
        self.first_boot_sealing = enabled;
        self
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod context_tests;
