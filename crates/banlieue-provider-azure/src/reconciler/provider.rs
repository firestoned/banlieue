// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `Provider` reconciler for backend class `azure`.
//!
//! Reads the region's resource SKUs and quota usages through ARM on a slow
//! cadence ([`PROVIDER_REQUEUE`]) and publishes one failure domain per
//! availability zone in which at least one mapped instance size is usable
//! by this subscription (ADR-0158 Decision 3, ADR-0160 Decision 2): offered
//! there and not restricted. Never a fixed list of three.
//!
//! Each failure domain carries:
//!
//! - `name` `<provider>-<zone>` (hashed to 63 characters when longer), and
//!   the labels `name` and `zone` beside the `Provider`'s own labels;
//! - `attributes.raw`: `zone` (the subscription's zone name, which the
//!   controller copies into `AzureVirtualMachine.spec.zone`), `location`,
//!   `resourceGroup`, `endpoint`, and `vmClasses`, the comma-separated
//!   `VMClass`es whose mapped size is usable in that zone;
//! - the storage classes whose target in that zone names a `sku`, and the
//!   network classes whose target names a `subnetId`;
//! - `capacity.cpus`: quota **headroom** in vCPUs (ADR-0161 Decision 1),
//!   the smallest of the regional headroom and the family headroom of every
//!   mapped size usable in the zone. Regional quota is shared by every zone,
//!   so each zone publishes the same regional bound; the scheduler must not
//!   add the zones together. It is what remains, not a total: usage already
//!   counts the VMs running. Memory and disk are not quota-limited.
//!
//! A mapped size usable in no zone, or a declared class usable in no zone,
//! sets `Ready=False` (`CapabilitiesIncomplete`), as the Proxmox provider
//! does for its classes; zero headroom does not (ADR-0161 Decision 3).
//! Throttling keeps the last published failure domains and says so in a
//! condition. An unreachable or refusing ARM publishes none (the SDK's
//! `unreachable_provider_status`).
//!
//! The pass also triggers the orphan sweep (ADR-0158 Decision 5) on its own
//! slower clock ([`super::resources::SWEEP_INTERVAL`]).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use banlieue_api::banlieue::{
    FailureDomain, FailureDomainAttributes, FailureDomainCapacity, Provider, ProviderStatus,
};
use banlieue_api::infrastructure::AzureVirtualMachine;
use banlieue_azure::ArmApi;
use banlieue_azure::types::{ResourceSku, Usage, vcpu_headroom};
use banlieue_provider_sdk::naming::truncate_with_hash;
use banlieue_provider_sdk::reconciler::{requeue_long, requeue_on_error};
use banlieue_provider_sdk::scope::ProviderScope;
use banlieue_provider_sdk::ssa::{FIELD_MANAGER_PROVIDER_AZURE, apply_status};
use banlieue_provider_sdk::status::{
    condition_status, provider_condition_types as condition_types, set_condition,
    unreachable_provider_status,
};
use kube::api::ListParams;
use kube::{ResourceExt, api::Api, runtime::controller::Action};
use tracing::{info, warn};

use crate::client::azure_connection;
use crate::context::Context;
use crate::error::{Error, Result};
use crate::reconciler::resources::{SWEEP_INTERVAL, sweep};

/// The `ProviderClass` name this provider serves.
pub const PROVIDER_CLASS_NAME: &str = "azure";
/// Storage target key: the managed-disk SKU (ADR-0160 Decision 4).
pub const TARGET_KEY_SKU: &str = "sku";
/// Network target key: the subnet resource ID.
pub const TARGET_KEY_SUBNET_ID: &str = "subnetId";
/// Network target key: `"true"` enables accelerated networking.
pub const TARGET_KEY_ACCELERATED_NETWORKING: &str = "acceleratedNetworking";
/// Failure-domain label and raw key naming the availability zone.
pub const KEY_ZONE: &str = "zone";
/// Failure-domain label mirroring the failure domain's own name.
pub const LABEL_NAME: &str = "name";
/// Raw key: the region.
pub const RAW_KEY_LOCATION: &str = "location";
/// Raw key: the resource group.
pub const RAW_KEY_RESOURCE_GROUP: &str = "resourceGroup";
/// Raw key: the ARM endpoint.
pub const RAW_KEY_ENDPOINT: &str = "endpoint";
/// Raw key: comma-separated `VMClass`es usable in this zone.
pub const RAW_KEY_VM_CLASSES: &str = "vmClasses";
/// How often SKUs and quota are read again (ADR-0160, ADR-0161).
pub const PROVIDER_REQUEUE: Duration = Duration::from_secs(600);
const LIST_SEPARATOR: &str = ",";

/// Stable `reason` strings.
pub mod reasons {
    /// Everything declared is usable somewhere.
    pub const RECONCILED: &str = "Reconciled";
    /// A mapped size or declared class is usable in no zone.
    pub const CAPABILITIES_INCOMPLETE: &str = "CapabilitiesIncomplete";
    /// ARM throttled the read; the last failure domains are kept.
    pub const THROTTLED: &str = "Throttled";
    /// ARM or Entra ID could not be reached, or answered with an error.
    pub const CONNECT_FAILED: &str = "ConnectFailed";
    /// The token exchange or ARM refused the identity (401/403).
    pub const UNAUTHORIZED: &str = "Unauthorized";
    /// `spec.connection.azure` is missing or unusable.
    pub const CONNECTION_INVALID: &str = "ConnectionInvalid";
}

/// The stable reason for a failure, so operators can match on it.
#[must_use]
pub fn failure_reason(e: &Error) -> &'static str {
    match e {
        Error::Invalid { .. } | Error::Missing(_) => reasons::CONNECTION_INVALID,
        Error::Azure(
            banlieue_azure::Error::Unauthorized { .. }
            | banlieue_azure::Error::Forbidden { .. }
            | banlieue_azure::Error::TokenExchange { .. },
        ) => reasons::UNAUTHORIZED,
        Error::Azure(banlieue_azure::Error::InvalidRequest { .. }) => reasons::CONNECTION_INVALID,
        _ => reasons::CONNECT_FAILED,
    }
}

/// The failure domain name for `zone` of `provider`: `<provider>-<zone>`,
/// hashed down to 63 characters (a label value) when longer.
#[must_use]
pub fn failure_domain_name(provider: &str, zone: &str) -> String {
    truncate_with_hash(&format!("{provider}-{zone}"))
}

fn find_sku<'a>(skus: &'a [ResourceSku], size: &str) -> Option<&'a ResourceSku> {
    skus.iter().find(|s| s.name.eq_ignore_ascii_case(size))
}

fn usable_in(skus: &[ResourceSku], size: &str, zone: &str) -> bool {
    find_sku(skus, size).is_some_and(|s| s.usable_zones().iter().any(|z| z == zone))
}

/// Every zone in which some mapped size (or per-zone override) is usable.
fn candidate_zones(provider: &Provider, skus: &[ResourceSku]) -> BTreeSet<String> {
    provider
        .spec
        .capabilities
        .instance_sizes
        .iter()
        .flat_map(|m| {
            std::iter::once(m.size.as_str()).chain(m.per_zone.iter().map(|z| z.size.as_str()))
        })
        .filter_map(|size| find_sku(skus, size))
        .flat_map(ResourceSku::usable_zones)
        .collect()
}

/// The quota headroom in `zone`: the smallest headroom across the families
/// of `sizes`, each already bounded by the regional headroom.
fn zone_headroom(skus: &[ResourceSku], usages: &[Usage], sizes: &[&str]) -> Option<u32> {
    sizes
        .iter()
        .filter_map(|size| find_sku(skus, size))
        .filter_map(|sku| vcpu_headroom(usages, &sku.family))
        .min()
        .map(|h| u32::try_from(h).unwrap_or(u32::MAX))
}

fn target_has(target: Option<&BTreeMap<String, String>>, key: &str) -> bool {
    target
        .and_then(|t| t.get(key))
        .is_some_and(|v| !v.trim().is_empty())
}

/// The `Provider` status for the SKUs and usages ARM returned. Pure.
#[must_use]
pub fn compute_status(
    provider: &Provider,
    skus: &[ResourceSku],
    usages: &[Usage],
    generation: i64,
) -> ProviderStatus {
    let caps = &provider.spec.capabilities;
    let provider_name = provider.name_any();
    let azure = provider.spec.connection.azure.clone().unwrap_or_default();
    let mut domains = Vec::new();
    let mut classes_seen: BTreeSet<String> = BTreeSet::new();
    let mut storage_seen: BTreeSet<String> = BTreeSet::new();
    let mut network_seen: BTreeSet<String> = BTreeSet::new();

    for zone in candidate_zones(provider, skus) {
        let usable: Vec<(&str, &str)> = caps
            .instance_sizes
            .iter()
            .map(|m| (m.vm_class.as_str(), m.size_for_zone(&zone)))
            .filter(|(_, size)| usable_in(skus, size, &zone))
            .collect();
        if usable.is_empty() {
            continue;
        }
        let vm_classes: Vec<&str> = usable.iter().map(|(class, _)| *class).collect();
        let sizes: Vec<&str> = usable.iter().map(|(_, size)| *size).collect();
        classes_seen.extend(vm_classes.iter().map(|c| (*c).to_string()));

        let storage: Vec<String> = caps
            .storage_classes
            .iter()
            .filter(|c| target_has(c.target_for_zone(&zone), TARGET_KEY_SKU))
            .map(|c| c.name.clone())
            .collect();
        let network: Vec<String> = caps
            .network_classes
            .iter()
            .filter(|c| target_has(c.target_for_zone(&zone), TARGET_KEY_SUBNET_ID))
            .map(|c| c.name.clone())
            .collect();
        storage_seen.extend(storage.iter().cloned());
        network_seen.extend(network.iter().cloned());

        let name = failure_domain_name(&provider_name, &zone);
        let mut labels = provider.metadata.labels.clone().unwrap_or_default();
        labels.insert(LABEL_NAME.to_string(), name.clone());
        labels.insert(KEY_ZONE.to_string(), zone.clone());
        let raw = BTreeMap::from([
            (KEY_ZONE.to_string(), zone.clone()),
            (RAW_KEY_LOCATION.to_string(), azure.location.clone()),
            (
                RAW_KEY_RESOURCE_GROUP.to_string(),
                azure.resource_group.clone(),
            ),
            (
                RAW_KEY_ENDPOINT.to_string(),
                provider.spec.connection.endpoint.clone(),
            ),
            (
                RAW_KEY_VM_CLASSES.to_string(),
                vm_classes.join(LIST_SEPARATOR),
            ),
        ]);
        let capacity = zone_headroom(skus, usages, &sizes).map(|cpus| FailureDomainCapacity {
            cpus: Some(cpus),
            ..FailureDomainCapacity::default()
        });
        domains.push(FailureDomain {
            name,
            labels,
            attributes: FailureDomainAttributes {
                available_storage_classes: storage,
                available_network_classes: network,
                features: caps.features.clone(),
                raw,
                capacity,
            },
        });
    }

    let mut problems: Vec<String> = Vec::new();
    if caps.instance_sizes.is_empty() {
        problems.push("no instanceSizes declared; no VMClass can run here".to_string());
    }
    for mapping in &caps.instance_sizes {
        if classes_seen.contains(&mapping.vm_class) {
            continue;
        }
        let why = if find_sku(skus, &mapping.size).is_none() {
            format!("is not offered in {}", azure.location)
        } else {
            "is restricted for this subscription in every zone".to_string()
        };
        problems.push(format!(
            "instanceSize {} for VMClass {} {why}",
            mapping.size, mapping.vm_class
        ));
    }
    for class in &caps.storage_classes {
        if !storage_seen.contains(&class.name) {
            problems.push(format!(
                "storageClass {}: no `{TARGET_KEY_SKU}` target in any usable zone",
                class.name
            ));
        }
    }
    for class in &caps.network_classes {
        if !network_seen.contains(&class.name) {
            problems.push(format!(
                "networkClass {}: no `{TARGET_KEY_SUBNET_ID}` target in any usable zone",
                class.name
            ));
        }
    }

    let mut conditions = Vec::new();
    set_condition(
        &mut conditions,
        condition_types::PROVIDER_REACHABLE,
        condition_status::TRUE,
        reasons::RECONCILED,
        format!(
            "read {} size(s) and {} quota line(s) in {}; {} usable zone(s)",
            skus.len(),
            usages.len(),
            azure.location,
            domains.len()
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

    ProviderStatus {
        failure_domains: domains,
        conditions,
        workload: None,
        observed_generation: Some(generation),
        ek_ca_certificates: vec![],
    }
}

/// The status while ARM throttles the provider: the last failure domains,
/// unchanged, with `Ready=False` (`Throttled`) saying why they are not fresh.
#[must_use]
pub fn throttled_status(
    prior: Option<&ProviderStatus>,
    retry_after: Duration,
    generation: i64,
) -> ProviderStatus {
    let mut status = prior.cloned().unwrap_or_default();
    status.workload = None;
    set_condition(
        &mut status.conditions,
        condition_types::READY,
        condition_status::FALSE,
        reasons::THROTTLED,
        format!(
            "Azure Resource Manager throttled the SKU and quota read; retrying after {}s with the last failure domains",
            retry_after.as_secs()
        ),
        generation,
    );
    status.observed_generation = Some(generation);
    status
}

/// Read SKUs and usages for the `Provider`'s region and compute its status.
///
/// # Errors
/// [`Error::Invalid`] without `connection.azure`; [`Error::Azure`] when ARM
/// cannot be read.
pub async fn probe(
    api: &dyn ArmApi,
    provider: &Provider,
    generation: i64,
) -> Result<ProviderStatus> {
    let location = azure_connection(&provider.spec.connection)?
        .location
        .clone();
    let skus = api.resource_skus(&location).await?;
    let usages = api.usages(&location).await?;
    Ok(compute_status(provider, &skus, &usages, generation))
}

/// Reconcile one `Provider` of class `azure`.
///
/// # Errors
/// [`Error`] only if the status patch itself fails; ARM and identity
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
    info!(endpoint = %provider.spec.connection.endpoint, "reconciling azure Provider");

    let api = match ctx.arm.build(&provider.spec.connection).await {
        Ok(api) => api,
        Err(e) => {
            let status = unreachable_provider_status(generation, failure_reason(&e), e.to_string());
            patch_status(&ctx, &name, &namespace, status).await?;
            return Ok(requeue_on_error());
        }
    };

    match probe(api.as_ref(), &provider, generation).await {
        Ok(status) => patch_status(&ctx, &name, &namespace, status).await?,
        Err(Error::Azure(banlieue_azure::Error::Throttled { retry_after })) => {
            let status = throttled_status(provider.status.as_ref(), retry_after, generation);
            patch_status(&ctx, &name, &namespace, status).await?;
            return Ok(Action::requeue(retry_after));
        }
        Err(e) => {
            warn!(error = %e, "azure Provider probe failed");
            let status = unreachable_provider_status(generation, failure_reason(&e), e.to_string());
            patch_status(&ctx, &name, &namespace, status).await?;
            return Ok(requeue_on_error());
        }
    }

    let key = format!("{namespace}.{name}");
    if ctx.sweep_clock.due(&key, Instant::now(), SWEEP_INTERVAL) {
        run_sweep(&ctx, api.as_ref(), &provider, &key).await;
    }
    Ok(Action::requeue(PROVIDER_REQUEUE))
}

/// The namespace the sweep must stay inside: `None` when this provider lists
/// machines cluster-wide (a shared provider), else the one namespace its
/// listing covers. A machine outside that namespace is invisible to the
/// listing, so its resources must never be judged (ADR-0158 Decision 5).
pub(crate) fn sweep_namespace<'a>(
    scope: Option<&'a ProviderScope>,
    home: &'a str,
) -> Option<&'a str> {
    match scope {
        Some(scope) if scope.selector().is_some() => None,
        _ => Some(home),
    }
}

/// The UID of every `AzureVirtualMachine` this provider can see, deleting
/// ones included. Listed through the provider's scope, the same API its
/// machine watch uses.
async fn live_machine_uids(ctx: &Context, home: &str) -> Result<BTreeSet<String>> {
    let api: Api<AzureVirtualMachine> = match ctx.scope() {
        Some(scope) => scope.api(&ctx.client),
        None => Api::namespaced(ctx.client.clone(), home),
    };
    let listed = api.list(&ListParams::default()).await?;
    Ok(listed.items.iter().filter_map(ResourceExt::uid).collect())
}

/// One orphan sweep for `provider`. Never fails the reconcile: a sweep that
/// cannot list or delete waits for the next pass, and a failed machine
/// listing skips the pass entirely, since every UID missing from it would
/// read as an orphan.
async fn run_sweep(ctx: &Context, api: &dyn ArmApi, provider: &Provider, key: &str) {
    let Ok(azure) = azure_connection(&provider.spec.connection) else {
        return;
    };
    let home = provider.namespace().unwrap_or_default();
    let live = match live_machine_uids(ctx, &home).await {
        Ok(live) => live,
        Err(e) => {
            warn!(error = %e, "orphan sweep skipped: cannot list AzureVirtualMachines");
            return;
        }
    };
    match sweep(
        api,
        &azure.resource_group,
        key,
        &live,
        sweep_namespace(ctx.scope(), &home),
        jiff::Timestamp::now(),
    )
    .await
    {
        Ok(report) => info!(
            deleted = report.deleted.len(),
            failed = report.failed.len(),
            deferred = report.deferred,
            throttled = report.throttled.is_some(),
            "orphan sweep finished"
        ),
        Err(e) => warn!(error = %e, "orphan sweep skipped: cannot list the resource group"),
    }
}

async fn patch_status(
    ctx: &Context,
    name: &str,
    namespace: &str,
    status: ProviderStatus,
) -> Result<()> {
    let api: Api<Provider> = Api::namespaced(ctx.client.clone(), namespace);
    apply_status(&api, name, FIELD_MANAGER_PROVIDER_AZURE, &status, true).await?;
    Ok(())
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;
