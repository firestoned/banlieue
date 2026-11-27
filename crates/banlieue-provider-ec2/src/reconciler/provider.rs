// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `Provider` reconciler for backend class `ec2`.
//!
//! Reads the region's availability zones and where each mapped instance type
//! is offered, on a slow cadence ([`PROVIDER_REQUEUE`]), and publishes one
//! failure domain per zone that offers at least one mapped instance type
//! (ADR-0158 Decision 3, ADR-0160). Never a fixed list of three.
//!
//! Each failure domain carries:
//!
//! - `name` `<provider>-<zone name>` (hashed to 63 characters when longer),
//!   and the labels `name` and `zone` beside the `Provider`'s own labels;
//! - `attributes.raw`: `zone` (this account's zone **name**, which the
//!   controller copies into `EC2Machine.spec.zone`), `zoneId` (the global
//!   zone ID, so two accounts compare, ADR-0158 Decision 3), `region`,
//!   `endpoint`, `vmClasses` (the comma-separated `VMClass`es whose mapped
//!   type is offered in that zone) and `tpmVmClasses` (those whose type
//!   supports NitroTPM 2.0 and UEFI boot, ADR-0167 Decision 5);
//! - the storage classes whose target in that zone names a `volumeType`, and
//!   the network classes whose target names a `subnetId` and
//!   `securityGroupIds`.
//!
//! **No capacity is published yet.** EC2's vCPU quota lives in Service
//! Quotas (`L-1216C47A`, ADR-0174 finding 8), which `banlieue-aws` does not
//! call yet; a failure domain without `capacity` is unbounded to the
//! scheduler, and a create the quota refuses is reported on the machine
//! (`QuotaExceeded`).
//!
//! A zone EC2 reports `unavailable` or `impaired` publishes no failure
//! domain. A mapped type offered in no zone, or a declared class usable in
//! no zone, sets `Ready=False` (`CapabilitiesIncomplete`). Throttling keeps
//! the last published failure domains and says so in a condition. An
//! unreachable or refusing EC2 (or STS) publishes none (the SDK's
//! `unreachable_provider_status`).
//!
//! The pass also triggers the orphan sweep (ADR-0158 Decision 5) on its own
//! slower clock ([`super::instances::SWEEP_INTERVAL`]).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use banlieue_api::banlieue::{
    FailureDomain, FailureDomainAttributes, InstanceSizeMapping, Provider, ProviderStatus,
};
use banlieue_api::infrastructure::EC2Machine;
use banlieue_aws::Ec2Api;
use banlieue_aws::instance::MAX_DESCRIBE_INSTANCE_TYPES;
use banlieue_aws::types::{AvailabilityZone, InstanceTypeInfo, InstanceTypeOffering};
use banlieue_provider_sdk::naming::truncate_with_hash;
use banlieue_provider_sdk::reconciler::{requeue_long, requeue_on_error};
use banlieue_provider_sdk::scope::ProviderScope;
use banlieue_provider_sdk::ssa::{FIELD_MANAGER_PROVIDER_EC2, apply_status};
use banlieue_provider_sdk::status::{
    condition_status, provider_condition_types as condition_types, set_condition,
    unreachable_provider_status,
};
use kube::api::ListParams;
use kube::{ResourceExt, api::Api, runtime::controller::Action};
use tracing::{info, warn};

use crate::client::ec2_connection;
use crate::context::Context;
use crate::error::{Error, Result};
use crate::reconciler::instances::{SWEEP_INTERVAL, sweep};

/// The `ProviderClass` name this provider serves.
pub const PROVIDER_CLASS_NAME: &str = "ec2";
/// Storage target key: the EBS volume type (ADR-0160 Decision 4).
pub const TARGET_KEY_VOLUME_TYPE: &str = "volumeType";
/// Network target key: the subnet ID (zonal).
pub const TARGET_KEY_SUBNET_ID: &str = "subnetId";
/// Network target key: the comma-separated security group IDs.
pub const TARGET_KEY_SECURITY_GROUP_IDS: &str = "securityGroupIds";
/// Failure-domain label and raw key naming the zone (this account's name).
pub const KEY_ZONE: &str = "zone";
/// Failure-domain label mirroring the failure domain's own name.
pub const LABEL_NAME: &str = "name";
/// Raw key: the zone ID, the same in every account.
pub const RAW_KEY_ZONE_ID: &str = "zoneId";
/// Raw key: the region.
pub const RAW_KEY_REGION: &str = "region";
/// Raw key: the EC2 endpoint.
pub const RAW_KEY_ENDPOINT: &str = "endpoint";
/// Raw key: comma-separated `VMClass`es offered in this zone.
pub const RAW_KEY_VM_CLASSES: &str = "vmClasses";
/// Raw key: comma-separated `VMClass`es offered in this zone whose type
/// supports NitroTPM 2.0 and UEFI boot (ADR-0167 Decision 5).
pub const RAW_KEY_TPM_VM_CLASSES: &str = "tpmVmClasses";
/// How often zones and offerings are read again (ADR-0160).
pub const PROVIDER_REQUEUE: Duration = Duration::from_secs(600);
/// How long a throttled read waits: the client already retried it with
/// jittered back-off.
pub const THROTTLED_REQUEUE: Duration = Duration::from_secs(60);
/// Zone states that publish no failure domain.
const UNUSABLE_ZONE_STATES: [&str; 2] = ["unavailable", "impaired"];
/// The NitroTPM version a `tpmEnabled` class needs.
const NITRO_TPM_VERSION: &str = "2.0";
const LIST_SEPARATOR: &str = ",";

/// Stable `reason` strings.
pub mod reasons {
    /// Everything declared is usable somewhere.
    pub const RECONCILED: &str = "Reconciled";
    /// A mapped type or declared class is usable in no zone.
    pub const CAPABILITIES_INCOMPLETE: &str = "CapabilitiesIncomplete";
    /// EC2 throttled the read; the last failure domains are kept.
    pub const THROTTLED: &str = "Throttled";
    /// EC2 or STS could not be reached, or answered with an error.
    pub const CONNECT_FAILED: &str = "ConnectFailed";
    /// STS or EC2 refused the identity or the action.
    pub const UNAUTHORIZED: &str = "Unauthorized";
    /// `spec.connection.ec2` is missing or unusable.
    pub const CONNECTION_INVALID: &str = "ConnectionInvalid";
}

/// The stable reason for a failure, so operators can match on it.
#[must_use]
pub fn failure_reason(e: &Error) -> &'static str {
    match e {
        Error::Invalid { .. } | Error::Missing(_) => reasons::CONNECTION_INVALID,
        Error::Aws(banlieue_aws::Error::Forbidden(_) | banlieue_aws::Error::Unauthenticated(_)) => {
            reasons::UNAUTHORIZED
        }
        Error::Aws(banlieue_aws::Error::Invalid(_)) => reasons::CONNECTION_INVALID,
        _ => reasons::CONNECT_FAILED,
    }
}

/// The failure domain name for `zone` of `provider`: `<provider>-<zone>`,
/// hashed down to 63 characters (a label value) when longer.
#[must_use]
pub fn failure_domain_name(provider: &str, zone: &str) -> String {
    truncate_with_hash(&format!("{provider}-{zone}"))
}

/// Every instance type the `Provider` maps, per-zone overrides included,
/// sorted and without repeats.
#[must_use]
pub fn mapped_types(sizes: &[InstanceSizeMapping]) -> Vec<String> {
    sizes
        .iter()
        .flat_map(|m| {
            std::iter::once(m.size.clone()).chain(m.per_zone.iter().map(|z| z.size.clone()))
        })
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

/// Whether `info` can run a `tpmEnabled` machine: NitroTPM 2.0 and UEFI.
fn supports_nitro_tpm(info: &InstanceTypeInfo) -> bool {
    info.nitro_tpm_supported
        && info.supports_uefi()
        && info
            .nitro_tpm_versions
            .iter()
            .any(|v| v == NITRO_TPM_VERSION)
}

fn target_has(target: Option<&BTreeMap<String, String>>, key: &str) -> bool {
    target
        .and_then(|t| t.get(key))
        .is_some_and(|v| !v.trim().is_empty())
}

/// What EC2 said about the region, read by [`probe`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegionView {
    /// `DescribeAvailabilityZones`.
    pub zones: Vec<AvailabilityZone>,
    /// `DescribeInstanceTypeOfferings` by zone ID for the mapped types.
    pub offerings: Vec<InstanceTypeOffering>,
    /// `DescribeInstanceTypes` for the mapped types offered somewhere.
    pub types: Vec<InstanceTypeInfo>,
}

impl RegionView {
    fn offered(&self, instance_type: &str, zone_id: &str) -> bool {
        self.offerings
            .iter()
            .any(|o| o.instance_type == instance_type && o.location == zone_id)
    }

    fn info(&self, instance_type: &str) -> Option<&InstanceTypeInfo> {
        self.types.iter().find(|t| t.instance_type == instance_type)
    }
}

/// The `Provider` status for what EC2 returned. Pure.
#[must_use]
pub fn compute_status(provider: &Provider, region: &RegionView, generation: i64) -> ProviderStatus {
    let caps = &provider.spec.capabilities;
    let provider_name = provider.name_any();
    let ec2 = provider.spec.connection.ec2.clone().unwrap_or_default();
    let mut zones: Vec<&AvailabilityZone> = region
        .zones
        .iter()
        .filter(|z| !UNUSABLE_ZONE_STATES.contains(&z.state.as_str()))
        .collect();
    zones.sort_by(|a, b| a.name.cmp(&b.name));
    let mut domains = Vec::new();
    let mut classes_seen: BTreeSet<String> = BTreeSet::new();
    let mut storage_seen: BTreeSet<String> = BTreeSet::new();
    let mut network_seen: BTreeSet<String> = BTreeSet::new();

    for zone in zones {
        let usable: Vec<(&str, &str)> = caps
            .instance_sizes
            .iter()
            .map(|m| (m.vm_class.as_str(), m.size_for_zone(&zone.name)))
            .filter(|(_, t)| region.offered(t, &zone.id))
            .collect();
        if usable.is_empty() {
            continue;
        }
        let vm_classes: Vec<&str> = usable.iter().map(|(class, _)| *class).collect();
        let tpm_classes: Vec<&str> = usable
            .iter()
            .filter(|(_, t)| region.info(t).is_some_and(supports_nitro_tpm))
            .map(|(class, _)| *class)
            .collect();
        classes_seen.extend(vm_classes.iter().map(|c| (*c).to_string()));

        let storage: Vec<String> = caps
            .storage_classes
            .iter()
            .filter(|c| target_has(c.target_for_zone(&zone.name), TARGET_KEY_VOLUME_TYPE))
            .map(|c| c.name.clone())
            .collect();
        let network: Vec<String> = caps
            .network_classes
            .iter()
            .filter(|c| {
                let t = c.target_for_zone(&zone.name);
                target_has(t, TARGET_KEY_SUBNET_ID) && target_has(t, TARGET_KEY_SECURITY_GROUP_IDS)
            })
            .map(|c| c.name.clone())
            .collect();
        storage_seen.extend(storage.iter().cloned());
        network_seen.extend(network.iter().cloned());

        let name = failure_domain_name(&provider_name, &zone.name);
        let mut labels = provider.metadata.labels.clone().unwrap_or_default();
        labels.insert(LABEL_NAME.to_string(), name.clone());
        labels.insert(KEY_ZONE.to_string(), zone.name.clone());
        let raw = BTreeMap::from([
            (KEY_ZONE.to_string(), zone.name.clone()),
            (RAW_KEY_ZONE_ID.to_string(), zone.id.clone()),
            (RAW_KEY_REGION.to_string(), ec2.region.clone()),
            (
                RAW_KEY_ENDPOINT.to_string(),
                provider.spec.connection.endpoint.clone(),
            ),
            (
                RAW_KEY_VM_CLASSES.to_string(),
                vm_classes.join(LIST_SEPARATOR),
            ),
            (
                RAW_KEY_TPM_VM_CLASSES.to_string(),
                tpm_classes.join(LIST_SEPARATOR),
            ),
        ]);
        domains.push(FailureDomain {
            name,
            labels,
            attributes: FailureDomainAttributes {
                available_storage_classes: storage,
                available_network_classes: network,
                features: caps.features.clone(),
                raw,
                capacity: None,
            },
        });
    }

    let mut problems: Vec<String> = Vec::new();
    if caps.instance_sizes.is_empty() {
        problems.push("no instanceSizes declared; no VMClass can run here".to_string());
    }
    for mapping in &caps.instance_sizes {
        if !classes_seen.contains(&mapping.vm_class) {
            problems.push(format!(
                "instanceSize {} for VMClass {} is not offered in any usable zone of {}",
                mapping.size, mapping.vm_class, ec2.region
            ));
        }
    }
    for class in &caps.storage_classes {
        if !storage_seen.contains(&class.name) {
            problems.push(format!(
                "storageClass {}: no `{TARGET_KEY_VOLUME_TYPE}` target in any usable zone",
                class.name
            ));
        }
    }
    for class in &caps.network_classes {
        if !network_seen.contains(&class.name) {
            problems.push(format!(
                "networkClass {}: no `{TARGET_KEY_SUBNET_ID}` and `{TARGET_KEY_SECURITY_GROUP_IDS}` target in any usable zone",
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
            "read {} zone(s) and {} offering(s) in {}; {} usable zone(s)",
            region.zones.len(),
            region.offerings.len(),
            ec2.region,
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

/// The status while EC2 throttles the provider: the last failure domains,
/// unchanged, with `Ready=False` (`Throttled`) saying why they are not fresh.
#[must_use]
pub fn throttled_status(prior: Option<&ProviderStatus>, generation: i64) -> ProviderStatus {
    let mut status = prior.cloned().unwrap_or_default();
    status.workload = None;
    set_condition(
        &mut status.conditions,
        condition_types::READY,
        condition_status::FALSE,
        reasons::THROTTLED,
        format!(
            "EC2 throttled the zone and offering read; retrying in {}s with the last failure domains",
            THROTTLED_REQUEUE.as_secs()
        ),
        generation,
    );
    status.observed_generation = Some(generation);
    status
}

/// Reads zones, offerings and type details for the `Provider`'s mapped
/// types. Types offered nowhere are not described: EC2 refuses a type it
/// does not know, and one mistyped mapping must not hide every zone.
///
/// # Errors
/// The first EC2 error.
pub async fn read_region(
    api: &dyn Ec2Api,
    provider: &Provider,
) -> banlieue_aws::Result<RegionView> {
    let zones = api.availability_zones().await?;
    let types = mapped_types(&provider.spec.capabilities.instance_sizes);
    if types.is_empty() {
        return Ok(RegionView {
            zones,
            ..RegionView::default()
        });
    }
    let offerings = api.instance_type_offerings(&types).await?;
    let offered: Vec<String> = offerings
        .iter()
        .map(|o| o.instance_type.clone())
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    let mut infos = Vec::new();
    for chunk in offered.chunks(MAX_DESCRIBE_INSTANCE_TYPES) {
        infos.extend(api.instance_types(chunk).await?);
    }
    Ok(RegionView {
        zones,
        offerings,
        types: infos,
    })
}

/// Read the region for the `Provider` and compute its status.
///
/// # Errors
/// [`Error::Invalid`] without `connection.ec2`; [`Error::Aws`] when EC2
/// cannot be read.
pub async fn probe(
    api: &dyn Ec2Api,
    provider: &Provider,
    generation: i64,
) -> Result<ProviderStatus> {
    ec2_connection(&provider.spec.connection)?;
    let region = read_region(api, provider).await?;
    Ok(compute_status(provider, &region, generation))
}

/// Reconcile one `Provider` of class `ec2`.
///
/// # Errors
/// [`Error`] only if the status patch itself fails; EC2 and identity
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
    info!(endpoint = %provider.spec.connection.endpoint, "reconciling ec2 Provider");

    let api = match ctx.ec2.build(&provider).await {
        Ok(api) => api,
        Err(e) => {
            let status = unreachable_provider_status(generation, failure_reason(&e), e.to_string());
            patch_status(&ctx, &name, &namespace, status).await?;
            return Ok(requeue_on_error());
        }
    };

    match probe(api.as_ref(), &provider, generation).await {
        Ok(status) => patch_status(&ctx, &name, &namespace, status).await?,
        Err(Error::Aws(e)) if e.is_throttled() => {
            let status = throttled_status(provider.status.as_ref(), generation);
            patch_status(&ctx, &name, &namespace, status).await?;
            return Ok(Action::requeue(THROTTLED_REQUEUE));
        }
        Err(e) => {
            warn!(error = %e, "ec2 Provider probe failed");
            let status = unreachable_provider_status(generation, failure_reason(&e), e.to_string());
            patch_status(&ctx, &name, &namespace, status).await?;
            return Ok(requeue_on_error());
        }
    }

    let key = format!("{namespace}.{name}");
    if ctx.sweep_clock.due(&key, Instant::now(), SWEEP_INTERVAL) {
        run_sweep(&ctx, api.as_ref(), &namespace, &key).await;
    }
    Ok(Action::requeue(PROVIDER_REQUEUE))
}

/// The namespace the sweep must stay inside: `None` when this provider lists
/// machines cluster-wide (a shared provider), else the one namespace its
/// listing covers. A machine outside that namespace is invisible to the
/// listing, so its instances must never be judged (ADR-0158 Decision 5).
pub(crate) fn sweep_namespace<'a>(
    scope: Option<&'a ProviderScope>,
    home: &'a str,
) -> Option<&'a str> {
    match scope {
        Some(scope) if scope.selector().is_some() => None,
        _ => Some(home),
    }
}

/// The UID of every `EC2Machine` this provider can see, deleting ones
/// included. Listed through the provider's scope, the same API its machine
/// watch uses.
async fn live_machine_uids(ctx: &Context, home: &str) -> Result<BTreeSet<String>> {
    let api: Api<EC2Machine> = match ctx.scope() {
        Some(scope) => scope.api(&ctx.client),
        None => Api::namespaced(ctx.client.clone(), home),
    };
    let listed = api.list(&ListParams::default()).await?;
    Ok(listed.items.iter().filter_map(ResourceExt::uid).collect())
}

/// One orphan sweep for the `Provider` `key` in `home`. Never fails the
/// reconcile: a sweep that cannot list or terminate waits for the next pass,
/// and a failed machine listing skips the pass entirely, since every UID
/// missing from it would read as an orphan.
async fn run_sweep(ctx: &Context, api: &dyn Ec2Api, home: &str, key: &str) {
    let live = match live_machine_uids(ctx, home).await {
        Ok(live) => live,
        Err(e) => {
            warn!(error = %e, "orphan sweep skipped: cannot list EC2Machines");
            ctx.sweep_metrics.record_skipped(key);
            return;
        }
    };
    match sweep(
        api,
        key,
        &live,
        sweep_namespace(ctx.scope(), home),
        jiff::Timestamp::now(),
    )
    .await
    {
        Ok(report) => {
            info!(
                found = report.found,
                deleted = report.deleted.len(),
                failed = report.failed.len(),
                deferred = report.deferred,
                throttled = report.throttled,
                "orphan sweep finished"
            );
            ctx.sweep_metrics.record(key, &report);
        }
        Err(e) => {
            warn!(error = %e, "orphan sweep skipped: cannot list the Provider's instances");
            ctx.sweep_metrics.record_skipped(key);
        }
    }
}

async fn patch_status(
    ctx: &Context,
    name: &str,
    namespace: &str,
    status: ProviderStatus,
) -> Result<()> {
    let api: Api<Provider> = Api::namespaced(ctx.client.clone(), namespace);
    apply_status(&api, name, FIELD_MANAGER_PROVIDER_EC2, &status, true).await?;
    Ok(())
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;
