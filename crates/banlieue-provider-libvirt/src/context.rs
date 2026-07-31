// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context for the libvirt provider.

use std::sync::Arc;

use k8s_openapi::api::core::v1::Toleration;
use kube::Client;

use crate::client::LibvirtClientFactory;

/// Context passed into every reconcile call.
#[derive(Clone)]
pub struct Context {
    /// Kubernetes API client.
    pub client: Client,
    /// Optional namespace scope — `Some` for single-namespace, `None` for
    /// cluster-wide watches.
    pub namespace: Option<String>,
    /// Builds a [`crate::client::LibvirtClient`] from a Provider's connection
    /// details. Held as `Arc<dyn ...>` so reconciles clone it cheaply and
    /// tests can inject a fake.
    pub libvirt: Arc<dyn LibvirtClientFactory>,

    /// Namespace holding the artifacts PVC and the import Jobs.
    ///
    /// Must match `banlieue-imagebuilder`'s `--build-namespace`: the Job
    /// mounts the PVC kairos-operator created there, and a PVC cannot be
    /// mounted across namespaces (ADR-0010).
    pub build_namespace: String,

    /// Container image the import Job runs — the banlieue image itself, so the
    /// data path stays inside banlieue's own supply chain (ADR-0011).
    pub import_image: String,

    /// ServiceAccount the import Job runs as, in [`Context::build_namespace`].
    ///
    /// A dedicated read-only identity, never this controller's own: that one
    /// can create Jobs, so a workload in the privileged build namespace
    /// holding it could create further privileged pods (ADR-0016 §4).
    pub import_service_account: String,

    /// Taints import Jobs may tolerate.
    ///
    /// Deliberately not a node selector: where an import Job runs follows from
    /// the artifacts PVC it mounts, and the scheduler already honours the
    /// bound PV's constraints. These only grant permission to land on a
    /// tainted node, which matters when the volume sits on a dedicated build
    /// node.
    pub import_tolerations: Vec<Toleration>,
}

impl Context {
    /// Construct a new [`Context`].
    pub fn new(
        client: Client,
        namespace: Option<String>,
        libvirt: Arc<dyn LibvirtClientFactory>,
        build_namespace: String,
        import_image: String,
        import_service_account: String,
        import_tolerations: Vec<Toleration>,
    ) -> Self {
        Self {
            client,
            namespace,
            libvirt,
            build_namespace,
            import_image,
            import_service_account,
            import_tolerations,
        }
    }
}
