// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context for the EC2 provider.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use banlieue_api::common::ProviderReference;
use banlieue_provider_sdk::scope::{NamespaceGate, ProviderScope, SharedServing};
use kube::Client;
use kube::runtime::controller::Action;

use crate::client::Ec2ClientFactory;
use crate::metrics::SweepMetrics;

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

/// Context passed into every reconcile call.
#[derive(Clone)]
pub struct Context {
    /// Kubernetes API client.
    pub client: Client,
    /// Optional namespace scope: `Some` for single-namespace, `None` for
    /// cluster-wide watches.
    pub namespace: Option<String>,
    /// Builds an EC2 client from a `Provider`. Held as `Arc<dyn ...>` so
    /// reconciles clone it cheaply and tests can inject the in-memory fake.
    pub ec2: Arc<dyn Ec2ClientFactory>,
    /// When set, this process serves only the named `Provider` (the operator
    /// runs one process per Provider).
    pub provider_name: Option<String>,
    /// The orphan sweep's cadence, per `Provider`.
    pub sweep_clock: Arc<SweepClock>,
    /// The orphan sweep's counters; registered at startup, unregistered
    /// (recording into nothing scraped) in tests.
    pub sweep_metrics: SweepMetrics,
    /// What this provider serves (ADR-0133).
    serving: SharedServing,
    /// The same scope, kept to list every `EC2Machine` this provider can see
    /// before the orphan sweep decides anything is gone.
    scope: Option<ProviderScope>,
}

impl Context {
    /// Construct a new [`Context`].
    pub fn new(
        client: Client,
        namespace: Option<String>,
        ec2: Arc<dyn Ec2ClientFactory>,
        provider_name: Option<String>,
    ) -> Self {
        Self {
            client,
            namespace,
            ec2,
            provider_name,
            sweep_clock: Arc::new(SweepClock::default()),
            sweep_metrics: SweepMetrics::default(),
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

    /// Install the orphan sweep's counters, already registered with the
    /// process's metrics registry.
    #[must_use]
    pub fn with_sweep_metrics(mut self, sweep_metrics: SweepMetrics) -> Self {
        self.sweep_metrics = sweep_metrics;
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
