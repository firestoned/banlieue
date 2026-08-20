// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `Provider` reconciler for backend class `libvirt`.
//!
//! Connects to the host over mutual TLS, then **verifies** that the storage
//! pools and networks the admin declared in `spec.capabilities` actually exist
//! there, and publishes the result as `status.failureDomains[]`.
//!
//! Capabilities stay *declared, not discovered* (non-negotiable #4): this
//! narrows the admin's declaration to what is really present rather than
//! inventing entries, exactly as the vSphere provider does. A pool named in
//! the spec but absent on the host is dropped from
//! `availableStorageClasses` and reported — which is the whole point of
//! probing at all, since a `Provider` that reports `Ready` without ever having
//! reached the host is actively misleading.
//!
//! A libvirt host is a single failure boundary, so exactly one failure domain
//! is published per `Provider` — unlike vSphere, where a datacenter/cluster
//! hierarchy yields several.

use std::collections::BTreeMap;
use std::sync::Arc;

use banlieue_api::banlieue::{FailureDomain, FailureDomainAttributes, Provider, ProviderStatus};
use banlieue_libvirt::{Network, StoragePool};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_PROVIDER_LIBVIRT;
use banlieue_provider_sdk::status::{condition_status, set_condition};
use kube::{
    Resource, ResourceExt,
    api::{Api, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{info, warn};

use crate::client::LibvirtClient;
use crate::context::Context;
use crate::error::{Error, Result};

/// The `providerClassRef.name` this controller answers to.
pub const PROVIDER_CLASS_NAME: &str = "libvirt";

/// Condition types written onto `Provider.status.conditions`.
pub mod condition_types {
    pub const READY: &str = "Ready";
    pub const PROVIDER_REACHABLE: &str = "ProviderReachable";
}

/// Stable `reason` strings. Operators match on these.
pub mod reasons {
    /// Connected, and every declared capability was found.
    pub const RECONCILED: &str = "Reconciled";
    /// Connected, but some declared pools or networks are missing.
    pub const CAPABILITIES_INCOMPLETE: &str = "CapabilitiesIncomplete";
    /// Could not reach or authenticate to the host.
    pub const CONNECT_FAILED: &str = "ConnectFailed";
    /// The credentials Secret or CA bundle is missing or malformed.
    pub const CREDENTIALS_UNAVAILABLE: &str = "CredentialsUnavailable";
    /// Reconciliation is suspended via `spec.paused`.
    pub const PAUSED: &str = "Paused";
}

/// Key in the credentials Secret holding the PEM client certificate.
pub const SECRET_KEY_TLS_CRT: &str = "tls.crt";
/// Key in the credentials Secret holding the PEM client private key.
pub const SECRET_KEY_TLS_KEY: &str = "tls.key";

/// Top-level reconcile entrypoint.
pub async fn reconcile(provider: Arc<Provider>, ctx: Arc<Context>) -> Result<Action> {
    let name = provider.name_any();
    let namespace = provider.namespace().unwrap_or_default();
    let generation = provider.metadata.generation.unwrap_or(0);

    let span = tracing::info_span!("reconcile", kind = "Provider", name = %name, generation);
    let _enter = span.enter();

    if provider.spec.provider_class_ref.name != PROVIDER_CLASS_NAME {
        // Another provider's concern.
        return Ok(requeue_long());
    }
    if provider.spec.paused {
        info!("provider paused; skipping");
        return Ok(requeue_long());
    }

    info!(endpoint = %provider.spec.connection.endpoint, "reconciling libvirt Provider");

    let identity = match crate::credentials::resolve(&ctx.client, &namespace, &provider).await {
        Ok(id) => id,
        Err(e) => {
            let status = failed_status(generation, reasons::CREDENTIALS_UNAVAILABLE, e.to_string());
            patch_status(&ctx, &name, &namespace, status).await?;
            return Ok(requeue_on_error());
        }
    };

    let client = match ctx
        .libvirt
        .build(&provider.spec.connection, &identity)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            let status = failed_status(generation, reasons::CONNECT_FAILED, e.to_string());
            patch_status(&ctx, &name, &namespace, status).await?;
            return Ok(requeue_on_error());
        }
    };

    let status = match compute_status(client.as_ref(), &provider, generation).await {
        Ok(s) => s,
        Err(e) => {
            let status = failed_status(generation, reasons::CONNECT_FAILED, e.to_string());
            patch_status(&ctx, &name, &namespace, status).await?;
            return Ok(requeue_on_error());
        }
    };

    patch_status(&ctx, &name, &namespace, status).await?;
    Ok(requeue_default())
}

/// `error_policy` invoked on `reconcile` failure.
pub fn error_policy(_p: Arc<Provider>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "libvirt provider reconcile error policy fired");
    requeue_on_error()
}

/// Build the `Provider` status from live inventory.
///
/// Pure with respect to `client`, so tests drive it with a `FakeClient` and
/// never touch kube or a libvirt host.
pub async fn compute_status(
    client: &dyn LibvirtClient,
    provider: &Provider,
    generation: i64,
) -> Result<ProviderStatus> {
    let pools = client.list_pools().await?;
    let networks = client.list_networks().await?;

    let declared_storage: Vec<&str> = provider
        .spec
        .capabilities
        .storage_classes
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    let declared_network: Vec<&str> = provider
        .spec
        .capabilities
        .network_classes
        .iter()
        .map(|c| c.name.as_str())
        .collect();

    let (available_storage, missing_storage) =
        partition_declared(&declared_storage, provider, &pools, Kind::Storage);
    let (available_network, missing_network) =
        partition_declared(&declared_network, provider, &networks, Kind::Network);

    let mut raw = BTreeMap::new();
    raw.insert(
        "endpoint".to_string(),
        provider.spec.connection.endpoint.clone(),
    );
    raw.insert(
        "pools".to_string(),
        join_names(pools.iter().map(|p| p.name.as_str())),
    );
    raw.insert(
        "networks".to_string(),
        join_names(networks.iter().map(|n| n.name.as_str())),
    );

    let name = provider.name_any();
    // A `failureDomainSelector` only ever matches `labels`, never the
    // top-level `name` field — mirroring it here (same contract as the
    // vSphere provider) lets an operator target this specific host by name
    // via `matchLabels: { name: ... }` instead of whatever labels happen to
    // be on the Provider itself. Wins over any user-supplied `name` label
    // on the Provider, since this IS that failure domain's real name.
    let mut labels = provider.metadata.labels.clone().unwrap_or_default();
    labels.insert("name".to_string(), name.clone());

    let fd = FailureDomain {
        // A libvirt host is one failure boundary; name it after the Provider
        // so the identifier is stable and does not leak the endpoint.
        name,
        labels,
        attributes: FailureDomainAttributes {
            available_storage_classes: available_storage,
            available_network_classes: available_network,
            features: provider.spec.capabilities.features.clone(),
            raw,
        },
    };

    let mut conditions = Vec::new();
    set_condition(
        &mut conditions,
        condition_types::PROVIDER_REACHABLE,
        condition_status::TRUE,
        reasons::RECONCILED,
        format!(
            "connected; {} pool(s), {} network(s)",
            pools.len(),
            networks.len()
        ),
        generation,
    );

    let missing: Vec<String> = missing_storage
        .into_iter()
        .map(|n| format!("storageClass {n}"))
        .chain(
            missing_network
                .into_iter()
                .map(|n| format!("networkClass {n}")),
        )
        .collect();

    if missing.is_empty() {
        set_condition(
            &mut conditions,
            condition_types::READY,
            condition_status::TRUE,
            reasons::RECONCILED,
            "all declared capabilities are present on the host".to_string(),
            generation,
        );
    } else {
        // Reachable but incomplete: Ready=False so the scheduler will not
        // place onto capabilities that do not exist.
        set_condition(
            &mut conditions,
            condition_types::READY,
            condition_status::FALSE,
            reasons::CAPABILITIES_INCOMPLETE,
            format!("declared but not found on the host: {}", missing.join(", ")),
            generation,
        );
    }

    Ok(ProviderStatus {
        failure_domains: vec![fd],
        conditions,
        // `status.workload` belongs to banlieue-operator's field manager
        // (ADR-0012); this provider must never write it, or the two managers
        // would contend over the same field.
        workload: None,
        observed_generation: Some(generation),
    })
}

/// Which capability list is being checked.
enum Kind {
    Storage,
    Network,
}

/// Split declared class names into (present, missing) by resolving each to its
/// backend target and checking that target exists on the host.
fn partition_declared<T: HasName>(
    declared: &[&str],
    provider: &Provider,
    actual: &[T],
    kind: Kind,
) -> (Vec<String>, Vec<String>) {
    let mut present = Vec::new();
    let mut missing = Vec::new();
    for name in declared {
        let target = match kind {
            // libvirt's failure domains are one host per Provider today, with
            // no (datacenter, cluster) concept to key a per-zone override on
            // (ADR-0030's out-of-scope note) — the mapping's default `target`
            // is therefore always what applies.
            Kind::Storage => provider
                .spec
                .capabilities
                .storage_classes
                .iter()
                .find(|c| c.name == *name)
                // libvirt storage classes map to a `pool`.
                .and_then(|c| c.target.as_ref())
                .and_then(|t| t.get("pool"))
                .cloned(),
            Kind::Network => provider
                .spec
                .capabilities
                .network_classes
                .iter()
                .find(|c| c.name == *name)
                // libvirt network classes map to a `network`.
                .and_then(|c| c.target.as_ref())
                .and_then(|t| t.get("network"))
                .cloned(),
        };
        match target {
            Some(t) if actual.iter().any(|a| a.name() == t) => present.push((*name).to_string()),
            _ => missing.push((*name).to_string()),
        }
    }
    (present, missing)
}

/// Minimal accessor so pools and networks share [`partition_declared`].
pub trait HasName {
    /// The backend object's name.
    fn name(&self) -> &str;
}
impl HasName for StoragePool {
    fn name(&self) -> &str {
        &self.name
    }
}
impl HasName for Network {
    fn name(&self) -> &str {
        &self.name
    }
}

fn join_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.collect::<Vec<_>>().join(",")
}

/// Status for a provider we could not reach or configure.
///
/// `failureDomains` is left **empty** on failure rather than stale: the
/// scheduler reads it to place VMs, and advertising domains we could not
/// verify would be worse than advertising none.
pub fn failed_status(generation: i64, reason: &str, message: String) -> ProviderStatus {
    let mut conditions = Vec::new();
    set_condition(
        &mut conditions,
        condition_types::PROVIDER_REACHABLE,
        condition_status::FALSE,
        reason,
        message.clone(),
        generation,
    );
    set_condition(
        &mut conditions,
        condition_types::READY,
        condition_status::FALSE,
        reason,
        message,
        generation,
    );
    ProviderStatus {
        failure_domains: Vec::new(),
        conditions,
        // `status.workload` belongs to banlieue-operator's field manager
        // (ADR-0012); this provider must never write it, or the two managers
        // would contend over the same field.
        workload: None,
        observed_generation: Some(generation),
    }
}

async fn patch_status(
    ctx: &Context,
    name: &str,
    namespace: &str,
    status: ProviderStatus,
) -> Result<()> {
    let patch = json!({
        "apiVersion": Provider::api_version(&()).to_string(),
        "kind": Provider::kind(&()).to_string(),
        "metadata": { "name": name },
        "status": status,
    });
    let api: Api<Provider> = Api::namespaced(ctx.client.clone(), namespace);
    let params = PatchParams::apply(FIELD_MANAGER_PROVIDER_LIBVIRT).force();
    api.patch_status(name, &params, &Patch::Apply(&patch))
        .await?;
    Ok(())
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;
