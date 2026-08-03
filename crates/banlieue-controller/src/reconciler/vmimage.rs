// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Aggregate readiness for `VMImage` (ADR-0015).
//!
//! A `VMImage` can name sources for several backends at once —
//! `examples/04-vmimage-ubuntu.yaml` names three. Each provider reconciles the
//! same object and publishes its own `status.perProvider[]` entry, merge-keyed
//! so the entries coexist.
//!
//! Nobody in that arrangement can answer "is this image ready?". A provider
//! sees only the rows it wrote, so an aggregate it computed would be an answer
//! to a different question — and when two providers each published one, the
//! `Ready` condition flipped with whichever reconciled last. This reconciler
//! owns that single field, under field manager `banlieue.io/controller`,
//! disjoint from every provider's.
//!
//! It is pure aggregation: it reads `perProvider[]` and writes one condition.
//! No backend is contacted, so the watch is cheap.

use std::sync::Arc;

use banlieue_api::banlieue::{ImagePerProviderStatus, VMImage};
use banlieue_provider_sdk::reconciler::{requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_CONTROLLER;
use banlieue_provider_sdk::status::{condition_status, set_condition};
use kube::{
    Resource, ResourceExt,
    api::{Api, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{debug, warn};

use crate::context::Context;
use crate::error::{Error, Result};

/// Condition type this reconciler owns.
const CONDITION_READY: &str = "Ready";

/// Stable `reason` strings for the aggregate `Ready` condition.
pub mod reasons {
    /// Every provider that has reported has the image available.
    pub const RECONCILED: &str = "Reconciled";
    /// At least one provider is not ready and offered no reason of its own.
    pub const NOT_READY: &str = "NotReady";
    /// No provider has published a `perProvider` entry yet.
    pub const NO_PROVIDERS: &str = "NoProviders";
}

/// Aggregate `Ready` value derived from every provider's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateReady {
    /// `True` / `False` / `Unknown`.
    pub status: &'static str,
    /// Stable reason. Inherits the first blocking provider's own reason so an
    /// operator reading `Ready=False` has somewhere to look next.
    pub reason: String,
    /// Human-readable summary.
    pub message: String,
}

/// Derive the aggregate `Ready` condition from all per-provider rows.
///
/// `True` only when every reporting provider is ready — an image usable on one
/// backend and missing on another is not "ready", because a `VirtualMachine`
/// may be scheduled onto either.
#[must_use]
pub fn aggregate_ready(rows: &[ImagePerProviderStatus]) -> AggregateReady {
    if rows.is_empty() {
        return AggregateReady {
            // Unknown, not False: nothing has reported, which is not the same
            // as having reported a problem.
            status: condition_status::UNKNOWN,
            reason: reasons::NO_PROVIDERS.to_string(),
            message: "no provider has reported on this image yet".to_string(),
        };
    }

    let unready: Vec<&ImagePerProviderStatus> = rows.iter().filter(|r| !r.ready).collect();
    if unready.is_empty() {
        return AggregateReady {
            status: condition_status::TRUE,
            reason: reasons::RECONCILED.to_string(),
            message: format!("image available on {} provider(s)", rows.len()),
        };
    }

    // Inherit the first blocking provider's reason. `perProvider` is merge-keyed
    // now, so the apiserver gives no ordering guarantee across managers; pick
    // deterministically by provider identity rather than by list position.
    let first = unready
        .iter()
        .min_by_key(|r| (&r.provider_namespace, &r.provider_name))
        .expect("unready is non-empty");

    AggregateReady {
        status: condition_status::FALSE,
        reason: first
            .reason
            .clone()
            .unwrap_or_else(|| reasons::NOT_READY.to_string()),
        message: format!(
            "{} of {} provider(s) do not have this image",
            unready.len(),
            rows.len()
        ),
    }
}

/// Build the status patch this reconciler applies.
///
/// Pure, so the field-manager split is testable without a cluster: the patch
/// must carry `conditions` and nothing else, or it would re-create exactly the
/// contention ADR-0015 removed.
#[must_use]
pub fn build_status_patch(
    name: &str,
    aggregate: &AggregateReady,
    generation: i64,
) -> serde_json::Value {
    let mut conditions = Vec::new();
    set_condition(
        &mut conditions,
        CONDITION_READY,
        aggregate.status,
        &aggregate.reason,
        aggregate.message.clone(),
        generation,
    );
    json!({
        "apiVersion": VMImage::api_version(&()).to_string(),
        "kind": VMImage::kind(&()).to_string(),
        "metadata": { "name": name },
        "status": { "conditions": conditions },
    })
}

/// Reconcile one `VMImage`: aggregate provider rows into `Ready`.
///
/// # Errors
/// [`Error::Kube`] if the status patch is rejected.
pub async fn reconcile(image: Arc<VMImage>, ctx: Arc<Context>) -> Result<Action> {
    let name = image.name_any();
    let generation = image.metadata.generation.unwrap_or(0);

    let rows = image
        .status
        .as_ref()
        .map(|s| s.per_provider.as_slice())
        .unwrap_or_default();
    let aggregate = aggregate_ready(rows);

    debug!(
        image = %name,
        providers = rows.len(),
        ready = aggregate.status,
        "aggregating VMImage readiness"
    );

    let api: Api<VMImage> = Api::all(ctx.client.clone());
    api.patch_status(
        &name,
        &PatchParams::apply(FIELD_MANAGER_CONTROLLER).force(),
        &Patch::Apply(&build_status_patch(&name, &aggregate, generation)),
    )
    .await?;

    // Nothing to poll for: the next provider write re-triggers the watch.
    Ok(requeue_long())
}

/// `error_policy` invoked on `reconcile` failure.
pub fn error_policy(_image: Arc<VMImage>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "vmimage aggregate reconcile error policy fired");
    requeue_on_error()
}

#[cfg(test)]
#[path = "vmimage_tests.rs"]
mod vmimage_tests;
