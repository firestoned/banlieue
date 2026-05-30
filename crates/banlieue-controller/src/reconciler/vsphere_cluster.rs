// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VSphereCluster` reconciler — CAPI InfraCluster failure-domain aggregation.
//!
//! Implements ADR-0002: the main controller reconciles `VSphereCluster`
//! (banlieue's CAPI v1beta2 InfraCluster) by aggregating the failure domains
//! published by one or more `Provider`s into the CAPI `status.failureDomains`
//! list. CAPI core + a control-plane provider (k0smotron) then balance machines
//! across those domains.
//!
//! This reconciler talks to **no backend** — it reads `Provider.status`
//! (which the provider's controller populated from vCenter) and republishes it
//! in CAPI shape. That preserves the CRD-only boundary (non-negotiable #1).
//! Capacity gating is the provider's job: a `Provider` simply omits a full
//! failure domain from its status, so it never reaches the aggregation here.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use banlieue_api::banlieue::Provider;
use banlieue_api::common::{ClusterFailureDomain, LabelSelector, condition_types};
use banlieue_api::infrastructure::{VSphereCluster, VSphereClusterSpec, VSphereClusterStatus};
use banlieue_provider_sdk::reconciler::{requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_CONTROLLER;
use banlieue_provider_sdk::status::{condition_status, set_condition};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{
    Resource, ResourceExt,
    api::{Api, ListParams, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{info, warn};

use crate::context::Context;
use crate::error::{Error, Result};

use super::scheduler::selector_matches;

/// Condition type (in addition to `Ready` from `condition_types`) reflecting
/// the in-band pause state. Mirrors the CAPI `Paused` condition.
const CONDITION_PAUSED: &str = "Paused";

/// Stable `reason` strings on `VSphereCluster.status.conditions`. Operators and
/// tests match against these — keep them stable.
mod reasons {
    pub const RECONCILED: &str = "Reconciled";
    pub const NO_FAILURE_DOMAINS: &str = "NoFailureDomains";
    pub const PAUSED: &str = "Paused";
}

/// Top-level reconcile entrypoint registered with [`kube::runtime::Controller`].
pub async fn reconcile(vsc: Arc<VSphereCluster>, ctx: Arc<Context>) -> Result<Action> {
    let namespace = vsc.namespace().ok_or(Error::Missing("namespace"))?;
    let name = vsc.name_any();
    let generation = vsc.metadata.generation.unwrap_or(0);

    let span = tracing::info_span!(
        "reconcile",
        kind = "VSphereCluster",
        namespace = %namespace,
        name = %name,
        generation,
    );
    let _enter = span.enter();

    if vsc.spec.paused {
        info!("VSphereCluster is paused — skipping reconciliation");
        patch_paused(&ctx, &namespace, &name, generation).await?;
        return Ok(requeue_long());
    }

    info!("reconciling VSphereCluster");

    // Read every Provider in the namespace; `build_status` selects the relevant
    // subset. Provider status is the only input — no backend access.
    let provider_api: Api<Provider> = Api::namespaced(ctx.client.clone(), &namespace);
    let all_providers = provider_api.list(&ListParams::default()).await?.items;

    let status = build_status(&vsc.spec, &all_providers, generation);
    let fd_count = status.failure_domains.len();

    patch_status(&ctx, &namespace, &name, status).await?;
    info!(fd_count, "VSphereCluster reconciled");

    // Failure domains change rarely (operator-driven Provider changes). The
    // Provider watch wired in `main.rs` triggers immediate reconciles; this
    // long requeue is only a backstop.
    Ok(requeue_long())
}

/// `error_policy` callback for the controller. Short backoff — most errors are
/// transient API blips.
pub fn error_policy(_vsc: Arc<VSphereCluster>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "VSphereCluster reconcile error policy fired");
    requeue_on_error()
}

/// Build the full [`VSphereClusterStatus`] from the spec and the set of all
/// `Provider`s in scope. Pure — the unit-tested core of the reconciler.
pub fn build_status(
    spec: &VSphereClusterSpec,
    all_providers: &[Provider],
    generation: i64,
) -> VSphereClusterStatus {
    let selected = select_providers(all_providers, spec);
    let failure_domains = aggregate_failure_domains(
        &selected,
        spec.control_plane_failure_domain_selector.as_ref(),
    );

    let provisioned = !failure_domains.is_empty();
    let mut conditions: Vec<Condition> = Vec::new();
    if provisioned {
        set_condition(
            &mut conditions,
            condition_types::READY,
            condition_status::TRUE,
            reasons::RECONCILED,
            format!(
                "aggregated {} failure domain(s) from {} provider(s)",
                failure_domains.len(),
                selected.len()
            ),
            generation,
        );
    } else {
        set_condition(
            &mut conditions,
            condition_types::READY,
            condition_status::FALSE,
            reasons::NO_FAILURE_DOMAINS,
            "no ready failure domains from the selected providers",
            generation,
        );
    }

    VSphereClusterStatus {
        initialization: banlieue_api::common::InitializationStatus {
            provisioned: Some(provisioned),
        },
        control_plane_endpoint: spec.control_plane_endpoint.clone(),
        failure_domains,
        conditions,
        observed_generation: Some(generation),
    }
}

/// Select the `Provider`s this cluster aggregates from. Explicit
/// `providerRefs` win when non-empty; otherwise `providerSelector` filters by
/// label (an empty selector matches every Provider). Pure.
pub fn select_providers<'a>(all: &'a [Provider], spec: &VSphereClusterSpec) -> Vec<&'a Provider> {
    if !spec.provider_refs.is_empty() {
        let names: BTreeSet<&str> = spec.provider_refs.iter().map(|r| r.name.as_str()).collect();
        return all
            .iter()
            .filter(|p| names.contains(p.name_any().as_str()))
            .collect();
    }

    let selector = Some(spec.provider_selector.clone());
    all.iter()
        .filter(|p| selector_matches(&selector, p.labels()))
        .collect()
}

/// Translate the selected `Provider`s' banlieue failure domains into the CAPI
/// v1beta2 [`ClusterFailureDomain`] list. `control_plane` eligibility is set
/// from `cp_selector` (matched against each FD's `labels`); a `None` selector
/// marks every domain control-plane eligible. Pure.
pub fn aggregate_failure_domains(
    providers: &[&Provider],
    cp_selector: Option<&LabelSelector>,
) -> Vec<ClusterFailureDomain> {
    // `selector_matches` takes `&Option<LabelSelector>`; clone once up front.
    let selector = cp_selector.cloned();

    let mut out = Vec::new();
    for provider in providers {
        let Some(status) = provider.status.as_ref() else {
            continue;
        };
        for fd in &status.failure_domains {
            let control_plane = selector_matches(&selector, &fd.labels);

            // Attributes: labels first, then provider-resolved `raw` overrides
            // (raw carries datacenter / cluster / resourcePool the machine
            // builder needs; labels are the scheduler-facing dc / cluster / env).
            let mut attributes: BTreeMap<String, String> = BTreeMap::new();
            for (k, v) in &fd.labels {
                attributes.insert(k.clone(), v.clone());
            }
            for (k, v) in &fd.attributes.raw {
                attributes.insert(k.clone(), v.clone());
            }

            out.push(ClusterFailureDomain {
                name: fd.name.clone(),
                control_plane: Some(control_plane),
                attributes,
            });
        }
    }
    out
}

/// SSA-patch the full computed status onto `VSphereCluster.status`.
async fn patch_status(
    ctx: &Context,
    namespace: &str,
    name: &str,
    status: VSphereClusterStatus,
) -> Result<()> {
    let patch = json!({
        "apiVersion": VSphereCluster::api_version(&()).to_string(),
        "kind": VSphereCluster::kind(&()).to_string(),
        "metadata": { "name": name, "namespace": namespace },
        "status": status,
    });
    apply_status_patch(ctx, namespace, name, patch).await
}

/// SSA-patch only the `Paused` / `Ready` conditions; leaves any previously
/// resolved `failureDomains` untouched (the SSA merge keeps them).
async fn patch_paused(ctx: &Context, namespace: &str, name: &str, generation: i64) -> Result<()> {
    let mut conditions: Vec<Condition> = Vec::new();
    set_condition(
        &mut conditions,
        CONDITION_PAUSED,
        condition_status::TRUE,
        reasons::PAUSED,
        "reconciliation paused via spec.paused",
        generation,
    );
    set_condition(
        &mut conditions,
        condition_types::READY,
        condition_status::FALSE,
        reasons::PAUSED,
        "reconciliation paused",
        generation,
    );
    let patch = json!({
        "apiVersion": VSphereCluster::api_version(&()).to_string(),
        "kind": VSphereCluster::kind(&()).to_string(),
        "metadata": { "name": name, "namespace": namespace },
        "status": { "conditions": conditions, "observedGeneration": generation },
    });
    apply_status_patch(ctx, namespace, name, patch).await
}

async fn apply_status_patch(
    ctx: &Context,
    namespace: &str,
    name: &str,
    patch: serde_json::Value,
) -> Result<()> {
    let api: Api<VSphereCluster> = Api::namespaced(ctx.client.clone(), namespace);
    let params = PatchParams::apply(FIELD_MANAGER_CONTROLLER).force();
    api.patch_status(name, &params, &Patch::Apply(&patch))
        .await?;
    Ok(())
}

#[cfg(test)]
#[path = "vsphere_cluster_tests.rs"]
mod vsphere_cluster_tests;
