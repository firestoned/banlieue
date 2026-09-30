// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context for the Proxmox provider.

use std::sync::Arc;

use kube::Client;

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
        }
    }
}
