// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context — the only value that all reconcilers receive.
//!
//! Keeping the [`kube::Client`] and any cached lister state here means the
//! reconcile functions stay testable with a synthesized `Context` and a fake
//! client.

use kube::Client;

/// Context passed into every reconcile call.
#[derive(Clone)]
pub struct Context {
    /// Kubernetes API client.
    pub client: Client,
    /// Optional namespace scope — when `Some`, the controller watches only
    /// this namespace. When `None`, it watches cluster-wide.
    pub namespace: Option<String>,
}

impl Context {
    /// Construct a new [`Context`].
    pub fn new(client: Client, namespace: Option<String>) -> Self {
        Self { client, namespace }
    }
}
