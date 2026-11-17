// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context for the Proxmox provider.

use std::sync::Arc;

use banlieue_api::common::ProviderReference;
use banlieue_provider_sdk::scope::{NamespaceGate, ProviderScope, SharedServing};
use kube::Client;
use kube::runtime::controller::Action;

use crate::client::ProxmoxClientFactory;

/// Context passed into every reconcile call.
#[derive(Clone)]
pub struct Context {
    /// Kubernetes API client.
    pub client: Client,
    /// Optional namespace scope: `Some` for single-namespace, `None` for
    /// cluster-wide watches.
    pub namespace: Option<String>,
    /// Builds a Proxmox API client from a Provider's connection details. Held
    /// as `Arc<dyn ...>` so reconciles clone it cheaply and tests can inject
    /// the in-memory fake.
    pub proxmox: Arc<dyn ProxmoxClientFactory>,
    /// When set, this process serves only the named \`Provider\` (the operator
    /// runs one process per Provider). Applies to the \`VMImage\` reconciler,
    /// which lists Providers; the \`Provider\` watch is narrowed server-side.
    pub provider_name: Option<String>,
    /// What this provider serves (ADR-0133): its home namespace plus, with a
    /// selector, the namespaces whose labels match. The default (tests) serves
    /// everything the watch delivers and reads from [`Context::namespace`].
    serving: SharedServing,
}

impl Context {
    /// Construct a new [`Context`].
    pub fn new(
        client: Client,
        namespace: Option<String>,
        proxmox: Arc<dyn ProxmoxClientFactory>,
        provider_name: Option<String>,
    ) -> Self {
        Self {
            client,
            namespace,
            proxmox,
            provider_name,
            serving: SharedServing::default(),
        }
    }

    /// Install the provider's scope and the gate over its namespace selector
    /// (ADR-0133 Decision 4).
    #[must_use]
    pub fn with_scope(mut self, scope: ProviderScope, gate: NamespaceGate) -> Self {
        self.serving = SharedServing::new(scope, gate);
        self
    }

    /// The namespace this provider reads its own `Provider`, credentials Secret
    /// and CA bundle from, whatever namespace a machine is in (ADR-0133
    /// Decision 5). That keeps hypervisor credentials out of tenant namespaces.
    #[must_use]
    pub fn home_namespace<'a>(&'a self, machine_namespace: &'a str) -> &'a str {
        self.serving
            .home_namespace(self.namespace.as_deref(), machine_namespace)
    }

    /// Whether this provider reconciles a machine in `machine_namespace` with
    /// this `providerRef` (ADR-0133 Decision 5).
    #[must_use]
    pub fn serves_machine(
        &self,
        machine_namespace: &str,
        provider_ref: &ProviderReference,
    ) -> bool {
        self.serving.serves_machine(
            self.provider_name.as_deref(),
            self.namespace.as_deref(),
            machine_namespace,
            provider_ref,
        )
    }

    /// `Some(action)` when a reconcile for `namespace` must do nothing because
    /// the namespace is not served; a deleting object is never skipped.
    #[must_use]
    pub fn skip_unserved(&self, namespace: &str, deleting: bool) -> Option<Action> {
        self.serving.skip_unserved(namespace, deleting)
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod context_tests;
