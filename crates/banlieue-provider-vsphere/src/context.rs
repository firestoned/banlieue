// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context for the vSphere provider.
//!
//! Carries the [`kube::Client`] plus a `Box<dyn VSphereClientFactory>` so
//! reconciler tests can inject a [`FakeClientFactory`] without touching
//! `vim_rs` or vCenter. It also carries the per-zone image-import Job settings
//! (ADR-0010 / ADR-0020), mirroring the libvirt provider's `Context`.

use std::sync::Arc;

use banlieue_api::common::ProviderReference;
use banlieue_provider_sdk::scope::{NamespaceGate, ProviderScope, SharedServing};
use k8s_openapi::api::core::v1::Toleration;
use kube::Client;
use kube::runtime::controller::Action;

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

    /// Namespace holding the artifacts PVC and the per-zone import Jobs.
    ///
    /// Must match `banlieue-imagebuilder`'s `--build-namespace`: the Job
    /// mounts the PVC kairos-operator created there, and a PVC cannot be
    /// mounted across namespaces (ADR-0010 / ADR-0020).
    pub build_namespace: String,

    /// Container image the import Job runs — the banlieue image itself, so the
    /// data path stays inside banlieue's own supply chain.
    pub import_image: String,

    /// ServiceAccount the import Job runs as, in [`Context::build_namespace`].
    ///
    /// A dedicated read-only identity, never this controller's own: that one
    /// can create Jobs, so a workload in the privileged build namespace
    /// holding it could create further privileged pods (ADR-0016 §4).
    pub import_service_account: String,

    /// Taints import Jobs may tolerate.
    ///
    /// Not a node selector: where an import Job runs follows from the artifacts
    /// PVC it mounts, and the scheduler already honours the bound PV's
    /// constraints. These only grant permission to land on a tainted node,
    /// which matters when the volume sits on a dedicated build node.
    pub import_tolerations: Vec<Toleration>,

    /// The `Provider` this workload serves (`--provider-name`). `None` for a
    /// static install, which serves every Provider of its class.
    pub provider_name: Option<String>,

    /// What this provider serves (ADR-0133): its home namespace plus, with a
    /// selector, the namespaces whose labels match. The default, outside the
    /// running provider (tests), serves everything the watch delivers and reads
    /// from [`Context::namespace`].
    serving: SharedServing,
}

impl Context {
    /// Construct a new [`Context`].
    pub fn new(
        client: Client,
        namespace: Option<String>,
        vsphere: Arc<dyn VSphereClientFactory>,
        build_namespace: String,
        import_image: String,
        import_service_account: String,
        import_tolerations: Vec<Toleration>,
    ) -> Self {
        Self {
            client,
            namespace,
            vsphere,
            build_namespace,
            import_image,
            import_service_account,
            import_tolerations,
            provider_name: None,
            serving: SharedServing::default(),
        }
    }

    /// This context, serving only the `Provider` named `provider_name`
    /// (see [`banlieue_provider_sdk::reconciler::serves`]).
    #[must_use]
    pub fn with_provider_name(mut self, provider_name: Option<String>) -> Self {
        self.provider_name = provider_name;
        self
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
    /// Decision 5). That is what keeps hypervisor credentials out of tenant
    /// namespaces.
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
