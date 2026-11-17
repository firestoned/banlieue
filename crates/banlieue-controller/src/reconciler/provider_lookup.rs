// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Which `Provider`s a `VirtualMachine` may be scheduled onto (ADR-0133).
//!
//! The lookup is the VM's own namespace first, then the provider namespace:
//!
//! 1. If the VM's namespace holds **any** `Provider`, those are the whole
//!    candidate set. A namespace that runs its own `Provider` has chosen its own
//!    backend, so a selector that matches nothing there fails rather than
//!    silently landing on the shared platform (fail closed).
//! 2. Otherwise, and only when `--provider-namespace` is set, the candidates are
//!    the `Provider`s in that namespace that **declare** the VM's namespace: the
//!    `Provider`'s `spec.namespaceSelector` matches the namespace's labels. A
//!    `Provider` with no selector serves only its own namespace and is never
//!    anyone's fallback.
//!
//! These functions are pure: the reconciler does the LISTs and the Namespace
//! GET, and hands the results in, so every rule is testable without a cluster.

use std::collections::BTreeMap;

use banlieue_api::banlieue::{Provider, VirtualMachine};
use k8s_openapi::api::core::v1::Namespace;
use kube::api::ListParams;
use kube::{Api, ResourceExt};
use tracing::{debug, info};

use super::scheduler::selector_matches;
use crate::context::Context;
use crate::error::Result;

/// Where the candidate `Provider`s came from, for logs and the machine's
/// `providerRef`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateSource {
    /// The VM's own namespace.
    VmNamespace,
    /// The provider namespace (a shared provider).
    ProviderNamespace,
    /// Nowhere: nothing to schedule onto.
    None,
}

/// The `Provider`s a VM may be scheduled onto, and where they came from.
#[derive(Clone, Debug)]
pub struct Candidates {
    /// The candidate set handed to the scheduler.
    pub providers: Vec<Provider>,
    /// Which namespace they were found in.
    pub source: CandidateSource,
}

/// Whether a provider class resolves its `Provider` from its own namespace, and
/// so can serve a machine whose `providerRef` names another namespace.
///
/// The list is `banlieue_provider_sdk::scope::SHARED_PROVIDER_BACKENDS`, shared
/// with the operator (ADR-0133 Decision 10): offering a foreign reference to a
/// class that cannot honour it would schedule a machine nothing will ever
/// reconcile.
#[must_use]
pub fn supports_shared_provider(class: &str) -> bool {
    banlieue_provider_sdk::scope::supports_shared_provider(class)
}

/// Whether the reconciler should look in the provider namespace at all.
///
/// # Arguments
/// * `local_provider_count` - how many `Provider`s the VM's own namespace holds.
/// * `provider_namespace` - `--provider-namespace`; unset or blank means no fallback.
/// * `vm_namespace` - the VM's namespace.
#[must_use]
pub fn should_try_provider_namespace(
    local_provider_count: usize,
    provider_namespace: Option<&str>,
    vm_namespace: &str,
) -> bool {
    if local_provider_count > 0 {
        return false;
    }
    let Some(provider_namespace) = provider_namespace.map(str::trim).filter(|n| !n.is_empty())
    else {
        return false;
    };
    provider_namespace != vm_namespace
}

/// Choose the candidate set (ADR-0133 Decisions 1 and 11).
///
/// # Arguments
/// * `local` - the `Provider`s in the VM's namespace.
/// * `platform` - the `Provider`s in the provider namespace; empty when no
///   fallback applies.
/// * `namespace_labels` - the labels of the VM's namespace.
#[must_use]
pub fn select_candidates(
    local: Vec<Provider>,
    platform: Vec<Provider>,
    namespace_labels: &BTreeMap<String, String>,
) -> Candidates {
    if !local.is_empty() {
        return Candidates {
            providers: local,
            source: CandidateSource::VmNamespace,
        };
    }
    let providers: Vec<Provider> = platform
        .into_iter()
        .filter(|p| supports_shared_provider(&p.spec.provider_class_ref.name))
        .filter(|p| serves_namespace(p, namespace_labels))
        .collect();
    let source = if providers.is_empty() {
        CandidateSource::None
    } else {
        CandidateSource::ProviderNamespace
    };
    Candidates { providers, source }
}

/// Whether `provider` declares a namespace carrying `namespace_labels` in its
/// `spec.namespaceSelector`.
///
/// No selector means "my own namespace only", and an empty one serves nothing
/// (it cannot be rendered onto the provider as a flag, and banlieue never
/// defaults to every namespace), so neither is ever a fallback.
fn serves_namespace(provider: &Provider, namespace_labels: &BTreeMap<String, String>) -> bool {
    let Some(selector) = provider.spec.namespace_selector.as_ref() else {
        return false;
    };
    !selector.is_empty() && selector_matches(&Some(selector.clone()), namespace_labels)
}

/// Keep the VMs whose namespace `serves` accepts.
///
/// A shared provider's load is every tenant's VMs placed on it, so the peers
/// the scheduler counts are listed cluster-wide and narrowed here to the
/// namespaces this controller serves. A VM with no namespace is dropped.
#[must_use]
pub fn retain_served(
    vms: Vec<VirtualMachine>,
    serves: impl Fn(&str) -> bool,
) -> Vec<VirtualMachine> {
    vms.into_iter()
        .filter(|vm| vm.namespace().is_some_and(|ns| serves(&ns)))
        .collect()
}

/// The candidate `Provider`s for a VM in `namespace` (ADR-0133 Decisions 1, 2, 11).
///
/// Lists the VM's own namespace; only when it holds none and
/// `--provider-namespace` is set does it also list that namespace and read the
/// VM namespace's labels, so a namespace with its own `Provider` costs one
/// LIST, as before.
///
/// # Errors
/// Any Kubernetes API error from the lists or the `Namespace` get.
pub async fn candidate_providers(ctx: &Context, namespace: &str) -> Result<Candidates> {
    let local: Vec<Provider> = Api::namespaced(ctx.client.clone(), namespace)
        .list(&ListParams::default())
        .await?
        .items;
    let provider_namespace = ctx.provider_namespace();
    if !should_try_provider_namespace(local.len(), provider_namespace, namespace) {
        return Ok(select_candidates(local, Vec::new(), &BTreeMap::new()));
    }
    // `should_try_provider_namespace` is true only for a set namespace.
    let Some(provider_namespace) = provider_namespace else {
        return Ok(select_candidates(local, Vec::new(), &BTreeMap::new()));
    };
    let platform: Vec<Provider> = Api::namespaced(ctx.client.clone(), provider_namespace)
        .list(&ListParams::default())
        .await?
        .items;
    let labels = namespace_labels(ctx, namespace).await?;
    let candidates = select_candidates(local, platform, &labels);
    info!(
        namespace,
        provider_namespace,
        source = ?candidates.source,
        candidates = candidates.providers.len(),
        "VM namespace holds no Provider; looked in the provider namespace"
    );
    Ok(candidates)
}

/// The labels of `namespace`, read with a `get` on `Namespace` (read-only).
async fn namespace_labels(ctx: &Context, namespace: &str) -> Result<BTreeMap<String, String>> {
    let api: Api<Namespace> = Api::all(ctx.client.clone());
    let ns = api.get(namespace).await?;
    Ok(ns.metadata.labels.unwrap_or_default())
}

/// The VMs whose placement counts against the chosen providers.
///
/// With the VM's own namespace's `Provider`s that is the namespace's VMs, as
/// ever. With a shared provider it is every served namespace's VMs, because
/// the provider's load is all of its tenants'. A VM elsewhere that names a
/// same-named `Provider` of its own is counted too, which can only
/// over-count: the safe direction for a capacity check.
///
/// # Errors
/// Any Kubernetes API error from the list.
pub async fn placement_peers(
    ctx: &Context,
    namespace: &str,
    source: CandidateSource,
) -> Result<Vec<VirtualMachine>> {
    if source != CandidateSource::ProviderNamespace {
        return Ok(Api::namespaced(ctx.client.clone(), namespace)
            .list(&ListParams::default())
            .await?
            .items);
    }
    let all: Vec<VirtualMachine> = Api::all(ctx.client.clone())
        .list(&ListParams::default())
        .await?
        .items;
    let peers = retain_served(all, |ns| ctx.serves(ns));
    debug!(
        peers = peers.len(),
        "counted peers across served namespaces"
    );
    Ok(peers)
}

#[cfg(test)]
#[path = "provider_lookup_tests.rs"]
mod provider_lookup_tests;
