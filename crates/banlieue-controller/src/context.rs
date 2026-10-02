// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context — the only value that all reconcilers receive.
//!
//! Keeping the [`kube::Client`] and any cached lister state here means the
//! reconcile functions stay testable with a synthesized `Context` and a fake
//! client.

use banlieue_api::banlieue::VirtualMachine;
use kube::Client;
use kube::runtime::reflector::Store;

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
}

impl Context {
    /// Construct a new [`Context`].
    pub fn new(client: Client, namespace: Option<String>) -> Self {
        Self {
            client,
            namespace,
            vm_store: None,
        }
    }

    /// Attach the `VirtualMachine` reflector store (ADR-0083).
    #[must_use]
    pub fn with_vm_store(mut self, store: Store<VirtualMachine>) -> Self {
        self.vm_store = Some(store);
        self
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod context_tests;
