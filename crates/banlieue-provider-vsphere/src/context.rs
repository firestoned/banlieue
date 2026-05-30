// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context for the vSphere provider.
//!
//! Carries the [`kube::Client`] plus a `Box<dyn VSphereClientFactory>` so
//! reconciler tests can inject a [`FakeClientFactory`] without touching
//! `vim_rs` or vCenter.

use std::sync::Arc;

use kube::Client;

use crate::client::VSphereClientFactory;

/// Context passed into every reconcile call.
#[derive(Clone)]
pub struct Context {
    /// Kubernetes API client.
    pub client: Client,
    /// Optional namespace scope — `Some` for single-namespace, `None` for
    /// cluster-wide watches.
    pub namespace: Option<String>,
    /// Factory that builds a [`crate::client::VSphereClient`] from a
    /// [`banlieue_api::banlieue::ProviderConnection`] + credentials Secret.
    /// Held as `Arc<dyn ...>` so the controller can clone it cheaply across
    /// many concurrent reconciles.
    pub vsphere: Arc<dyn VSphereClientFactory>,
}

impl Context {
    /// Construct a new [`Context`].
    pub fn new(
        client: Client,
        namespace: Option<String>,
        vsphere: Arc<dyn VSphereClientFactory>,
    ) -> Self {
        Self {
            client,
            namespace,
            vsphere,
        }
    }
}
