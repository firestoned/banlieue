// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context for `banlieue-operator`.

use kube::Client;
use kube::runtime::events::{Recorder, Reporter};

/// Identifies this controller as the source of the Events it publishes, so
/// `kubectl describe` attributes them and two controllers' events stay
/// distinguishable.
pub const EVENT_REPORTER: &str = "banlieue.io/operator";

/// Context passed into every reconcile call.
#[derive(Clone)]
pub struct Context {
    /// Kubernetes API client.
    pub client: Client,

    /// Publishes Events against the objects this operator reconciles.
    ///
    /// Conditions say what state a resource is in; events say what the
    /// controller did and when. Without them a Provider that never comes up
    /// gives an operator nothing to look at short of controller logs.
    pub recorder: Recorder,

    /// Namespace the operator itself runs in.
    ///
    /// Used only as a reporting default; provider workloads land in their
    /// Provider's namespace unless the `ProviderClass` pins
    /// `workloadNamespace` (ADR-0012).
    pub namespace: String,

    /// Namespace image-import Jobs run in — the privileged build namespace of
    /// ADR-0016. Used only as the subject namespace of each Provider's import
    /// RoleBinding; the operator creates nothing there itself.
    pub imagebuild_namespace: String,

    /// `--build-node-selector` values to forward to every provider workload,
    /// so its import Jobs can be scheduled where the artifacts PV is
    /// (ADR-0016 follow-up). Forwarded verbatim; the provider parses them.
    pub build_node_selector: Vec<String>,
    /// `--build-toleration` values, forwarded for the same reason.
    pub build_toleration: Vec<String>,
}

impl Context {
    /// Construct a new [`Context`].
    ///
    /// The reporter's `instance` is the pod name when the downward API supplied
    /// one, so events from two operator replicas remain attributable.
    #[must_use]
    pub fn new(client: Client, namespace: String) -> Self {
        Self::with_imagebuild_namespace(
            client,
            namespace,
            crate::bootstrap::DEFAULT_IMAGEBUILD_NAMESPACE.to_string(),
        )
    }

    /// Construct with an explicit build namespace.
    #[must_use]
    pub fn with_imagebuild_namespace(
        client: Client,
        namespace: String,
        imagebuild_namespace: String,
    ) -> Self {
        Self::with_build_scheduling(
            client,
            namespace,
            imagebuild_namespace,
            Vec::new(),
            Vec::new(),
        )
    }

    /// Construct with build-scheduling flags to forward to provider workloads.
    #[must_use]
    pub fn with_build_scheduling(
        client: Client,
        namespace: String,
        imagebuild_namespace: String,
        build_node_selector: Vec<String>,
        build_toleration: Vec<String>,
    ) -> Self {
        let reporter = Reporter {
            controller: EVENT_REPORTER.to_string(),
            instance: std::env::var("POD_NAME").ok(),
        };
        let recorder = Recorder::new(client.clone(), reporter);
        Self {
            client,
            recorder,
            namespace,
            imagebuild_namespace,
            build_node_selector,
            build_toleration,
        }
    }
}
