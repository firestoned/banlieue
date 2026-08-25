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
    FailureDomain, FailureDomainAttributes, FailureDomainNameOverride, Provider,
    ProviderCapabilities, ProviderConnection, ProviderStatus,
};
use banlieue_provider_sdk::reconciler::{requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_PROVIDER_VSPHERE;
use banlieue_provider_sdk::status::{condition_status, set_condition};
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{
    Resource, ResourceExt,
    api::{Api, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{info, warn};

use crate::client::{Credentials, Datastore, Network, VSphereClient};
use crate::context::Context;
use crate::error::{Error, Result};

/// Provider-class name banlieue uses to identify a vCenter-backed Provider.
/// Must match `Provider.spec.providerClassRef.name` for this binary to act.
pub const PROVIDER_CLASS_NAME: &str = "vsphere";

/// Standard k8s Secret keys we read from `Provider.spec.connection.credentialsRef`.
const SECRET_KEY_USERNAME: &str = "username";
const SECRET_KEY_PASSWORD: &str = "password";

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

    // Seed status updates with the conditions already on the object so
    // `set_condition` preserves each `lastTransitionTime` when the status is
    // unchanged. Rebuilding from an empty Vec re-stamped `now()` every pass,
    // so the server-side-apply patch always differed → new resourceVersion →
    // watch event → reconcile → hot-loop.
    let existing = provider
        .status
        .as_ref()
        .map(|s| s.conditions.clone())
        .unwrap_or_default();

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
                &existing,
                condition_types::PROVIDER_REACHABLE,
                reason,
                msg,
            )
            .await?;
            return Ok(requeue_on_error());
        }
    };

    // Resolve the CA bundle (inline / configMapRef / secretRef) to PEM, if any.
    let ca_bundle_pem = match crate::reconciler::ca_bundle::resolve_ca_bundle(
        &ctx,
        &namespace,
        &provider.spec.connection.ca_bundle,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "caBundle resolution failed");
            patch_status_failed(
                &ctx,
                &namespace,
                &name,
                generation,
                &existing,
                condition_types::PROVIDER_REACHABLE,
                reasons::CONNECT_FAILED,
                format!("{e}"),
            )
            .await?;
            return Ok(requeue_on_error());
        }
    };

    // Connect to vCenter.
    let client = match ctx
        .vsphere
        .build(&provider.spec.connection, &creds, ca_bundle_pem.as_deref())
        .await
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "vCenter connect failed");
            patch_status_failed(
                &ctx,
                &namespace,
                &name,
                generation,
                &existing,
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
    let failure_domains = match discover_inventory(
        client.as_ref(),
        &provider_name,
        &provider.spec.capabilities,
        &provider.spec.failure_domain_name_overrides,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "inventory walk failed");
            patch_status_failed(
                &ctx,
                &namespace,
                &name,
                generation,
                &existing,
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

    patch_status_success(
        &ctx,
        &namespace,
        &name,
        generation,
        &existing,
        failure_domains,
    )
    .await?;

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
///
/// `overrides` (`Provider.spec.failureDomainNameOverrides`, ADR-0023) lets an
/// admin replace the auto-computed name for specific `(datacenter, cluster)`
/// pairs with a simpler one; every other pair still gets the auto-computed,
/// collision-safe name. Fails closed if the result has two failure domains
/// with the same name — most likely two overrides resolving to the same
/// name, which would silently reintroduce the cross-zone collision
/// ADR-0020 Decision #5 fixed.
pub async fn discover_inventory(
    client: &dyn VSphereClient,
    provider_name: &str,
    capabilities: &ProviderCapabilities,
    overrides: &[FailureDomainNameOverride],
) -> Result<Vec<FailureDomain>> {
    let mut out = Vec::new();
    for dc in client.list_datacenters().await? {
        for cluster in client.list_clusters(&dc).await? {
            // Per-cluster reachability drives which declared storage/network
            // classes are available in this failure domain (ADR-0019).
            let datastores = client.list_datastores(&cluster).await?;
            let networks = client.list_networks(&cluster).await?;
            let attributes = compute_failure_domain_attributes(
                capabilities,
                &datastores,
                &networks,
                &dc.name,
                &cluster.name,
            );
            out.push(build_failure_domain(
                provider_name,
                &dc.name,
                &cluster.name,
                attributes,
                overrides,
            ));
        }
    }
    // Deterministic order: vCenter does not guarantee a stable inventory
    // ordering across calls, and an unstable order would rewrite
    // `status.failureDomains` every reconcile — a status change that
    // re-triggers the watch and hot-loops the controller. Sorting by the
    // (unique) FD name pins it.
    out.sort_by(|a, b| a.name.cmp(&b.name));

    let mut seen = std::collections::BTreeSet::new();
    for fd in &out {
        if !seen.insert(fd.name.as_str()) {
            return Err(Error::InvalidSpec(format!(
                "two failure domains resolved to the same name {:?} — check \
                 spec.failureDomainNameOverrides for two (datacenter, cluster) \
                 pairs mapped to the same name",
                fd.name
            )));
        }
    }
    Ok(out)
}

/// Find an admin-declared override for `(dc, cluster)`, if any.
fn find_failure_domain_name_override<'a>(
    overrides: &'a [FailureDomainNameOverride],
    dc: &str,
    cluster: &str,
) -> Option<&'a str> {
    overrides
        .iter()
        .find(|o| o.datacenter == dc && o.cluster == cluster)
        .map(|o| o.name.as_str())
}

/// Build a single [`FailureDomain`] from the (provider, dc, cluster) triple and
/// its already-computed [`FailureDomainAttributes`]. Uses a matching entry in
/// `overrides` (ADR-0023) verbatim (slugified/hash-guarded the same way any
/// name is) instead of the auto-computed name, when one matches.
fn build_failure_domain(
    provider: &str,
    dc_name: &str,
    cluster_name: &str,
    attributes: FailureDomainAttributes,
    overrides: &[FailureDomainNameOverride],
) -> FailureDomain {
    let mut labels = BTreeMap::new();
    labels.insert("datacenter".to_string(), dc_name.to_string());
    labels.insert("cluster".to_string(), cluster_name.to_string());

    let name = match find_failure_domain_name_override(overrides, dc_name, cluster_name) {
        Some(override_name) => crate::k8s_name::collision_safe_name(&[override_name]),
        None => failure_domain_name(&FailureDomainIdentity {
            provider,
            dc: dc_name,
            cluster: cluster_name,
        }),
    };

    // A `failureDomainSelector` only ever matches `labels` — the resolved
    // `name` above (auto-computed or an ADR-0023 override) is otherwise
    // unselectable, since it's a top-level field, not a label. Mirroring it
    // here lets an operator target a specific zone by its friendly override
    // name (`matchLabels: { name: cluster-01 }`) instead of the raw,
    // backend-reported `cluster` label.
    labels.insert("name".to_string(), name.clone());

    FailureDomain {
        name,
        labels,
        attributes,
    }
}

/// Compute a failure domain's capability attributes from the reachable
/// datastores/networks and the Provider's declared capabilities (ADR-0019).
/// Each class's target is resolved for THIS `(dc_name, cluster_name)`
/// specifically — an exact `per_zone` entry if one matches, else the
/// mapping's default `target` — so the same abstract class name can be
/// reachable via a different concrete datastore/port-group per cluster of
/// the same Provider (ADR-0030). Pure and unit-tested with the `FakeClient`
/// fixtures.
pub fn compute_failure_domain_attributes(
    capabilities: &ProviderCapabilities,
    datastores: &[Datastore],
    networks: &[Network],
    dc_name: &str,
    cluster_name: &str,
) -> FailureDomainAttributes {
    let available_storage_classes = capabilities
        .storage_classes
        .iter()
        .filter(|sc| {
            sc.target_for(dc_name, cluster_name)
                .is_some_and(|target| storage_class_reachable(target, datastores))
        })
        .map(|sc| sc.name.clone())
        .collect();
    let available_network_classes = capabilities
        .network_classes
        .iter()
        .filter(|nc| {
            nc.target_for(dc_name, cluster_name)
                .is_some_and(|target| network_class_reachable(target, networks))
        })
        .map(|nc| nc.name.clone())
        .collect();

    let mut raw = BTreeMap::new();
    raw.insert("datacenter".to_string(), dc_name.to_string());
    raw.insert("cluster".to_string(), cluster_name.to_string());

    FailureDomainAttributes {
        available_storage_classes,
        available_network_classes,
        // Admin-asserted features are passed through verbatim; capability-flag
        // downgrade is an ADR-0019 follow-up.
        features: capabilities.features.clone(),
        raw,
    }
}

/// True when a `storageClasses[].target` is satisfiable by the datastores
/// reachable from a cluster. Supports `datastore` (exact name) and
/// `datastoreCluster` (SDRS StoragePod membership). `tagCategory`/`tag` targets
/// need the CIS REST tagging API and are not evaluated here (ADR-0019).
fn storage_class_reachable(target: &BTreeMap<String, String>, datastores: &[Datastore]) -> bool {
    if let Some(name) = target.get("datastore") {
        return datastores.iter().any(|d| &d.name == name);
    }
    if let Some(dsc) = target.get("datastoreCluster") {
        return datastores
            .iter()
            .any(|d| d.datastore_cluster.as_deref() == Some(dsc.as_str()));
    }
    false
}

/// True when a `networkClasses[].target` is satisfiable by the networks
/// reachable from a cluster. `portGroup` matches a standard port group;
/// `distributedPortGroup` matches a distributed (vDS) one.
fn network_class_reachable(target: &BTreeMap<String, String>, networks: &[Network]) -> bool {
    if let Some(name) = target.get("portGroup") {
        return networks.iter().any(|n| !n.distributed && &n.name == name);
    }
    if let Some(name) = target.get("distributedPortGroup") {
        return networks.iter().any(|n| n.distributed && &n.name == name);
    }
    false
}

/// Named fields identifying one generated `FailureDomain.name` — named so a
/// call site can't accidentally swap `dc` and `cluster` the way three
/// positional `&str` parameters would silently allow.
pub struct FailureDomainIdentity<'a> {
    pub provider: &'a str,
    pub dc: &'a str,
    pub cluster: &'a str,
}

/// DNS-label-friendly `<provider>-<dc>-<cluster>` name, capped at 63 chars
/// (the K8s label-value limit) and collision-safe when truncated — see
/// [`crate::k8s_name::collision_safe_name`].
pub fn failure_domain_name(id: &FailureDomainIdentity<'_>) -> String {
    crate::k8s_name::collision_safe_name(&[id.provider, id.dc, id.cluster])
}

/// Fetch the credentials Secret named by `connection.credentials_ref` and
/// pluck `username` / `password`. `pub(crate)` — also used by the
/// `vspheremachine` reconciler (ADR-0024) to connect for the same Provider.
pub(crate) async fn read_credentials(
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
    existing_conditions: &[Condition],
    failure_domains: Vec<FailureDomain>,
) -> Result<()> {
    let mut conditions = existing_conditions.to_vec();
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
        // `status.workload` belongs to banlieue-operator's field manager
        // (ADR-0012). `None` is skipped during serialization, so it never
        // appears in this server-side-apply patch and ownership is untouched.
        workload: None,
        observed_generation: Some(generation),
    };
    patch_provider_status(ctx, namespace, name, status).await
}

/// SSA-patch a failure condition (one of Ready / ProviderReachable) onto
/// `Provider.status.conditions`. Preserves any previously-known failure
/// domains rather than blanking them — that information is still valuable
/// while we recover.
// Internal status-patch helper: the arguments are all irreducible plumbing
// (client, object coordinates, the prior conditions to preserve transition
// times, and the specific failure to record), so the arg count is expected.
#[allow(clippy::too_many_arguments)]
async fn patch_status_failed(
    ctx: &Context,
    namespace: &str,
    name: &str,
    generation: i64,
    existing_conditions: &[Condition],
    condition_type: &str,
    reason: &str,
    message: String,
) -> Result<()> {
    let mut conditions = existing_conditions.to_vec();
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
