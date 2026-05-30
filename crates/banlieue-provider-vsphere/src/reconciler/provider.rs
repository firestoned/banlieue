// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `Provider` reconciler — capability introspection against vCenter.
//!
//! Phase 1B iteration 1: list datacenters and clusters, emit one
//! `FailureDomain` per (dc, cluster), and set the `Ready` /
//! `ProviderReachable` conditions on `Provider.status`. Storage-class /
//! network-class verification and feature detection land in iteration 2 once
//! the vim_rs surface for datastores / port groups is in place.

use std::collections::BTreeMap;
use std::sync::Arc;

use banlieue_api::banlieue::{
    FailureDomain, FailureDomainAttributes, Provider, ProviderConnection, ProviderStatus,
};
use banlieue_provider_sdk::reconciler::{requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_PROVIDER_VSPHERE;
use banlieue_provider_sdk::status::{condition_status, set_condition};
use k8s_openapi::api::core::v1::Secret;
use kube::{
    Resource, ResourceExt,
    api::{Api, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{info, warn};

use crate::client::{Credentials, VSphereClient};
use crate::context::Context;
use crate::error::{Error, Result};

/// Provider-class name banlieue uses to identify a vCenter-backed Provider.
/// Must match `Provider.spec.providerClassRef.name` for this binary to act.
pub const PROVIDER_CLASS_NAME: &str = "vsphere";

/// Standard k8s Secret keys we read from `Provider.spec.connection.credentialsRef`.
const SECRET_KEY_USERNAME: &str = "username";
const SECRET_KEY_PASSWORD: &str = "password";

/// Maximum length of a generated FailureDomain name. K8s label values are
/// capped at 63 chars and the FailureDomain name is often used as one.
const MAX_FD_NAME_LEN: usize = 63;

/// Condition type set on `Provider.status.conditions`.
mod condition_types {
    pub const READY: &str = "Ready";
    pub const PROVIDER_REACHABLE: &str = "ProviderReachable";
}

/// Stable `reason` strings on the conditions. Keep these stable — operators
/// match against them in alerts and tests.
mod reasons {
    pub const RECONCILED: &str = "Reconciled";
    pub const CONNECT_FAILED: &str = "ConnectFailed";
    pub const SECRET_MISSING: &str = "SecretMissing";
    pub const SECRET_INVALID: &str = "SecretInvalid";
    pub const INVENTORY_FAILED: &str = "InventoryFailed";
}

/// Top-level reconcile entrypoint registered with [`kube::runtime::Controller`].
pub async fn reconcile(provider: Arc<Provider>, ctx: Arc<Context>) -> Result<Action> {
    let namespace = provider.namespace().ok_or(Error::Missing("namespace"))?;
    let name = provider.name_any();
    let generation = provider.metadata.generation.unwrap_or(0);

    // Predicate: only handle Providers of class "vsphere". The Controller's
    // watch sees every Provider in scope; cheaper than a server-side label
    // selector at v1alpha1 where ProviderClass isn't a real CR yet.
    if provider.spec.provider_class_ref.name != PROVIDER_CLASS_NAME {
        return Ok(requeue_long());
    }

    let span = tracing::info_span!(
        "reconcile",
        kind = "Provider",
        namespace = %namespace,
        name = %name,
        generation,
    );
    let _enter = span.enter();
    info!("reconciling Provider");

    if provider.spec.paused {
        info!("provider is paused — skipping reconciliation");
        return Ok(requeue_long());
    }

    // Resolve credentials.
    let creds = match read_credentials(&ctx, &namespace, &provider.spec.connection).await {
        Ok(c) => c,
        Err(e) => {
            let (reason, msg) = match &e {
                Error::Missing(_) => (reasons::SECRET_MISSING, format!("{e}")),
                _ => (reasons::SECRET_INVALID, format!("{e}")),
            };
            warn!(error = %e, "credentials resolution failed");
            patch_status_failed(
                &ctx,
                &namespace,
                &name,
                generation,
                condition_types::PROVIDER_REACHABLE,
                reason,
                msg,
            )
            .await?;
            return Ok(requeue_on_error());
        }
    };

    // Connect to vCenter.
    let client = match ctx.vsphere.build(&provider.spec.connection, &creds).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "vCenter connect failed");
            patch_status_failed(
                &ctx,
                &namespace,
                &name,
                generation,
                condition_types::PROVIDER_REACHABLE,
                reasons::CONNECT_FAILED,
                format!("{e}"),
            )
            .await?;
            return Ok(requeue_on_error());
        }
    };

    // Walk inventory and build the failure-domain list.
    let provider_name = name.clone();
    let failure_domains = match discover_inventory(client.as_ref(), &provider_name).await {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "inventory walk failed");
            patch_status_failed(
                &ctx,
                &namespace,
                &name,
                generation,
                condition_types::READY,
                reasons::INVENTORY_FAILED,
                format!("{e}"),
            )
            .await?;
            return Ok(requeue_on_error());
        }
    };

    let fd_count = failure_domains.len();
    info!(fd_count, "vCenter inventory walk complete");

    patch_status_success(&ctx, &namespace, &name, generation, failure_domains).await?;

    // Capability introspection is comparatively expensive (a few vCenter
    // round-trips). Re-poll on the long requeue cadence rather than the
    // controller default; spec/secret changes trigger immediate reconciles
    // via the kube watcher.
    Ok(requeue_long())
}

/// `error_policy` callback the controller invokes when [`reconcile`] returns
/// `Err`. Short backoff — most errors here are transient (network blips,
/// vCenter session expiry).
pub fn error_policy(_provider: Arc<Provider>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "reconcile error policy fired");
    requeue_on_error()
}

/// Walk the vCenter inventory and produce one [`FailureDomain`] per
/// (datacenter, cluster). Pure with respect to its `client` argument — used
/// directly by the reconciler and by unit tests with a [`FakeClient`].
pub async fn discover_inventory(
    client: &dyn VSphereClient,
    provider_name: &str,
) -> Result<Vec<FailureDomain>> {
    let mut out = Vec::new();
    for dc in client.list_datacenters().await? {
        for cluster in client.list_clusters(&dc).await? {
            out.push(build_failure_domain(provider_name, &dc.name, &cluster.name));
        }
    }
    Ok(out)
}

/// Build a single [`FailureDomain`] from the (provider, dc, cluster) triple.
fn build_failure_domain(provider: &str, dc_name: &str, cluster_name: &str) -> FailureDomain {
    let mut labels = BTreeMap::new();
    labels.insert("dc".to_string(), dc_name.to_string());
    labels.insert("cluster".to_string(), cluster_name.to_string());

    let mut raw = BTreeMap::new();
    raw.insert("datacenter".to_string(), dc_name.to_string());
    raw.insert("cluster".to_string(), cluster_name.to_string());

    FailureDomain {
        name: failure_domain_name(provider, dc_name, cluster_name),
        labels,
        attributes: FailureDomainAttributes {
            available_storage_classes: Vec::new(),
            available_network_classes: Vec::new(),
            features: Vec::new(),
            raw,
        },
    }
}

/// Slugify `<provider>-<dc>-<cluster>` into a DNS-label-friendly string.
/// Truncates to 63 chars (the K8s label-value cap). Pure and unit-tested.
pub fn failure_domain_name(provider: &str, dc: &str, cluster: &str) -> String {
    let raw = format!("{provider}-{dc}-{cluster}");
    let slug = slugify(&raw);
    if slug.len() > MAX_FD_NAME_LEN {
        slug[..MAX_FD_NAME_LEN].trim_end_matches('-').to_string()
    } else {
        slug
    }
}

/// Lowercase the input, replace any run of non-alphanumeric characters with
/// a single `-`, and strip leading/trailing dashes.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_dash = true;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Fetch the credentials Secret named by `connection.credentials_ref` and
/// pluck `username` / `password`.
async fn read_credentials(
    ctx: &Context,
    namespace: &str,
    connection: &ProviderConnection,
) -> Result<Credentials> {
    let secret_name = &connection.credentials_ref.name;
    let api: Api<Secret> = Api::namespaced(ctx.client.clone(), namespace);
    let secret = api.get(secret_name).await.map_err(|e| {
        // 404 → missing; everything else surfaces as the raw kube error.
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

/// SSA-patch `Provider.status` with the discovered failure domains plus the
/// success conditions.
async fn patch_status_success(
    ctx: &Context,
    namespace: &str,
    name: &str,
    generation: i64,
    failure_domains: Vec<FailureDomain>,
) -> Result<()> {
    let mut conditions = Vec::new();
    set_condition(
        &mut conditions,
        condition_types::PROVIDER_REACHABLE,
        condition_status::TRUE,
        reasons::RECONCILED,
        "vCenter reachable; inventory walk succeeded",
        generation,
    );
    set_condition(
        &mut conditions,
        condition_types::READY,
        condition_status::TRUE,
        reasons::RECONCILED,
        "Provider reconciled",
        generation,
    );

    let status = ProviderStatus {
        failure_domains,
        conditions,
        observed_generation: Some(generation),
    };
    patch_provider_status(ctx, namespace, name, status).await
}

/// SSA-patch a failure condition (one of Ready / ProviderReachable) onto
/// `Provider.status.conditions`. Preserves any previously-known failure
/// domains rather than blanking them — that information is still valuable
/// while we recover.
async fn patch_status_failed(
    ctx: &Context,
    namespace: &str,
    name: &str,
    generation: i64,
    condition_type: &str,
    reason: &str,
    message: String,
) -> Result<()> {
    let mut conditions = Vec::new();
    set_condition(
        &mut conditions,
        condition_type,
        condition_status::FALSE,
        reason,
        &message,
        generation,
    );
    // Aggregate Ready=False whenever any sub-condition fails so dashboards
    // can match a single boolean.
    if condition_type != condition_types::READY {
        set_condition(
            &mut conditions,
            condition_types::READY,
            condition_status::FALSE,
            reason,
            message,
            generation,
        );
    }

    // Don't bother carrying failure_domains across error transitions — the
    // SSA merge will keep whatever the previous successful reconcile wrote.
    let patch = json!({
        "apiVersion": Provider::api_version(&()).to_string(),
        "kind": Provider::kind(&()).to_string(),
        "metadata": { "name": name, "namespace": namespace },
        "status": {
            "conditions": conditions,
            "observedGeneration": generation,
        }
    });
    apply_status_patch(ctx, namespace, name, patch).await
}

async fn patch_provider_status(
    ctx: &Context,
    namespace: &str,
    name: &str,
    status: ProviderStatus,
) -> Result<()> {
    let patch = json!({
        "apiVersion": Provider::api_version(&()).to_string(),
        "kind": Provider::kind(&()).to_string(),
        "metadata": { "name": name, "namespace": namespace },
        "status": status,
    });
    apply_status_patch(ctx, namespace, name, patch).await
}

async fn apply_status_patch(
    ctx: &Context,
    namespace: &str,
    name: &str,
    patch: serde_json::Value,
) -> Result<()> {
    let api: Api<Provider> = Api::namespaced(ctx.client.clone(), namespace);
    let params = PatchParams::apply(FIELD_MANAGER_PROVIDER_VSPHERE).force();
    api.patch_status(name, &params, &Patch::Apply(&patch))
        .await?;
    Ok(())
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;
