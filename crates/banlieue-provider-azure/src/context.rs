// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context for the Azure provider.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use banlieue_api::common::ProviderReference;
use banlieue_provider_sdk::scope::{NamespaceGate, ProviderScope, SharedServing};
use k8s_openapi::api::core::v1::Toleration;
use kube::Client;
use kube::runtime::controller::Action;

use crate::client::ArmClientFactory;

/// When each `Provider`'s orphan sweep last ran (ADR-0158 Decision 5).
///
/// Only a rate limiter: the `Provider` reconcile also runs on its own status
/// changes, and the sweep must keep its slow cadence. Losing it on restart
/// costs one early sweep, which the grace period already makes safe.
#[derive(Debug, Default)]
pub struct SweepClock {
    last: Mutex<BTreeMap<String, Instant>>,
}

impl SweepClock {
    /// Whether the sweep for `key` is due at `now` with `interval` between
    /// passes; records `now` when it is, so concurrent callers run it once.
    pub fn due(&self, key: &str, now: Instant, interval: Duration) -> bool {
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = last.get(key)
            && now.saturating_duration_since(*previous) < interval
        {
            return false;
        }
        last.insert(key.to_string(), now);
        true
    }
}

/// Where and how the image publish Jobs run (ADR-0162 Decision 1).
#[derive(Clone, Debug, PartialEq)]
pub struct PublishSettings {
    /// The imagebuild namespace: the artifacts PVC and the Jobs live there
    /// (ADR-0016). Must match banlieue-imagebuilder's `--build-namespace`.
    pub build_namespace: String,
    /// The image the Jobs run: banlieue's own.
    pub image: String,
    /// The ServiceAccount the Jobs run as, in `build_namespace`.
    pub service_account: String,
    /// Taints the Jobs may tolerate.
    pub tolerations: Vec<Toleration>,
}

impl Default for PublishSettings {
    fn default() -> Self {
        Self {
            build_namespace: banlieue_provider_sdk::defaults::DEFAULT_BUILD_NAMESPACE.to_string(),
            image: banlieue_provider_sdk::defaults::DEFAULT_IMPORT_IMAGE.to_string(),
            service_account: crate::reconciler::publish_job::DEFAULT_PUBLISHER_SERVICE_ACCOUNT
                .to_string(),
            tolerations: Vec::new(),
        }
    }
}

/// Context passed into every reconcile call.
#[derive(Clone)]
pub struct Context {
    /// Kubernetes API client.
    pub client: Client,
    /// Optional namespace scope: `Some` for single-namespace, `None` for
    /// cluster-wide watches.
    pub namespace: Option<String>,
    /// Builds an ARM client from a Provider's connection. Held as
    /// `Arc<dyn ...>` so reconciles clone it cheaply and tests can inject
    /// the in-memory fake.
    pub arm: Arc<dyn ArmClientFactory>,
    /// When set, this process serves only the named `Provider` (the operator
    /// runs one process per Provider).
    pub provider_name: Option<String>,
    /// The orphan sweep's cadence, per `Provider`.
    pub sweep_clock: Arc<SweepClock>,
    /// The image publish Jobs' namespace, image and identity.
    pub publish: PublishSettings,
    /// What this provider serves (ADR-0133).
    serving: SharedServing,
    /// The same scope, kept to list every `AzureVirtualMachine` this
    /// provider can see before the orphan sweep decides anything is gone.
    scope: Option<ProviderScope>,
}

impl Context {
    /// Construct a new [`Context`].
    pub fn new(
        client: Client,
        namespace: Option<String>,
        arm: Arc<dyn ArmClientFactory>,
        provider_name: Option<String>,
    ) -> Self {
        Self {
            client,
            namespace,
            arm,
            provider_name,
            sweep_clock: Arc::new(SweepClock::default()),
            publish: PublishSettings::default(),
            serving: SharedServing::default(),
            scope: None,
        }
    }

    /// Install the provider's scope and the gate over its namespace selector
    /// (ADR-0133 Decision 4).
    #[must_use]
    pub fn with_scope(mut self, scope: ProviderScope, gate: NamespaceGate) -> Self {
        self.scope = Some(scope.clone());
        self.serving = SharedServing::new(scope, gate);
        self
    }

    /// Install the publish Jobs' settings.
    #[must_use]
    pub fn with_publishing(mut self, publish: PublishSettings) -> Self {
        self.publish = publish;
        self
    }

    /// The scope installed by [`Context::with_scope`], if any.
    #[must_use]
    pub fn scope(&self) -> Option<&ProviderScope> {
        self.scope.as_ref()
    }

    /// The namespace this provider reads its own `Provider` from, whatever
    /// namespace a machine is in (ADR-0133 Decision 5).
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
