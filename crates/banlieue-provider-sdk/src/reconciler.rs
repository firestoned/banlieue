// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Small helpers around [`kube::runtime::controller::Action`].
//!
//! Centralizing these constants keeps requeue semantics consistent across
//! controllers and makes it cheap to tune them globally.

use std::sync::Arc;
use std::time::Duration;

use banlieue_api::banlieue::Provider;
use kube::runtime::controller::Action;
use kube::runtime::watcher::Config;
use kube::{Api, Client, Resource, api::ListParams};
use tracing::warn;

/// Default requeue interval when reconciliation succeeded and the resource
/// is in a stable state. Matches Cluster API's default tick.
pub const REQUEUE_DEFAULT_SECS: u64 = 30;

/// Requeue interval after a transient failure — short enough that recovery
/// is fast, long enough to avoid hammering a degraded apiserver / backend.
pub const REQUEUE_ON_ERROR_SECS: u64 = 5;

/// Requeue interval for terminal-but-poll-y states (e.g. waiting for a long
/// backend operation like an OVF import).
pub const REQUEUE_LONG_SECS: u64 = 300;

/// Returns an [`Action`] requeuing after the default interval.
pub fn requeue_default() -> Action {
    Action::requeue(Duration::from_secs(REQUEUE_DEFAULT_SECS))
}

/// Returns an [`Action`] requeuing after the short error interval.
pub fn requeue_on_error() -> Action {
    Action::requeue(Duration::from_secs(REQUEUE_ON_ERROR_SECS))
}

/// Returns an [`Action`] requeuing after the long interval (slow polls).
pub fn requeue_long() -> Action {
    Action::requeue(Duration::from_secs(REQUEUE_LONG_SECS))
}

/// Returns an [`Action`] with no requeue — for terminal states.
pub fn no_requeue() -> Action {
    Action::await_change()
}

/// Whether a provider started as `provider_name` reconciles a machine whose
/// `spec.providerRef` names `machine_provider`.
///
/// The operator starts one workload per `Provider` with `--provider-name`,
/// and every workload watches every machine of its kind in the namespace.
/// A workload must leave other Providers' machines alone: it cannot reach
/// their credentials, and acting on them only produces errors. A provider
/// started without a name (a static install) serves every Provider.
#[must_use]
pub fn serves(provider_name: Option<&str>, machine_provider: &str) -> bool {
    provider_name.is_none_or(|mine| mine == machine_provider)
}

/// Whether a provider reconciles a machine, by name **and** namespace
/// (ADR-0133 Decision 5).
///
/// A provider serves machines in more than its home namespace, so the name
/// half of [`serves`] is no longer enough: the machine's `providerRef` must
/// resolve to a `Provider` *in the provider's home namespace*, where its
/// credentials live. A bare reference resolves in the machine's own namespace,
/// so a tenant machine that merely names the shared provider is not served: a
/// tenant must not be able to steer a machine onto a provider by name alone.
///
/// # Arguments
/// * `provider_name` - `--provider-name`; `None` for a static install, which
///   serves every `Provider` in its home namespace.
/// * `home_namespace` - the provider's home namespace.
/// * `machine_namespace` - the namespace the machine is in.
/// * `provider_ref` - the machine's `spec.providerRef`.
#[must_use]
pub fn serves_machine(
    provider_name: Option<&str>,
    home_namespace: &str,
    machine_namespace: &str,
    provider_ref: &banlieue_api::common::ProviderReference,
) -> bool {
    if provider_ref.namespace_or(machine_namespace) != home_namespace {
        return false;
    }
    serves(provider_name, &provider_ref.name)
}

/// The `Provider`s a workload reconciles shared rows (such as
/// `VMImage.status.perProvider`) for.
///
/// Started with `--provider-name` (one workload per `Provider`, as the
/// operator deploys), only that `Provider`: the workload can read that
/// `Provider`'s credentials and no other's. A static install (`None`) serves
/// every `Provider` it is given. ADR-0087, Amendment 2026-11-01.
#[must_use]
pub fn providers_in_scope(
    providers: Vec<banlieue_api::banlieue::Provider>,
    provider_name: Option<&str>,
) -> Vec<banlieue_api::banlieue::Provider> {
    providers
        .into_iter()
        .filter(|p| {
            serves(
                provider_name,
                p.metadata.name.as_deref().unwrap_or_default(),
            )
        })
        .collect()
}

/// Watch configuration for a `Provider` controller: narrowed server-side to
/// the one object when the workload was started with `--provider-name`.
///
/// A standalone function so the scoping rule is testable without a cluster.
#[must_use]
pub fn provider_watch_config(provider_name: Option<&str>) -> Config {
    match provider_name {
        Some(name) => Config::default().fields(&format!("metadata.name={name}")),
        None => Config::default(),
    }
}

/// Keep the `Provider`s whose `spec.providerClassRef.name` is `class`.
#[must_use]
pub fn providers_of_class(providers: Vec<Provider>, class: &str) -> Vec<Provider> {
    providers
        .into_iter()
        .filter(|p| p.spec.provider_class_ref.name == class)
        .collect()
}

/// List the `Provider`s of one class: in `namespace`, or in every namespace
/// when it is `None`.
///
/// No `--provider-name` scoping happens here; narrow the result with
/// [`providers_in_scope`] where the caller did so before.
///
/// # Errors
/// The LIST's [`kube::Error`].
pub async fn list_providers_of_class(
    client: &Client,
    namespace: Option<&str>,
    class: &str,
) -> std::result::Result<Vec<Provider>, kube::Error> {
    let api: Api<Provider> = match namespace {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };
    let listed = api.list(&ListParams::default()).await?;
    Ok(providers_of_class(listed.items, class))
}

/// The error policy every provider reconciler shares: log the failure with
/// the kind it was reconciling, and requeue on the error interval.
///
/// Pass it straight to `Controller::run`; the object and context are unused,
/// so it fits any reconciler's error and context types.
pub fn requeue_error_policy<K, E, C>(_object: Arc<K>, error: &E, _ctx: Arc<C>) -> Action
where
    K: Resource<DynamicType = ()>,
    E: std::fmt::Display,
{
    warn!(error = %error, kind = %K::kind(&()), "reconcile error policy fired");
    requeue_on_error()
}

#[cfg(test)]
#[path = "reconciler_tests.rs"]
mod reconciler_tests;
