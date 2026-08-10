// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context for the vSphere provider.
//!
//! Carries the [`kube::Client`] plus a `Box<dyn VSphereClientFactory>` so
//! reconciler tests can inject a [`FakeClientFactory`] without touching
//! `vim_rs` or vCenter. It also carries the per-zone image-import Job settings
//! (ADR-0010 / ADR-0020), mirroring the libvirt provider's `Context`.

use std::sync::Arc;

use k8s_openapi::api::core::v1::Toleration;
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
        }
    }
}
