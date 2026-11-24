// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! What ARM returns, read into the facts the provider decides on.
//!
//! - [`ResourceSku::usable_zones`]: the zones a size can actually be created
//!   in by *this* subscription: offered minus restricted (ADR-0160). On the
//!   spike subscription one size was offered in three zones and restricted
//!   in all three.
//! - [`vcpu_headroom`]: quota as capacity, the smaller of regional and family
//!   headroom (ADR-0161).
//! - [`InstanceView`]: provisioning and power state; `Succeeded` is
//!   `GuestReady` on Azure (ADR-0163).
//! - [`primary_private_ip`]: the address a `VirtualMachine` reports.
//! - [`orphans`]: tagged resources whose owner is gone (ADR-0158 Decision 5).
//!
//! ARM's casing is irregular: a resource group name came back upper-cased
//! inside disk IDs, and family names mix `Standard` and `standard`. IDs and
//! families are compared case-insensitively.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Error, Result, truncate};
use crate::tags::{TAG_OWNER_UID, TAG_PROVIDER};

const VM_RESOURCE_TYPE: &str = "virtualMachines";
const RESTRICTION_ZONE: &str = "Zone";
const RESTRICTION_LOCATION: &str = "Location";
const CAP_VCPUS: &str = "vCPUs";
const CAP_DISK_CONTROLLERS: &str = "DiskControllerTypes";
/// A size that declares no controller types boots from SCSI only.
const DEFAULT_DISK_CONTROLLER: &str = "SCSI";
const REGIONAL_CORES: &str = "cores";
const PROVISIONING_PREFIX: &str = "ProvisioningState/";
const POWER_PREFIX: &str = "PowerState/";
const FAILED_PREFIX: &str = "failed/";

fn decode<'a, T: Deserialize<'a>>(what: &str, body: &'a [u8]) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| Error::Decode(format!("{what}: {e}")))
}

/// Whether two ARM resource IDs name the same resource.
#[must_use]
pub fn same_resource_id(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

// ----- resource SKUs -----------------------------------------------------

#[derive(Deserialize)]
struct SkuList {
    #[serde(default)]
    value: Vec<RawSku>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSku {
    #[serde(default)]
    resource_type: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    family: String,
    #[serde(default)]
    location_info: Vec<RawLocationInfo>,
    #[serde(default)]
    restrictions: Vec<RawRestriction>,
    #[serde(default)]
    capabilities: Vec<RawCapability>,
}

#[derive(Deserialize)]
struct RawLocationInfo {
    #[serde(default)]
    zones: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRestriction {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    restriction_info: RawRestrictionInfo,
}

#[derive(Default, Deserialize)]
struct RawRestrictionInfo {
    #[serde(default)]
    zones: Vec<String>,
}

#[derive(Deserialize)]
struct RawCapability {
    name: String,
    value: String,
}

/// A VM size as the subscription sees it in one region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceSku {
    /// Size name, such as `Standard_D2s_v7`.
    pub name: String,
    /// Quota family, such as `StandardDsv7Family`.
    pub family: String,
    /// vCPUs, when reported.
    pub vcpus: Option<u32>,
    /// Disk controllers the size boots from (`SCSI`, `NVMe`).
    pub disk_controller_types: Vec<String>,
    offered_zones: BTreeSet<String>,
    restricted_zones: BTreeSet<String>,
    restricted_location: bool,
}

impl ResourceSku {
    /// Zones this subscription can create the size in, sorted.
    #[must_use]
    pub fn usable_zones(&self) -> Vec<String> {
        if self.restricted_location {
            return Vec::new();
        }
        self.offered_zones
            .difference(&self.restricted_zones)
            .cloned()
            .collect()
    }

    /// Whether the size boots from `controller` (`SCSI` or `NVMe`, any case).
    #[must_use]
    pub fn supports_controller(&self, controller: &str) -> bool {
        self.disk_controller_types
            .iter()
            .any(|c| c.eq_ignore_ascii_case(controller))
    }
}

/// Parses `GET /subscriptions/{s}/providers/Microsoft.Compute/skus`, keeping
/// virtual machine sizes only.
///
/// # Errors
/// [`Error::Decode`] if the body is not the documented shape.
pub fn parse_resource_skus(body: &[u8]) -> Result<Vec<ResourceSku>> {
    let list: SkuList = decode("resource SKUs", body)?;
    Ok(list
        .value
        .into_iter()
        .filter(|s| s.resource_type == VM_RESOURCE_TYPE)
        .map(|s| {
            let capability = |name: &str| {
                s.capabilities
                    .iter()
                    .find(|c| c.name == name)
                    .map(|c| c.value.clone())
            };
            let disk_controller_types = capability(CAP_DISK_CONTROLLERS).map_or_else(
                || vec![DEFAULT_DISK_CONTROLLER.to_string()],
                |v| v.split(',').map(|c| c.trim().to_string()).collect(),
            );
            ResourceSku {
                vcpus: capability(CAP_VCPUS).and_then(|v| v.parse().ok()),
                disk_controller_types,
                offered_zones: s
                    .location_info
                    .iter()
                    .flat_map(|l| l.zones.iter().cloned())
                    .collect(),
                restricted_zones: s
                    .restrictions
                    .iter()
                    .filter(|r| r.kind == RESTRICTION_ZONE)
                    .flat_map(|r| r.restriction_info.zones.iter().cloned())
                    .collect(),
                restricted_location: s
                    .restrictions
                    .iter()
                    .any(|r| r.kind == RESTRICTION_LOCATION),
                name: s.name,
                family: s.family,
            }
        })
        .collect())
}

// ----- usages ------------------------------------------------------------

#[derive(Deserialize)]
struct UsageList {
    #[serde(default)]
    value: Vec<RawUsage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUsage {
    name: RawUsageName,
    #[serde(default)]
    current_value: u64,
    #[serde(default)]
    limit: u64,
}

#[derive(Deserialize)]
struct RawUsageName {
    value: String,
}

/// One quota line: a family's or the region's vCPUs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage {
    /// `cores` for the region, or a family name.
    pub name: String,
    /// In use.
    pub current: u64,
    /// Allowed.
    pub limit: u64,
}

impl Usage {
    fn headroom(&self) -> u64 {
        self.limit.saturating_sub(self.current)
    }
}

/// Parses `GET .../locations/{region}/usages`.
///
/// # Errors
/// [`Error::Decode`] if the body is not the documented shape.
pub fn parse_usages(body: &[u8]) -> Result<Vec<Usage>> {
    let list: UsageList = decode("usages", body)?;
    Ok(list
        .value
        .into_iter()
        .map(|u| Usage {
            name: u.name.value,
            current: u.current_value,
            limit: u.limit,
        })
        .collect())
}

/// vCPUs that can still be created for `family`: the smaller of the
/// region's and the family's headroom, or the region's when the family has
/// no line. `None` when the region's own line is missing.
#[must_use]
pub fn vcpu_headroom(usages: &[Usage], family: &str) -> Option<u64> {
    let regional = usages.iter().find(|u| u.name == REGIONAL_CORES)?.headroom();
    let family_headroom = usages
        .iter()
        .find(|u| u.name.eq_ignore_ascii_case(family))
        .map(Usage::headroom);
    Some(family_headroom.map_or(regional, |f| f.min(regional)))
}

// ----- instance view -----------------------------------------------------

#[derive(Deserialize)]
struct RawInstanceView {
    #[serde(default)]
    statuses: Vec<RawStatus>,
}

#[derive(Deserialize)]
struct RawStatus {
    #[serde(default)]
    code: String,
    message: Option<String>,
}

/// `ProvisioningState/...` of a VM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provisioning {
    /// Creating.
    Creating,
    /// Updating.
    Updating,
    /// Deleting.
    Deleting,
    /// Succeeded: on a banlieue image, the guest reported ready (ADR-0163).
    Succeeded,
    /// Failed, with the reason (`OSProvisioningTimedOut`, ...).
    Failed {
        /// The reason after `failed/`.
        reason: String,
    },
    /// Any other state, as ARM spelled it.
    Other(String),
    /// No provisioning status in the view.
    Unknown,
}

/// `PowerState/...` of a VM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    /// Starting.
    Starting,
    /// Running.
    Running,
    /// Stopping.
    Stopping,
    /// Stopped, still allocated (still billed for compute).
    Stopped,
    /// Deallocating.
    Deallocating,
    /// Deallocated (not billed for compute).
    Deallocated,
    /// Absent or unrecognised.
    Unknown,
}

/// The parts of a VM's instance view banlieue uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceView {
    /// Provisioning state.
    pub provisioning: Provisioning,
    /// ARM's message on the provisioning status, cut to a sane length.
    pub provisioning_message: Option<String>,
    /// Power state.
    pub power: Power,
}

impl InstanceView {
    /// Whether the guest has reported ready (ADR-0163 Decision 1). Sticky:
    /// a deallocated VM that once succeeded stays succeeded.
    #[must_use]
    pub fn guest_ready(&self) -> bool {
        self.provisioning == Provisioning::Succeeded
    }
}

fn provisioning_from(state: &str) -> Provisioning {
    if let Some(reason) = state.strip_prefix(FAILED_PREFIX) {
        return Provisioning::Failed {
            reason: reason.to_string(),
        };
    }
    match state {
        "creating" => Provisioning::Creating,
        "updating" => Provisioning::Updating,
        "deleting" => Provisioning::Deleting,
        "succeeded" => Provisioning::Succeeded,
        other => Provisioning::Other(other.to_string()),
    }
}

fn power_from(state: &str) -> Power {
    match state {
        "starting" => Power::Starting,
        "running" => Power::Running,
        "stopping" => Power::Stopping,
        "stopped" => Power::Stopped,
        "deallocating" => Power::Deallocating,
        "deallocated" => Power::Deallocated,
        _ => Power::Unknown,
    }
}

/// Parses `GET .../virtualMachines/{vm}/instanceView`.
///
/// # Errors
/// [`Error::Decode`] if the body is not the documented shape.
pub fn parse_instance_view(body: &[u8]) -> Result<InstanceView> {
    let view: RawInstanceView = decode("instance view", body)?;
    let provisioning_status = view
        .statuses
        .iter()
        .find(|s| s.code.starts_with(PROVISIONING_PREFIX));
    let power = view
        .statuses
        .iter()
        .find_map(|s| s.code.strip_prefix(POWER_PREFIX))
        .map_or(Power::Unknown, power_from);
    Ok(InstanceView {
        provisioning: provisioning_status
            .and_then(|s| s.code.strip_prefix(PROVISIONING_PREFIX))
            .map_or(Provisioning::Unknown, provisioning_from),
        provisioning_message: provisioning_status
            .and_then(|s| s.message.as_deref())
            .map(truncate),
        power,
    })
}

// ----- NIC ---------------------------------------------------------------

#[derive(Deserialize)]
struct RawNic {
    #[serde(default)]
    properties: RawNicProperties,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNicProperties {
    #[serde(default)]
    ip_configurations: Vec<RawIpConfiguration>,
}

#[derive(Deserialize)]
struct RawIpConfiguration {
    #[serde(default)]
    properties: RawIpConfigurationProperties,
}

#[derive(Default, Deserialize)]
struct RawIpConfigurationProperties {
    #[serde(default)]
    primary: bool,
    #[serde(rename = "privateIPAddress")]
    private_ip_address: Option<String>,
}

/// The NIC's primary private IPv4 address, `None` until Azure has assigned
/// one.
///
/// # Errors
/// [`Error::Decode`] if the body is not the documented shape.
pub fn primary_private_ip(body: &[u8]) -> Result<Option<String>> {
    let nic: RawNic = decode("network interface", body)?;
    let configs = nic.properties.ip_configurations;
    let chosen = configs
        .iter()
        .find(|c| c.properties.primary)
        .or_else(|| configs.first());
    Ok(chosen.and_then(|c| c.properties.private_ip_address.clone()))
}

// ----- listed resources and orphans ---------------------------------------

#[derive(Deserialize)]
struct ResourceList {
    #[serde(default)]
    value: Vec<RawResource>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawResource {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "type", default)]
    resource_type: String,
    #[serde(default)]
    tags: BTreeMap<String, String>,
    created_time: Option<String>,
}

/// A resource from a resource-group listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaggedResource {
    /// Resource ID.
    pub id: String,
    /// Name.
    pub name: String,
    /// Type, such as `Microsoft.Network/networkInterfaces`.
    pub resource_type: String,
    /// Tags.
    pub tags: BTreeMap<String, String>,
    /// When it was created, when ARM said and the value parsed.
    pub created_time: Option<jiff::Timestamp>,
}

/// Parses `GET .../resourceGroups/{rg}/resources?$expand=createdTime`.
///
/// # Errors
/// [`Error::Decode`] if the body is not the documented shape.
pub fn parse_resources(body: &[u8]) -> Result<Vec<TaggedResource>> {
    let list: ResourceList = decode("resources", body)?;
    Ok(list
        .value
        .into_iter()
        .map(|r| TaggedResource {
            created_time: r.created_time.and_then(|t| t.parse().ok()),
            id: r.id,
            name: r.name,
            resource_type: r.resource_type,
            tags: r.tags,
        })
        .collect())
}

/// The resources the orphan sweep may delete: tagged for `provider`, owned
/// by a UID not in `live_uids`, and created at least `grace` before `now`
/// (ADR-0158 Decision 5). A resource with no creation time is never
/// returned: the sweep cannot prove it is not a create in flight.
#[must_use]
pub fn orphans<'a>(
    resources: &'a [TaggedResource],
    provider: &str,
    live_uids: &BTreeSet<String>,
    now: jiff::Timestamp,
    grace: Duration,
) -> Vec<&'a TaggedResource> {
    let Ok(grace) = jiff::SignedDuration::try_from(grace) else {
        return Vec::new();
    };
    resources
        .iter()
        .filter(|r| r.tags.get(TAG_PROVIDER).map(String::as_str) == Some(provider))
        .filter(|r| {
            r.tags
                .get(TAG_OWNER_UID)
                .is_some_and(|uid| !live_uids.contains(uid))
        })
        .filter(|r| {
            r.created_time
                .is_some_and(|created| now.duration_since(created) >= grace)
        })
        .collect()
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod types_tests;
