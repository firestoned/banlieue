// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! An in-memory Azure Resource Manager for unit tests.
//!
//! It refuses what ARM refused in the roadmap 23 phase 0 spike
//! (rules/testing.md, rule 1): a size restricted in the zone; an NVMe-only
//! size from an image that does not declare NVMe; an OS disk smaller than
//! the image; a tag name Azure rejects; a create beyond quota; a VM whose NIC
//! does not exist; deleting a NIC a VM still uses; changing a VM whose
//! provisioning failed; a burst of deletes. It allows what ARM allowed:
//! deleting something already gone, and deleting a VM takes its NIC and OS
//! disk. A refused VM create leaves its NIC behind, as in the spike.
//!
//! The VM size catalogue and quota come from real responses captured on the
//! spike subscription (`fixtures/`); quota is generous unless
//! [`FakeArm::use_captured_quota`] switches to the captured figures.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;

use crate::api::{ArmApi, PowerAction};
use crate::error::{Error, Result};
use crate::operation::{OperationHandle, OperationStatus};
use crate::tags::validate_tags;
use crate::types::{
    InstanceView, Power, Provisioning, ResourceSku, TaggedResource, Usage, parse_resource_skus,
    parse_usages,
};
use crate::vm::{NicRequest, VmRequest};

const CAPTURED_SKUS: &[u8] = include_bytes!("../fixtures/resource-skus.json");
const CAPTURED_USAGES: &[u8] = include_bytes!("../fixtures/usages.json");

/// The subscription every fake resource ID is under.
const FAKE_SUBSCRIPTION: &str = "00000000-0000-0000-0000-000000000000";
/// Quota limit per line when the fake is not using the captured figures.
const GENEROUS_QUOTA_LIMIT: u64 = 1_000;
const REGIONAL_CORES: &str = "cores";
/// Throttle hint the spike saw on a burst of deletes.
const THROTTLE_HINT: Duration = Duration::from_secs(1);
/// Azure keeps .0 to .3 of each subnet; the first VM gets .4.
const FIRST_HOST_OCTET: u16 = 4;
const FAKE_SUBNET_PREFIX: &str = "10.0.0.";
const OS_DISK_SUFFIX: &str = "_OsDisk";
const NIC_TYPE: &str = "Microsoft.Network/networkInterfaces";
const VM_TYPE: &str = "Microsoft.Compute/virtualMachines";
const DISK_TYPE: &str = "Microsoft.Compute/disks";

/// The resource ID the fake gives NIC `name` in `resource_group`.
#[must_use]
pub fn fake_nic_id(resource_group: &str, name: &str) -> String {
    format!(
        "/subscriptions/{FAKE_SUBSCRIPTION}/resourceGroups/{resource_group}/providers/{NIC_TYPE}/{name}"
    )
}

type Key = (String, String);

fn key(resource_group: &str, name: &str) -> Key {
    (
        resource_group.to_ascii_lowercase(),
        name.to_ascii_lowercase(),
    )
}

struct FakeNic {
    name: String,
    tags: BTreeMap<String, String>,
    ip: String,
    attached_vm: Option<String>,
    created: jiff::Timestamp,
}

struct FakeVm {
    name: String,
    nic: Key,
    disk: Key,
    family: String,
    vcpus: u64,
    provisioning: Provisioning,
    power: Power,
    tags: BTreeMap<String, String>,
    created: jiff::Timestamp,
}

struct FakeDisk {
    name: String,
    created: jiff::Timestamp,
}

struct FakeImage {
    floor_gb: u32,
    controllers: Vec<String>,
}

struct State {
    skus: Vec<ResourceSku>,
    captured_usages: Vec<Usage>,
    use_captured_quota: bool,
    images: BTreeMap<String, FakeImage>,
    nics: BTreeMap<Key, FakeNic>,
    vms: BTreeMap<Key, FakeVm>,
    disks: BTreeMap<Key, FakeDisk>,
    delete_burst: Option<u32>,
    deletes_used: u32,
    next_host: u16,
}

/// In-memory [`ArmApi`].
pub struct FakeArm {
    state: Mutex<State>,
}

fn not_found(code: &str, what: &str) -> Error {
    Error::NotFound {
        code: code.to_string(),
        message: format!("{what} was not found."),
    }
}

fn invalid(code: &str, message: String) -> Error {
    Error::InvalidRequest {
        code: code.to_string(),
        message,
    }
}

impl FakeArm {
    /// A fake whose size catalogue and quota lines are the ones captured on
    /// the spike subscription. `_location` is accepted for symmetry: the
    /// captured catalogue is one region's.
    ///
    /// # Errors
    /// [`Error::Decode`] if a fixture does not parse.
    pub fn from_fixtures(_location: &str) -> Result<Self> {
        Ok(Self {
            state: Mutex::new(State {
                skus: parse_resource_skus(CAPTURED_SKUS)?,
                captured_usages: parse_usages(CAPTURED_USAGES)?,
                use_captured_quota: false,
                images: BTreeMap::new(),
                nics: BTreeMap::new(),
                vms: BTreeMap::new(),
                disks: BTreeMap::new(),
                delete_burst: None,
                deletes_used: 0,
                next_host: FIRST_HOST_OCTET,
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panicking test leaves the fake poisoned; the data is still usable.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Registers a gallery image version: its minimum OS disk size and the
    /// disk controllers its definition declares.
    pub fn register_image(&self, id: &str, floor_gb: u32, controllers: &[&str]) {
        self.lock().images.insert(
            id.to_ascii_lowercase(),
            FakeImage {
                floor_gb,
                controllers: controllers.iter().map(|c| (*c).to_string()).collect(),
            },
        );
    }

    /// Switches quota to the captured figures.
    pub fn use_captured_quota(&self) {
        self.lock().use_captured_quota = true;
    }

    /// Allows `burst` deletes before throttling.
    pub fn set_delete_burst(&self, burst: u32) {
        let mut s = self.lock();
        s.delete_burst = Some(burst);
        s.deletes_used = 0;
    }

    /// Resets the delete budget, as time passing would.
    pub fn refill_delete_budget(&self) {
        self.lock().deletes_used = 0;
    }

    /// The guest posted Ready to WireServer: provisioning succeeds.
    pub fn guest_reports_ready(&self, resource_group: &str, name: &str) {
        if let Some(vm) = self.lock().vms.get_mut(&key(resource_group, name)) {
            vm.provisioning = Provisioning::Succeeded;
            vm.power = Power::Running;
        }
    }

    /// Provisioning failed with `reason` (`OSProvisioningTimedOut`).
    pub fn fail_provisioning(&self, resource_group: &str, name: &str, reason: &str) {
        if let Some(vm) = self.lock().vms.get_mut(&key(resource_group, name)) {
            vm.provisioning = Provisioning::Failed {
                reason: reason.to_string(),
            };
            vm.power = Power::Running;
        }
    }

    fn consume_delete(state: &mut State) -> Result<()> {
        if let Some(burst) = state.delete_burst
            && state.deletes_used >= burst
        {
            return Err(Error::Throttled {
                retry_after: THROTTLE_HINT,
            });
        }
        state.deletes_used += 1;
        Ok(())
    }

    fn current_vcpus(state: &State, family: Option<&str>) -> u64 {
        state
            .vms
            .values()
            .filter(|vm| family.is_none_or(|f| vm.family.eq_ignore_ascii_case(f)))
            .map(|vm| vm.vcpus)
            .sum()
    }

    fn usage_lines(state: &State) -> Vec<Usage> {
        let mut lines: Vec<Usage> = if state.use_captured_quota {
            state.captured_usages.clone()
        } else {
            state
                .captured_usages
                .iter()
                .map(|u| Usage {
                    name: u.name.clone(),
                    current: 0,
                    limit: GENEROUS_QUOTA_LIMIT,
                })
                .collect()
        };
        for line in &mut lines {
            let family = (line.name != REGIONAL_CORES).then_some(line.name.as_str());
            line.current += Self::current_vcpus(state, family);
        }
        lines
    }
}

#[async_trait]
impl ArmApi for FakeArm {
    async fn put_nic(
        &self,
        resource_group: &str,
        name: &str,
        request: &NicRequest,
    ) -> Result<Option<OperationHandle>> {
        request.to_body()?;
        validate_tags(&request.tags)?;
        let mut s = self.lock();
        let k = key(resource_group, name);
        if let Some(existing) = s.nics.get_mut(&k) {
            existing.tags.clone_from(&request.tags);
            return Ok(None);
        }
        let ip = format!("{FAKE_SUBNET_PREFIX}{}", s.next_host);
        s.next_host += 1;
        s.nics.insert(
            k,
            FakeNic {
                name: name.to_string(),
                tags: request.tags.clone(),
                ip,
                attached_vm: None,
                created: jiff::Timestamp::now(),
            },
        );
        Ok(None)
    }

    async fn nic_private_ip(&self, resource_group: &str, name: &str) -> Result<Option<String>> {
        self.lock()
            .nics
            .get(&key(resource_group, name))
            .map(|n| Some(n.ip.clone()))
            .ok_or_else(|| not_found("NotFound", "The network interface"))
    }

    async fn delete_nic(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<OperationHandle>> {
        let mut s = self.lock();
        Self::consume_delete(&mut s)?;
        let k = key(resource_group, name);
        let Some(nic) = s.nics.get(&k) else {
            return Ok(None);
        };
        if let Some(vm) = &nic.attached_vm {
            return Err(invalid(
                "NicInUse",
                format!("Network Interface {name} is used by existing resource {vm}."),
            ));
        }
        s.nics.remove(&k);
        Ok(None)
    }

    async fn put_vm(
        &self,
        resource_group: &str,
        name: &str,
        request: &VmRequest,
    ) -> Result<Option<OperationHandle>> {
        request.to_body()?;
        let mut s = self.lock();
        let vm_key = key(resource_group, name);
        if s.vms.contains_key(&vm_key) {
            return Ok(None);
        }

        let nic_key = s
            .nics
            .iter()
            .find(|((rg, n), _)| fake_nic_id(rg, n).eq_ignore_ascii_case(&request.nic_id))
            .map(|(k, _)| k.clone())
            .ok_or_else(|| {
                invalid(
                    "InvalidResourceReference",
                    format!(
                        "Resource {} referenced by resource {name} was not found.",
                        request.nic_id
                    ),
                )
            })?;

        let image = s
            .images
            .get(&request.image_version_id.to_ascii_lowercase())
            .ok_or_else(|| not_found("GalleryImageNotFound", "The gallery image version"))?;

        let sku = s
            .skus
            .iter()
            .find(|sku| sku.name.eq_ignore_ascii_case(&request.size))
            .ok_or_else(|| Error::SkuNotAvailable {
                code: "SkuNotAvailable".to_string(),
                message: format!("The requested size {} is not available.", request.size),
            })?;
        if !sku.usable_zones().contains(&request.zone) {
            return Err(Error::SkuNotAvailable {
                code: "NotAvailableForSubscription".to_string(),
                message: format!(
                    "{} is not available in zone {} for this subscription.",
                    request.size, request.zone
                ),
            });
        }
        if !image.controllers.iter().any(|c| sku.supports_controller(c)) {
            return Err(invalid(
                "InvalidParameter",
                format!(
                    "The VM size '{}' cannot boot with OS image or disk. Please check that disk controller types supported by the OS image or disk is one of the supported disk controller types for the VM size.",
                    request.size
                ),
            ));
        }
        if request.os_disk_size_gb < image.floor_gb {
            return Err(invalid(
                "ResizeDiskError",
                format!(
                    "Disk size {} GB is smaller than the image's {} GB.",
                    request.os_disk_size_gb, image.floor_gb
                ),
            ));
        }

        let vcpus = u64::from(sku.vcpus.unwrap_or_default());
        let family = sku.family.clone();
        let usages = Self::usage_lines(&s);
        let headroom = crate::types::vcpu_headroom(&usages, &family).unwrap_or_default();
        if headroom < vcpus {
            return Err(Error::QuotaExceeded {
                code: "QuotaExceeded".to_string(),
                message: format!(
                    "Operation could not be completed as it results in exceeding approved {family} Cores quota."
                ),
            });
        }
        if let Some(other) = s.nics.get(&nic_key).and_then(|n| n.attached_vm.clone()) {
            return Err(Error::Conflict {
                code: "NicInUse".to_string(),
                message: format!("The network interface is used by {other}."),
            });
        }

        let disk_key = key(resource_group, &format!("{name}{OS_DISK_SUFFIX}"));
        let now = jiff::Timestamp::now();
        s.disks.insert(
            disk_key.clone(),
            FakeDisk {
                name: format!("{name}{OS_DISK_SUFFIX}"),
                created: now,
            },
        );
        if let Some(nic) = s.nics.get_mut(&nic_key) {
            nic.attached_vm = Some(name.to_string());
        }
        s.vms.insert(
            vm_key,
            FakeVm {
                name: name.to_string(),
                nic: nic_key,
                disk: disk_key,
                family,
                vcpus,
                provisioning: Provisioning::Creating,
                power: Power::Starting,
                tags: request.tags.clone(),
                created: now,
            },
        );
        Ok(None)
    }

    async fn vm_instance_view(&self, resource_group: &str, name: &str) -> Result<InstanceView> {
        self.lock()
            .vms
            .get(&key(resource_group, name))
            .map(|vm| InstanceView {
                provisioning: vm.provisioning.clone(),
                provisioning_message: None,
                power: vm.power,
            })
            .ok_or_else(|| not_found("ResourceNotFound", "The virtual machine"))
    }

    async fn delete_vm(&self, resource_group: &str, name: &str) -> Result<Option<OperationHandle>> {
        let mut s = self.lock();
        Self::consume_delete(&mut s)?;
        let Some(vm) = s.vms.remove(&key(resource_group, name)) else {
            return Ok(None);
        };
        s.nics.remove(&vm.nic);
        s.disks.remove(&vm.disk);
        Ok(None)
    }

    async fn power(
        &self,
        resource_group: &str,
        name: &str,
        action: PowerAction,
    ) -> Result<Option<OperationHandle>> {
        let mut s = self.lock();
        let vm = s
            .vms
            .get_mut(&key(resource_group, name))
            .ok_or_else(|| not_found("ResourceNotFound", "The virtual machine"))?;
        if let Provisioning::Failed { reason } = &vm.provisioning {
            return Err(Error::Conflict {
                code: "OperationNotAllowed".to_string(),
                message: format!("Provisioning failure ({reason}) has reached a terminal state."),
            });
        }
        vm.power = match action {
            PowerAction::Start | PowerAction::Restart => Power::Running,
            PowerAction::Deallocate => Power::Deallocated,
            PowerAction::PowerOff => Power::Stopped,
        };
        Ok(None)
    }

    async fn delete_disk(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<OperationHandle>> {
        let mut s = self.lock();
        Self::consume_delete(&mut s)?;
        s.disks.remove(&key(resource_group, name));
        Ok(None)
    }

    async fn operation_status(&self, _handle: &OperationHandle) -> Result<OperationStatus> {
        // The fake completes every call synchronously.
        Ok(OperationStatus::Succeeded)
    }

    async fn resource_skus(&self, _location: &str) -> Result<Vec<ResourceSku>> {
        Ok(self.lock().skus.clone())
    }

    async fn usages(&self, _location: &str) -> Result<Vec<Usage>> {
        Ok(Self::usage_lines(&self.lock()))
    }

    async fn list_resources(&self, resource_group: &str) -> Result<Vec<TaggedResource>> {
        let s = self.lock();
        let rg = resource_group.to_ascii_lowercase();
        let id = |kind: &str, name: &str| {
            format!(
                "/subscriptions/{FAKE_SUBSCRIPTION}/resourceGroups/{resource_group}/providers/{kind}/{name}"
            )
        };
        let nics = s
            .nics
            .iter()
            .filter(|((r, _), _)| *r == rg)
            .map(|(_, n)| TaggedResource {
                id: id(NIC_TYPE, &n.name),
                name: n.name.clone(),
                resource_type: NIC_TYPE.to_string(),
                tags: n.tags.clone(),
                created_time: Some(n.created),
            });
        let vms = s
            .vms
            .iter()
            .filter(|((r, _), _)| *r == rg)
            .map(|(_, v)| TaggedResource {
                id: id(VM_TYPE, &v.name),
                name: v.name.clone(),
                resource_type: VM_TYPE.to_string(),
                tags: v.tags.clone(),
                created_time: Some(v.created),
            });
        // ARM does not copy a VM's tags onto the OS disk it creates.
        let disks = s
            .disks
            .iter()
            .filter(|((r, _), _)| *r == rg)
            .map(|(_, d)| TaggedResource {
                id: id(DISK_TYPE, &d.name),
                name: d.name.clone(),
                resource_type: DISK_TYPE.to_string(),
                tags: BTreeMap::new(),
                created_time: Some(d.created),
            });
        Ok(nics.chain(vms).chain(disks).collect())
    }
}

#[cfg(test)]
#[path = "fake_tests.rs"]
mod fake_tests;
