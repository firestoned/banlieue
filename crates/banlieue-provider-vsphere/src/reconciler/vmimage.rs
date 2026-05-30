// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VMImage` reconciler — template-availability check on vSphere.
//!
//! For every `Provider` of class `vsphere` in scope, look up the template
//! named in `VMImage.spec.sources[]` (where `provider_class == "vsphere"`).
//! Flip the matching `VMImage.status.perProvider[]` entry to `ready=true`
//! (with `resolved_ref` populated) when the template is found in every
//! datacenter the Provider has a failure domain in; `ready=false` otherwise
//! with a stable [`reasons`] tag.
//!
//! The pure helper [`compute_per_provider_status`] takes a slice of
//! `(provider, client)` pairs so the reconciler tests can drive it with
//! `FakeClient` and never touch `kube::Api`.

use std::sync::Arc;

use banlieue_api::banlieue::{
    ImagePerProviderStatus, ImageSource, ImageSourceKind, Provider, VMImage, VMImageStatus,
};
use banlieue_provider_sdk::reconciler::{requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_PROVIDER_VSPHERE;
use banlieue_provider_sdk::status::{condition_status, set_condition};
use k8s_openapi::api::core::v1::Secret;
use kube::{
    Resource, ResourceExt,
    api::{Api, ListParams, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{info, warn};

use super::provider::PROVIDER_CLASS_NAME;
use crate::client::{Credentials, Datacenter, VSphereClient};
use crate::context::Context;
use crate::error::{Error, Result};

const SECRET_KEY_USERNAME: &str = "username";
const SECRET_KEY_PASSWORD: &str = "password";

/// Condition types written onto `VMImage.status.conditions`.
mod condition_types {
    pub const READY: &str = "Ready";
}

/// Stable `reason` strings for `ImagePerProviderStatus.reason` and the
/// aggregate `Ready` condition. Operators match against these.
pub mod reasons {
    /// All resolved providers have the template available.
    pub const RECONCILED: &str = "Reconciled";
    /// At least one vSphere Provider does not have the template in any
    /// reachable datacenter.
    pub const TEMPLATE_NOT_FOUND: &str = "TemplateNotFound";
    /// The Provider's credentials Secret is missing or malformed.
    pub const SECRET_UNAVAILABLE: &str = "SecretUnavailable";
    /// We could not connect to the Provider's vCenter.
    pub const CONNECT_FAILED: &str = "ConnectFailed";
    /// vCenter rejected the inventory walk during template lookup.
    pub const LOOKUP_FAILED: &str = "LookupFailed";
    /// No vSphere ImageSource on this VMImage — nothing to do for this provider class.
    pub const NO_VSPHERE_SOURCE: &str = "NoVSphereSource";
}

/// Top-level reconcile entrypoint.
///
/// 1. Read the `VMImage` spec and bail early if no vsphere `ImageSource` is
///    declared (other providers handle their own classes).
/// 2. List every `Provider` (cluster-wide or scoped) of class `vsphere`.
/// 3. For each Provider, connect and look up the template name in every
///    failure-domain datacenter.
/// 4. SSA-patch `VMImage.status.perProvider[]` with the per-provider rows
///    and set the aggregate `Ready` condition.
pub async fn reconcile(image: Arc<VMImage>, ctx: Arc<Context>) -> Result<Action> {
    let name = image.name_any();
    let generation = image.metadata.generation.unwrap_or(0);

    let span = tracing::info_span!(
        "reconcile",
        kind = "VMImage",
        name = %name,
        generation,
    );
    let _enter = span.enter();
    info!("reconciling VMImage");

    let Some(vsphere_source) = find_vsphere_source(&image.spec.sources) else {
        // Not our concern — every other provider handles its own ImageSources.
        return Ok(requeue_long());
    };

    let providers = list_vsphere_providers(&ctx).await?;
    if providers.is_empty() {
        info!("no vsphere Providers in scope — leaving status untouched");
        return Ok(requeue_long());
    }

    let mut rows: Vec<ImagePerProviderStatus> = Vec::with_capacity(providers.len());
    for provider in &providers {
        let row = reconcile_for_provider(&ctx, provider, vsphere_source).await;
        rows.push(row);
    }

    let aggregate = aggregate_ready(&rows);
    patch_vmimage_status(&ctx, &name, generation, rows, aggregate).await?;

    Ok(requeue_long())
}

/// `error_policy` invoked on `reconcile` failure.
pub fn error_policy(_image: Arc<VMImage>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "vmimage reconcile error policy fired");
    requeue_on_error()
}

/// Compute the per-provider status row for one `(Provider, vsphere ImageSource)`
/// pair. Connects to the Provider's vCenter, walks its failure-domain
/// datacenters, and confirms the template exists in each. Errors become
/// `ready=false` rows with a stable `reason`; never returns `Err`.
pub async fn reconcile_for_provider(
    ctx: &Context,
    provider: &Provider,
    source: &ImageSource,
) -> ImagePerProviderStatus {
    let namespace = provider.namespace().unwrap_or_default();

    let creds = match read_credentials(ctx, &namespace, provider).await {
        Ok(c) => c,
        Err(e) => {
            return per_provider_failure(provider, reasons::SECRET_UNAVAILABLE, e.to_string());
        }
    };
    let client = match ctx.vsphere.build(&provider.spec.connection, &creds).await {
        Ok(c) => c,
        Err(e) => return per_provider_failure(provider, reasons::CONNECT_FAILED, e.to_string()),
    };

    let datacenters = match dcs_from_provider_status(provider, client.as_ref()).await {
        Ok(v) => v,
        Err(e) => {
            return per_provider_failure(provider, reasons::LOOKUP_FAILED, e.to_string());
        }
    };

    compute_template_status(client.as_ref(), &datacenters, &source.reference, provider).await
}

/// Pure helper for the per-provider template check: given a connected client
/// and a list of candidate datacenters, return a populated
/// [`ImagePerProviderStatus`] row.
pub async fn compute_template_status(
    client: &dyn VSphereClient,
    datacenters: &[Datacenter],
    template_name: &str,
    provider: &Provider,
) -> ImagePerProviderStatus {
    if datacenters.is_empty() {
        return per_provider_failure(
            provider,
            reasons::TEMPLATE_NOT_FOUND,
            "no datacenters discovered for this Provider".to_string(),
        );
    }

    let mut hits = Vec::new();
    for dc in datacenters {
        match client.find_template(dc, template_name).await {
            Ok(Some(t)) => hits.push((dc.name.clone(), t)),
            Ok(None) => {}
            Err(e) => {
                return per_provider_failure(provider, reasons::LOOKUP_FAILED, e.to_string());
            }
        }
    }

    if hits.is_empty() {
        return per_provider_failure(
            provider,
            reasons::TEMPLATE_NOT_FOUND,
            format!("template {template_name:?} not present in any datacenter"),
        );
    }

    let resolved = render_resolved_ref(&hits, template_name);
    ImagePerProviderStatus {
        provider_name: provider.name_any(),
        provider_namespace: provider.namespace().unwrap_or_default(),
        ready: true,
        resolved_ref: Some(resolved),
        reason: Some(reasons::RECONCILED.to_string()),
        message: None,
    }
}

/// Pick the first vsphere `ImageSource` of kind `Template`. Iter 2a does not
/// support `Url`-import or `BackingFile` — those land in later iterations.
pub fn find_vsphere_source(sources: &[ImageSource]) -> Option<&ImageSource> {
    sources
        .iter()
        .find(|s| s.provider_class == PROVIDER_CLASS_NAME && s.kind == ImageSourceKind::Template)
}

/// Aggregate `Ready` condition value: True only if every per-provider entry
/// is `ready=true`.
pub fn aggregate_ready(rows: &[ImagePerProviderStatus]) -> AggregateReady {
    if rows.is_empty() {
        return AggregateReady {
            status: condition_status::UNKNOWN,
            reason: reasons::NO_VSPHERE_SOURCE,
            message: "no providers reconciled yet".to_string(),
        };
    }
    let unready: Vec<&ImagePerProviderStatus> = rows.iter().filter(|r| !r.ready).collect();
    if unready.is_empty() {
        AggregateReady {
            status: condition_status::TRUE,
            reason: reasons::RECONCILED,
            message: format!("template available on {} provider(s)", rows.len()),
        }
    } else {
        // Inherit the first failure's reason so dashboards can drill in.
        let reason = unready[0]
            .reason
            .as_deref()
            .unwrap_or(reasons::LOOKUP_FAILED);
        AggregateReady {
            status: condition_status::FALSE,
            reason: leak(reason),
            message: format!(
                "{} of {} providers do not have the template",
                unready.len(),
                rows.len()
            ),
        }
    }
}

/// Aggregate result of [`aggregate_ready`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateReady {
    pub status: &'static str,
    pub reason: &'static str,
    pub message: String,
}

// `reason` strings in `ImagePerProviderStatus` are `String`s; in the aggregate
// they are `&'static str`s (because [`set_condition`] takes `&str` and we
// want a stable enum-like set). When promoting a per-row `String` to a
// `&'static str`, we accept that the leak only happens on transition (rare).
fn leak(s: &str) -> &'static str {
    match s {
        reasons::RECONCILED => reasons::RECONCILED,
        reasons::TEMPLATE_NOT_FOUND => reasons::TEMPLATE_NOT_FOUND,
        reasons::SECRET_UNAVAILABLE => reasons::SECRET_UNAVAILABLE,
        reasons::CONNECT_FAILED => reasons::CONNECT_FAILED,
        reasons::LOOKUP_FAILED => reasons::LOOKUP_FAILED,
        reasons::NO_VSPHERE_SOURCE => reasons::NO_VSPHERE_SOURCE,
        // Unknown reason → bucket as LOOKUP_FAILED so dashboards still match a
        // known string. Never leak arbitrary input.
        _ => reasons::LOOKUP_FAILED,
    }
}

fn per_provider_failure(
    provider: &Provider,
    reason: &str,
    message: String,
) -> ImagePerProviderStatus {
    ImagePerProviderStatus {
        provider_name: provider.name_any(),
        provider_namespace: provider.namespace().unwrap_or_default(),
        ready: false,
        resolved_ref: None,
        reason: Some(reason.to_string()),
        message: Some(message),
    }
}

fn render_resolved_ref(hits: &[(String, crate::client::Template)], template_name: &str) -> String {
    // vSphere convention: "[datacenter,...] template-name". With one DC hit
    // we render the simpler "[dc] name"; with multiple we list all the DCs.
    let dcs: Vec<&str> = hits.iter().map(|(dc, _)| dc.as_str()).collect();
    format!("[{}] {}", dcs.join(","), template_name)
}

/// Read the Provider's credentials Secret. Mirrors `provider.rs::read_credentials`
/// — kept local so the two reconcilers can evolve independently.
async fn read_credentials(
    ctx: &Context,
    namespace: &str,
    provider: &Provider,
) -> Result<Credentials> {
    let secret_name = &provider.spec.connection.credentials_ref.name;
    let api: Api<Secret> = Api::namespaced(ctx.client.clone(), namespace);
    let secret = api.get(secret_name).await.map_err(|e| {
        if let kube::Error::Api(api_err) = &e
            && api_err.code == 404
        {
            return Error::Missing("Provider.spec.connection.credentialsRef");
        }
        Error::Kube(e)
    })?;
    let data = secret.data.unwrap_or_default();
    let username = data
        .get(SECRET_KEY_USERNAME)
        .ok_or(Error::Missing("secret.data.username"))?;
    let password = data
        .get(SECRET_KEY_PASSWORD)
        .ok_or(Error::Missing("secret.data.password"))?;
    Ok(Credentials {
        username: String::from_utf8(username.0.clone())
            .map_err(|_| Error::Missing("secret.data.username (not utf-8)"))?,
        password: String::from_utf8(password.0.clone())
            .map_err(|_| Error::Missing("secret.data.password (not utf-8)"))?,
    })
}

/// Resolve the candidate datacenters for a Provider. Prefers
/// `Provider.status.failureDomains[*].attributes.raw["datacenter"]` populated
/// by the [`super::provider`] reconciler; falls back to a live `list_datacenters`
/// when the status is empty (first-touch race).
async fn dcs_from_provider_status(
    provider: &Provider,
    client: &dyn VSphereClient,
) -> Result<Vec<Datacenter>> {
    let mut from_status: Vec<String> = Vec::new();
    if let Some(status) = provider.status.as_ref() {
        for fd in &status.failure_domains {
            if let Some(dc) = fd.attributes.raw.get("datacenter")
                && !from_status.contains(dc)
            {
                from_status.push(dc.clone());
            }
        }
    }
    if from_status.is_empty() {
        // Provider hasn't been reconciled yet; do a live walk.
        return client.list_datacenters().await;
    }
    let live = client.list_datacenters().await?;
    // Cross-reference: keep only DCs that vCenter currently reports AND that
    // appear in Provider.status. Drops stale Provider.status entries.
    Ok(live
        .into_iter()
        .filter(|dc| from_status.contains(&dc.name))
        .collect())
}

/// List vsphere-class Providers in scope.
async fn list_vsphere_providers(ctx: &Context) -> Result<Vec<Provider>> {
    let api: Api<Provider> = match ctx.namespace.as_deref() {
        Some(ns) => Api::namespaced(ctx.client.clone(), ns),
        None => Api::all(ctx.client.clone()),
    };
    let list = api.list(&ListParams::default()).await?;
    Ok(list
        .into_iter()
        .filter(|p| p.spec.provider_class_ref.name == PROVIDER_CLASS_NAME)
        .collect())
}

async fn patch_vmimage_status(
    ctx: &Context,
    name: &str,
    generation: i64,
    per_provider: Vec<ImagePerProviderStatus>,
    aggregate: AggregateReady,
) -> Result<()> {
    let mut conditions = Vec::new();
    set_condition(
        &mut conditions,
        condition_types::READY,
        aggregate.status,
        aggregate.reason,
        aggregate.message,
        generation,
    );

    let status = VMImageStatus {
        per_provider,
        conditions,
        observed_generation: Some(generation),
    };

    let patch = json!({
        "apiVersion": VMImage::api_version(&()).to_string(),
        "kind": VMImage::kind(&()).to_string(),
        "metadata": { "name": name },
        "status": status,
    });

    let api: Api<VMImage> = Api::all(ctx.client.clone());
    let params = PatchParams::apply(FIELD_MANAGER_PROVIDER_VSPHERE).force();
    api.patch_status(name, &params, &Patch::Apply(&patch))
        .await?;
    Ok(())
}

#[cfg(test)]
#[path = "vmimage_tests.rs"]
mod vmimage_tests;
