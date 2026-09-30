// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `Provider` reconciler for backend class `proxmox`.
//!
//! Connects with the API token, then **verifies** that the storages and
//! bridges the admin declared in `spec.capabilities` exist on each online
//! node and can do what they are declared for, and publishes one failure
//! domain per node (roadmap 06).
//!
//! Capabilities stay *declared, not discovered* (non-negotiable 4): probing
//! narrows the declaration to what is really usable, and reports the rest
//! loudly. A storage that exists but does not allow `images` content is the
//! roadmap's named gotcha, and the `seed-iso` class must allow `iso`.

use std::collections::BTreeMap;
use std::sync::Arc;

use banlieue_api::banlieue::{FailureDomain, FailureDomainAttributes, Provider, ProviderStatus};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_PROVIDER_PROXMOX;
use banlieue_provider_sdk::status::{condition_status, set_condition};
use banlieue_proxmox::{NetworkIface, Node, ProxmoxApi, Storage};
use kube::{
    Resource, ResourceExt,
    api::{Api, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{info, warn};

use crate::context::Context;
use crate::error::{Error, Result};

/// The `ProviderClass` name this provider serves.
pub const PROVIDER_CLASS_NAME: &str = "proxmox";
/// Target key naming a Proxmox storage id.
pub const TARGET_KEY_STORAGE: &str = "storage";
/// Target key naming a Proxmox bridge.
pub const TARGET_KEY_BRIDGE: &str = "bridge";
/// The storage class that holds the NoCloud seed ISO (ADR-0075 Decision 3).
pub const SEED_STORAGE_CLASS: &str = "seed-iso";
/// Content type a disk storage must allow.
const CONTENT_IMAGES: &str = "images";
/// Content type the seed storage must allow.
const CONTENT_ISO: &str = "iso";
/// Failure-domain raw attribute naming the Proxmox node. The controller
/// schedules by this key (`FD_RAW_PROXMOX_NODE`).
pub const RAW_KEY_NODE: &str = "node";

/// Condition types published on a `Provider`.
pub mod condition_types {
    /// Overall readiness.
    pub const READY: &str = "Ready";
    /// The API answered.
    pub const PROVIDER_REACHABLE: &str = "ProviderReachable";
}

/// Stable `reason` strings.
pub mod reasons {
    /// Everything declared is present and usable.
    pub const RECONCILED: &str = "Reconciled";
    /// A declared storage or network class is missing or unusable.
    pub const CAPABILITIES_INCOMPLETE: &str = "CapabilitiesIncomplete";
    /// The API could not be reached or answered with an error.
    pub const CONNECT_FAILED: &str = "ConnectFailed";
    /// The token was refused or lacks the privilege (401/403).
    pub const UNAUTHORIZED: &str = "Unauthorized";
    /// The credentials Secret is missing or malformed.
    pub const CREDENTIALS_UNAVAILABLE: &str = "CredentialsUnavailable";
}

/// Reconcile one `Provider` of class `proxmox`.
///
/// # Errors
/// [`Error`] only if the status patch itself fails; backend and credential
/// failures are published as conditions instead.
pub async fn reconcile(provider: Arc<Provider>, ctx: Arc<Context>) -> Result<Action> {
    let name = provider.name_any();
    let namespace = provider.namespace().unwrap_or_default();
    let generation = provider.metadata.generation.unwrap_or(0);
    let span = tracing::info_span!("reconcile", kind = "Provider", %name, generation);
    let _enter = span.enter();

    if provider.spec.provider_class_ref.name != PROVIDER_CLASS_NAME || provider.spec.paused {
        return Ok(requeue_long());
    }
    info!(endpoint = %provider.spec.connection.endpoint, "reconciling proxmox Provider");

    let outcome = async {
        let creds = crate::credentials::resolve(&ctx.client, &namespace, &provider).await?;
        let api = ctx.proxmox.build(&provider.spec.connection, &creds).await?;
        compute_status(api.as_ref(), &provider, generation).await
    }
    .await;

    match outcome {
        Ok(status) => {
            patch_status(&ctx, &name, &namespace, status).await?;
            Ok(requeue_default())
        }
        Err(e) => {
            let status = failed_status(generation, failure_reason(&e), e.to_string());
            patch_status(&ctx, &name, &namespace, status).await?;
            Ok(requeue_on_error())
        }
    }
}

/// Requeue after a reconcile error.
pub fn error_policy(_p: Arc<Provider>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "proxmox provider reconcile error policy fired");
    requeue_on_error()
}

/// The stable reason for a failure, so operators can match on it.
#[must_use]
pub fn failure_reason(e: &Error) -> &'static str {
    match e {
        Error::Missing(_) => reasons::CREDENTIALS_UNAVAILABLE,
        Error::Proxmox(p) if p.is_unauthorized() => reasons::UNAUTHORIZED,
        _ => reasons::CONNECT_FAILED,
    }
}

/// A declared class resolved against one node.
enum Check {
    Available,
    Problem(String),
}

fn storage_check(class: &str, target: Option<&str>, storages: &[Storage]) -> Check {
    let Some(id) = target else {
        return Check::Problem(format!(
            "storageClass {class}: no `{TARGET_KEY_STORAGE}` target declared"
        ));
    };
    let need = if class == SEED_STORAGE_CLASS {
        CONTENT_ISO
    } else {
        CONTENT_IMAGES
    };
    let Some(storage) = storages.iter().find(|s| s.storage == id) else {
        return Check::Problem(format!("storageClass {class}: storage '{id}' not found"));
    };
    if !storage.enabled || !storage.active {
        return Check::Problem(format!(
            "storageClass {class}: storage '{id}' is disabled or inactive"
        ));
    }
    if !storage.has_content(need) {
        return Check::Problem(format!(
            "storageClass {class}: storage '{id}' does not allow {need} content"
        ));
    }
    Check::Available
}

fn network_check(class: &str, target: Option<&str>, nets: &[NetworkIface]) -> Check {
    let Some(bridge) = target else {
        return Check::Problem(format!(
            "networkClass {class}: no `{TARGET_KEY_BRIDGE}` target declared"
        ));
    };
    if nets.iter().any(|n| n.iface == bridge && n.is_bridge()) {
        return Check::Available;
    }
    Check::Problem(format!("networkClass {class}: bridge '{bridge}' not found"))
}

/// Probe the cluster and compute the `Provider` status.
///
/// One failure domain per **online** node. A declared class is listed on a
/// node only where it is really usable; `Ready` is false when a declared
/// class is usable on no node at all.
///
/// # Errors
/// [`Error::Proxmox`] if the API cannot be read; [`Error::Invalid`] if no
/// node is online.
pub async fn compute_status(
    api: &dyn ProxmoxApi,
    provider: &Provider,
    generation: i64,
) -> Result<ProviderStatus> {
    let version = api.version().await?;
    let nodes: Vec<Node> = api
        .list_nodes()
        .await?
        .into_iter()
        .filter(Node::is_online)
        .collect();
    if nodes.is_empty() {
        return Err(Error::Invalid {
            what: "cluster",
            detail: "no online node".to_string(),
        });
    }

    let caps = &provider.spec.capabilities;
    let provider_name = provider.name_any();
    let mut domains = Vec::with_capacity(nodes.len());
    // class -> every reason it was unusable, for classes usable nowhere.
    let mut storage_reasons: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut network_reasons: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut storage_ok: Vec<String> = Vec::new();
    let mut network_ok: Vec<String> = Vec::new();

    for node in &nodes {
        let storages = api.node_storage(&node.node).await?;
        let nets = api.node_networks(&node.node).await?;
        let mut avail_storage = Vec::new();
        for class in &caps.storage_classes {
            let target = class
                .target_for("", "")
                .and_then(|t| t.get(TARGET_KEY_STORAGE))
                .map(String::as_str);
            match storage_check(&class.name, target, &storages) {
                Check::Available => {
                    avail_storage.push(class.name.clone());
                    storage_ok.push(class.name.clone());
                }
                Check::Problem(why) => storage_reasons
                    .entry(class.name.clone())
                    .or_default()
                    .push(format!("{why} on node {}", node.node)),
            }
        }
        let mut avail_network = Vec::new();
        for class in &caps.network_classes {
            let target = class
                .target_for("", "")
                .and_then(|t| t.get(TARGET_KEY_BRIDGE))
                .map(String::as_str);
            match network_check(&class.name, target, &nets) {
                Check::Available => {
                    avail_network.push(class.name.clone());
                    network_ok.push(class.name.clone());
                }
                Check::Problem(why) => network_reasons
                    .entry(class.name.clone())
                    .or_default()
                    .push(format!("{why} on node {}", node.node)),
            }
        }

        let name = format!("{provider_name}-{}", node.node);
        let mut labels = provider.metadata.labels.clone().unwrap_or_default();
        labels.insert("name".to_string(), name.clone());
        let raw = BTreeMap::from([
            (RAW_KEY_NODE.to_string(), node.node.clone()),
            ("version".to_string(), version.version.clone()),
            (
                "endpoint".to_string(),
                provider.spec.connection.endpoint.clone(),
            ),
        ]);
        domains.push(FailureDomain {
            name,
            labels,
            attributes: FailureDomainAttributes {
                available_storage_classes: avail_storage,
                available_network_classes: avail_network,
                features: caps.features.clone(),
                raw,
            },
        });
    }

    // A class is a problem only when it is usable on no node at all.
    let mut problems: Vec<String> = Vec::new();
    for (class, why) in &storage_reasons {
        if !storage_ok.contains(class) {
            problems.push(why.first().cloned().unwrap_or_default());
        }
    }
    for (class, why) in &network_reasons {
        if !network_ok.contains(class) {
            problems.push(why.first().cloned().unwrap_or_default());
        }
    }

    let mut conditions = Vec::new();
    set_condition(
        &mut conditions,
        condition_types::PROVIDER_REACHABLE,
        condition_status::TRUE,
        reasons::RECONCILED,
        format!(
            "connected to Proxmox VE {}; {} online node(s)",
            version.version,
            nodes.len()
        ),
        generation,
    );
    if problems.is_empty() {
        set_condition(
            &mut conditions,
            condition_types::READY,
            condition_status::TRUE,
            reasons::RECONCILED,
            "all declared capabilities are usable".to_string(),
            generation,
        );
    } else {
        set_condition(
            &mut conditions,
            condition_types::READY,
            condition_status::FALSE,
            reasons::CAPABILITIES_INCOMPLETE,
            format!("declared but not usable: {}", problems.join("; ")),
            generation,
        );
    }

    Ok(ProviderStatus {
        failure_domains: domains,
        conditions,
        workload: None,
        observed_generation: Some(generation),
        ek_ca_certificates: vec![],
    })
}

/// Status published when the cluster could not be probed at all.
#[must_use]
pub fn failed_status(generation: i64, reason: &str, message: String) -> ProviderStatus {
    let mut conditions = Vec::new();
    for t in [condition_types::PROVIDER_REACHABLE, condition_types::READY] {
        set_condition(
            &mut conditions,
            t,
            condition_status::FALSE,
            reason,
            message.clone(),
            generation,
        );
    }
    ProviderStatus {
        failure_domains: Vec::new(),
        conditions,
        workload: None,
        observed_generation: Some(generation),
        ek_ca_certificates: vec![],
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
    let params = PatchParams::apply(FIELD_MANAGER_PROVIDER_PROXMOX).force();
    api.patch_status(name, &params, &Patch::Apply(&patch))
        .await?;
    Ok(())
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;
