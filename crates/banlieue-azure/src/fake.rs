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
//!
//! For the publish Job (ADR-0162) it models the image definition, the
//! upload disk with its write SAS and page blob ([`FakePageBlob`]), the
//! snapshot and the image version, and refuses what ARM refused: an upload
//! size that is not whole 512-byte pages or whose virtual size is not a
//! whole MiB; write access to a disk that is not awaiting an upload; a
//! snapshot of a disk still being uploaded; a version name that is not
//! `int.int.int`; a version from a source lacking NVMe when its definition
//! declares it (the spike's `does not have a matching diskControllerType`);
//! changing a definition's features; deleting a definition that still has
//! versions; and a SAS on a host that is not the cloud's upload host.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;

use crate::api::{ArmApi, DiskAccess, PowerAction, PublishApi};
use crate::error::{Error, Result};
use crate::fake_blob::FakePageBlob;
use crate::gallery::{
    DISK_STATE_ACTIVE_UPLOAD, DISK_STATE_READY_TO_UPLOAD, DISK_STATE_UNATTACHED,
    FEATURE_DISK_CONTROLLER_TYPES, GalleryImageDefinition, GalleryImageVersion,
    GalleryImageVersionRequest, ImageDefinitionRequest, ManagedDisk, PROVISIONING_CREATING,
    PROVISIONING_SUCCEEDED, Snapshot, SnapshotRequest, UploadDiskRequest, check_version_name,
    declares_nvme,
};
use crate::operation::{OperationHandle, OperationStatus};
use crate::tags::validate_tags;
use crate::types::{
    InstanceView, Power, Provisioning, ResourceSku, TaggedResource, Usage, parse_resource_skus,
    parse_usages,
};
use crate::upload::{PAGE_ALIGNMENT, PageWriter, PageWriterFactory, SasUrl, validate_upload_url};
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
const SNAPSHOT_TYPE: &str = "Microsoft.Compute/snapshots";
const GALLERY_TYPE: &str = "Microsoft.Compute/galleries";
/// The managed-disk upload domain of public Azure; the fake's SAS URLs
/// live under it unless [`FakeArm::set_upload_host`] says otherwise.
pub const FAKE_UPLOAD_HOST_SUFFIX: &str = "blob.storage.azure.net";
const FAKE_UPLOAD_HOST: &str = "md-fake.z1.blob.storage.azure.net";
const FAKE_SAS_QUERY: &str = "sv=2018-03-28&sr=b&si=fake&sig=FAKE";
/// Azure requires a VHD's virtual size to be a whole MiB.
const MIB: u64 = 1024 * 1024;
const CODE_INVALID_PARAMETER: &str = "InvalidParameter";
const CODE_OPERATION_NOT_ALLOWED: &str = "OperationNotAllowed";

/// The resource ID the fake gives NIC `name` in `resource_group`.
#[must_use]
pub fn fake_nic_id(resource_group: &str, name: &str) -> String {
    format!(
        "/subscriptions/{FAKE_SUBSCRIPTION}/resourceGroups/{resource_group}/providers/{NIC_TYPE}/{name}"
    )
}

fn compute_id(resource_group: &str, kind: &str, name: &str) -> String {
    format!(
        "/subscriptions/{FAKE_SUBSCRIPTION}/resourceGroups/{resource_group}/providers/Microsoft.Compute/{kind}/{name}"
    )
}

fn definition_id(resource_group: &str, gallery: &str, image: &str) -> String {
    format!(
        "/subscriptions/{FAKE_SUBSCRIPTION}/resourceGroups/{resource_group}/providers/{GALLERY_TYPE}/{gallery}/images/{image}"
    )
}

fn version_id(resource_group: &str, gallery: &str, image: &str, version: &str) -> String {
    format!(
        "{}/versions/{version}",
        definition_id(resource_group, gallery, image)
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

type GalleryKey = (String, String, String);
type VersionKey = (String, String, String, String);

fn gallery_key(rg: &str, gallery: &str, image: &str) -> GalleryKey {
    (
        rg.to_ascii_lowercase(),
        gallery.to_ascii_lowercase(),
        image.to_ascii_lowercase(),
    )
}

fn version_key(rg: &str, gallery: &str, image: &str, version: &str) -> VersionKey {
    let (r, g, i) = gallery_key(rg, gallery, image);
    (r, g, i, version.to_string())
}

struct FakeDefinition {
    id: String,
    features: BTreeMap<String, String>,
    tags: BTreeMap<String, String>,
}

struct FakeVersion {
    id: String,
    state: String,
    tags: BTreeMap<String, String>,
    /// Reads left before a `Creating` version reports `Succeeded`.
    creating_reads: u32,
}

struct FakeUploadDisk {
    id: String,
    state: String,
    upload_size: u64,
    controllers: Option<String>,
    tags: BTreeMap<String, String>,
    blob: Arc<FakePageBlob>,
}

struct FakeSnapshot {
    id: String,
    controllers: Option<String>,
    tags: BTreeMap<String, String>,
}

/// The gallery side of the fake: definitions, versions, upload disks,
/// snapshots, and the knobs tests turn.
#[derive(Default)]
struct Gallery {
    definitions: BTreeMap<GalleryKey, FakeDefinition>,
    versions: BTreeMap<VersionKey, FakeVersion>,
    upload_disks: BTreeMap<Key, FakeUploadDisk>,
    snapshots: BTreeMap<Key, FakeSnapshot>,
    version_creating_reads: u32,
    upload_host: Option<String>,
    calls: u32,
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
    gallery: Gallery,
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
                gallery: Gallery::default(),
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

    /// Someone else retagged the VM (or created one with this name): its
    /// tags become `tags`, so ownership checks can be exercised.
    pub fn set_vm_tags(&self, resource_group: &str, name: &str, tags: BTreeMap<String, String>) {
        if let Some(vm) = self.lock().vms.get_mut(&key(resource_group, name)) {
            vm.tags = tags;
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

    // ----- Gallery publishing (ADR-0162) -----

    /// Newly created image versions report `Creating` for the next `reads`
    /// reads, then `Succeeded`, as a 7 to 22 minute create would.
    pub fn hold_versions_creating(&self, reads: u32) {
        self.lock().gallery.version_creating_reads = reads;
    }

    /// The host the fake's write SAS URLs name. Default: a host under
    /// [`FAKE_UPLOAD_HOST_SUFFIX`].
    pub fn set_upload_host(&self, host: &str) {
        self.lock().gallery.upload_host = Some(host.to_string());
    }

    /// Calls made to any gallery, upload-disk or snapshot method.
    #[must_use]
    pub fn publish_calls(&self) -> u32 {
        self.lock().gallery.calls
    }

    /// Puts an image version in place directly, as another Job or an admin
    /// would have: `state` and `tags` as given (a `Creating` one honours
    /// [`FakeArm::hold_versions_creating`]).
    pub fn insert_gallery_image_version(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
        version: &str,
        state: &str,
        tags: BTreeMap<String, String>,
    ) {
        let id = version_id(resource_group, gallery, image, version);
        let mut s = self.lock();
        let creating_reads = s.gallery.version_creating_reads;
        s.gallery.versions.insert(
            version_key(resource_group, gallery, image, version),
            FakeVersion {
                id,
                state: state.to_string(),
                tags,
                creating_reads,
            },
        );
    }

    /// Changes an existing version's provisioning state.
    pub fn set_gallery_image_version_state(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
        version: &str,
        state: &str,
    ) {
        if let Some(v) = self.lock().gallery.versions.get_mut(&version_key(
            resource_group,
            gallery,
            image,
            version,
        )) {
            v.state = state.to_string();
            v.creating_reads = 0;
        }
    }

    /// Puts an image definition in place directly, with `features` and
    /// `tags` as given.
    pub fn insert_gallery_image_definition(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
        features: BTreeMap<String, String>,
        tags: BTreeMap<String, String>,
    ) {
        self.lock().gallery.definitions.insert(
            gallery_key(resource_group, gallery, image),
            FakeDefinition {
                id: definition_id(resource_group, gallery, image),
                features,
                tags,
            },
        );
    }

    /// Puts a snapshot in place directly, declaring `controllers`.
    pub fn insert_snapshot(
        &self,
        resource_group: &str,
        name: &str,
        controllers: Option<&str>,
        tags: BTreeMap<String, String>,
    ) {
        self.lock().gallery.snapshots.insert(
            key(resource_group, name),
            FakeSnapshot {
                id: compute_id(resource_group, "snapshots", name),
                controllers: controllers.map(ToString::to_string),
                tags,
            },
        );
    }

    /// Forces an upload disk's `diskState`, as a Job killed mid-upload
    /// leaves it.
    pub fn set_disk_state(&self, resource_group: &str, name: &str, state: &str) {
        if let Some(d) = self
            .lock()
            .gallery
            .upload_disks
            .get_mut(&key(resource_group, name))
        {
            d.state = state.to_string();
        }
    }

    /// The page blob behind an upload disk.
    #[must_use]
    pub fn upload_blob(&self, resource_group: &str, name: &str) -> Option<Arc<FakePageBlob>> {
        self.lock()
            .gallery
            .upload_disks
            .get(&key(resource_group, name))
            .map(|d| d.blob.clone())
    }

    /// Whether a snapshot exists.
    #[must_use]
    pub fn has_snapshot(&self, resource_group: &str, name: &str) -> bool {
        self.lock()
            .gallery
            .snapshots
            .contains_key(&key(resource_group, name))
    }

    /// Whether an upload disk exists.
    #[must_use]
    pub fn has_upload_disk(&self, resource_group: &str, name: &str) -> bool {
        self.lock()
            .gallery
            .upload_disks
            .contains_key(&key(resource_group, name))
    }

    /// Whether an image definition exists.
    #[must_use]
    pub fn has_gallery_image_definition(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
    ) -> bool {
        self.lock()
            .gallery
            .definitions
            .contains_key(&gallery_key(resource_group, gallery, image))
    }

    fn gallery_call(&self) -> MutexGuard<'_, State> {
        let mut s = self.lock();
        s.gallery.calls += 1;
        s
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

    async fn vm_tags(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<BTreeMap<String, String>>> {
        Ok(self
            .lock()
            .vms
            .get(&key(resource_group, name))
            .map(|vm| vm.tags.clone()))
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
        s.gallery.upload_disks.remove(&key(resource_group, name));
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
        let upload_disks = s
            .gallery
            .upload_disks
            .iter()
            .filter(|((r, _), _)| *r == rg)
            .map(|((_, n), d)| TaggedResource {
                id: d.id.clone(),
                name: n.clone(),
                resource_type: DISK_TYPE.to_string(),
                tags: d.tags.clone(),
                created_time: None,
            });
        let snapshots = s
            .gallery
            .snapshots
            .iter()
            .filter(|((r, _), _)| *r == rg)
            .map(|((_, n), d)| TaggedResource {
                id: d.id.clone(),
                name: n.clone(),
                resource_type: SNAPSHOT_TYPE.to_string(),
                tags: d.tags.clone(),
                created_time: None,
            });
        Ok(nics
            .chain(vms)
            .chain(disks)
            .chain(upload_disks)
            .chain(snapshots)
            .collect())
    }

    async fn get_gallery_image_version(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
        version: &str,
    ) -> Result<Option<GalleryImageVersion>> {
        check_version_name(version)?;
        let mut s = self.gallery_call();
        let Some(v) =
            s.gallery
                .versions
                .get_mut(&version_key(resource_group, gallery, image, version))
        else {
            return Ok(None);
        };
        if v.state == PROVISIONING_CREATING {
            if v.creating_reads == 0 {
                v.state = PROVISIONING_SUCCEEDED.to_string();
            } else {
                v.creating_reads -= 1;
            }
        }
        Ok(Some(GalleryImageVersion {
            id: v.id.clone(),
            provisioning_state: v.state.clone(),
            tags: v.tags.clone(),
        }))
    }

    async fn delete_gallery_image_version(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
        version: &str,
    ) -> Result<Option<OperationHandle>> {
        check_version_name(version)?;
        let mut s = self.gallery_call();
        Self::consume_delete(&mut s)?;
        s.gallery
            .versions
            .remove(&version_key(resource_group, gallery, image, version));
        Ok(None)
    }
}

fn refuse(code: &str, message: String) -> Error {
    Error::Conflict {
        code: code.to_string(),
        message,
    }
}

#[async_trait]
impl PublishApi for FakeArm {
    async fn get_gallery_image_definition(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
    ) -> Result<Option<GalleryImageDefinition>> {
        let s = self.gallery_call();
        Ok(s.gallery
            .definitions
            .get(&gallery_key(resource_group, gallery, image))
            .map(|d| GalleryImageDefinition {
                id: d.id.clone(),
                tags: d.tags.clone(),
                features: d.features.clone(),
            }))
    }

    async fn put_gallery_image_definition(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
        request: &ImageDefinitionRequest,
    ) -> Result<Option<OperationHandle>> {
        request.to_body()?;
        let mut s = self.gallery_call();
        let k = gallery_key(resource_group, gallery, image);
        let features = request.features();
        if let Some(existing) = s.gallery.definitions.get_mut(&k) {
            if existing.features != features {
                return Err(refuse(
                    "PropertyChangeNotAllowed",
                    "Changing property 'features' is not allowed.".to_string(),
                ));
            }
            existing.tags.clone_from(&request.tags);
            return Ok(None);
        }
        s.gallery.definitions.insert(
            k,
            FakeDefinition {
                id: definition_id(resource_group, gallery, image),
                features,
                tags: request.tags.clone(),
            },
        );
        Ok(None)
    }

    async fn delete_gallery_image_definition(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
    ) -> Result<Option<OperationHandle>> {
        let mut s = self.gallery_call();
        Self::consume_delete(&mut s)?;
        let k = gallery_key(resource_group, gallery, image);
        let nested = s
            .gallery
            .versions
            .keys()
            .any(|(r, g, i, _)| (r, g, i) == (&k.0, &k.1, &k.2));
        if nested {
            return Err(refuse(
                "CannotDeleteResource",
                format!("Gallery image {image} has nested resources and cannot be deleted."),
            ));
        }
        s.gallery.definitions.remove(&k);
        Ok(None)
    }

    async fn list_gallery_image_versions(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
    ) -> Result<Vec<GalleryImageVersion>> {
        let s = self.gallery_call();
        let k = gallery_key(resource_group, gallery, image);
        Ok(s.gallery
            .versions
            .iter()
            .filter(|((r, g, i, _), _)| (r, g, i) == (&k.0, &k.1, &k.2))
            .map(|(_, v)| GalleryImageVersion {
                id: v.id.clone(),
                provisioning_state: v.state.clone(),
                tags: v.tags.clone(),
            })
            .collect())
    }

    async fn put_gallery_image_version(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
        version: &str,
        request: &GalleryImageVersionRequest,
    ) -> Result<Option<OperationHandle>> {
        check_version_name(version)?;
        request.to_body()?;
        let mut s = self.gallery_call();
        let def = s
            .gallery
            .definitions
            .get(&gallery_key(resource_group, gallery, image))
            .ok_or_else(|| not_found("ParentResourceNotFound", "The gallery image"))?;
        let wants_nvme = def
            .features
            .get(FEATURE_DISK_CONTROLLER_TYPES)
            .is_some_and(|c| declares_nvme(c));
        let source = s
            .gallery
            .snapshots
            .values()
            .find(|snap| snap.id.eq_ignore_ascii_case(&request.snapshot_id))
            .ok_or_else(|| {
                invalid(
                    "InvalidResourceReference",
                    format!("Resource {} was not found.", request.snapshot_id),
                )
            })?;
        let source_nvme = source.controllers.as_deref().is_some_and(declares_nvme);
        if wants_nvme && !source_nvme {
            return Err(refuse(
                "Conflict",
                format!(
                    "The source {} does not have a matching diskControllerType feature support; target image 'SCSI, NVMe', source '{}'.",
                    request.snapshot_id,
                    source.controllers.clone().unwrap_or_default()
                ),
            ));
        }
        let reads = s.gallery.version_creating_reads;
        let k = version_key(resource_group, gallery, image, version);
        if s.gallery.versions.contains_key(&k) {
            return Ok(None);
        }
        s.gallery.versions.insert(
            k,
            FakeVersion {
                id: version_id(resource_group, gallery, image, version),
                state: PROVISIONING_CREATING.to_string(),
                tags: request.tags.clone(),
                creating_reads: reads,
            },
        );
        Ok(None)
    }

    async fn get_disk(&self, resource_group: &str, name: &str) -> Result<Option<ManagedDisk>> {
        let s = self.gallery_call();
        Ok(s.gallery
            .upload_disks
            .get(&key(resource_group, name))
            .map(|d| ManagedDisk {
                id: d.id.clone(),
                disk_state: d.state.clone(),
                upload_size_bytes: Some(d.upload_size),
                disk_controller_types: d.controllers.clone(),
                tags: d.tags.clone(),
            }))
    }

    async fn put_upload_disk(
        &self,
        resource_group: &str,
        name: &str,
        request: &UploadDiskRequest,
    ) -> Result<Option<OperationHandle>> {
        request.to_body()?;
        let size = request.upload_size_bytes;
        if !size.is_multiple_of(PAGE_ALIGNMENT) || size <= PAGE_ALIGNMENT {
            return Err(invalid(
                CODE_INVALID_PARAMETER,
                format!("uploadSizeBytes {size} must be a multiple of {PAGE_ALIGNMENT}."),
            ));
        }
        if !(size - PAGE_ALIGNMENT).is_multiple_of(MIB) {
            return Err(invalid(
                CODE_INVALID_PARAMETER,
                format!(
                    "The VHD's virtual size {} is not a multiple of 1 MiB.",
                    size - PAGE_ALIGNMENT
                ),
            ));
        }
        let mut s = self.gallery_call();
        let k = key(resource_group, name);
        if let Some(existing) = s.gallery.upload_disks.get(&k) {
            if existing.upload_size != size {
                return Err(refuse(
                    "PropertyChangeNotAllowed",
                    "Changing property 'uploadSizeBytes' is not allowed.".to_string(),
                ));
            }
            return Ok(None);
        }
        s.gallery.upload_disks.insert(
            k,
            FakeUploadDisk {
                id: compute_id(resource_group, "disks", name),
                state: DISK_STATE_READY_TO_UPLOAD.to_string(),
                upload_size: size,
                controllers: Some(crate::gallery::DISK_CONTROLLER_TYPES.to_string()),
                tags: request.tags.clone(),
                blob: Arc::new(FakePageBlob::new(size)),
            },
        );
        Ok(None)
    }

    async fn grant_disk_write_access(
        &self,
        resource_group: &str,
        name: &str,
        _duration_secs: u32,
    ) -> Result<DiskAccess> {
        let mut s = self.gallery_call();
        let host = s
            .gallery
            .upload_host
            .clone()
            .unwrap_or_else(|| FAKE_UPLOAD_HOST.to_string());
        let disk = s
            .gallery
            .upload_disks
            .get_mut(&key(resource_group, name))
            .ok_or_else(|| not_found("ResourceNotFound", "The disk"))?;
        if disk.state != DISK_STATE_READY_TO_UPLOAD && disk.state != DISK_STATE_ACTIVE_UPLOAD {
            return Err(refuse(
                CODE_OPERATION_NOT_ALLOWED,
                format!(
                    "Disk {name} is in state {} and cannot be opened for upload.",
                    disk.state
                ),
            ));
        }
        disk.state = DISK_STATE_ACTIVE_UPLOAD.to_string();
        Ok(DiskAccess::Granted(SasUrl::new(format!(
            "https://{host}/{}/{}/abcd?{FAKE_SAS_QUERY}",
            resource_group.to_ascii_lowercase(),
            name.to_ascii_lowercase()
        ))))
    }

    async fn disk_access_result(&self, _handle: &OperationHandle) -> Result<Option<SasUrl>> {
        Err(invalid(
            CODE_OPERATION_NOT_ALLOWED,
            "the fake grants access synchronously; there is no operation".to_string(),
        ))
    }

    async fn revoke_disk_access(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<OperationHandle>> {
        let mut s = self.gallery_call();
        if let Some(disk) = s.gallery.upload_disks.get_mut(&key(resource_group, name))
            && disk.state == DISK_STATE_ACTIVE_UPLOAD
        {
            disk.state = DISK_STATE_UNATTACHED.to_string();
        }
        Ok(None)
    }

    async fn get_snapshot(&self, resource_group: &str, name: &str) -> Result<Option<Snapshot>> {
        let s = self.gallery_call();
        Ok(s.gallery
            .snapshots
            .get(&key(resource_group, name))
            .map(|snap| Snapshot {
                id: snap.id.clone(),
                provisioning_state: PROVISIONING_SUCCEEDED.to_string(),
                tags: snap.tags.clone(),
            }))
    }

    async fn put_snapshot(
        &self,
        resource_group: &str,
        name: &str,
        request: &SnapshotRequest,
    ) -> Result<Option<OperationHandle>> {
        request.to_body()?;
        let mut s = self.gallery_call();
        let source = s
            .gallery
            .upload_disks
            .values()
            .find(|d| d.id.eq_ignore_ascii_case(&request.source_disk_id))
            .ok_or_else(|| {
                invalid(
                    "InvalidResourceReference",
                    format!("Resource {} was not found.", request.source_disk_id),
                )
            })?;
        if source.state == DISK_STATE_ACTIVE_UPLOAD || source.state == DISK_STATE_READY_TO_UPLOAD {
            return Err(refuse(
                CODE_OPERATION_NOT_ALLOWED,
                format!(
                    "The disk is in state {} and cannot be copied until its upload is finished.",
                    source.state
                ),
            ));
        }
        let controllers = source.controllers.clone();
        let k = key(resource_group, name);
        if s.gallery.snapshots.contains_key(&k) {
            return Ok(None);
        }
        s.gallery.snapshots.insert(
            k,
            FakeSnapshot {
                id: compute_id(resource_group, "snapshots", name),
                controllers,
                tags: request.tags.clone(),
            },
        );
        Ok(None)
    }

    async fn delete_snapshot(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<OperationHandle>> {
        let mut s = self.gallery_call();
        Self::consume_delete(&mut s)?;
        s.gallery.snapshots.remove(&key(resource_group, name));
        Ok(None)
    }
}

impl PageWriterFactory for FakeArm {
    /// Checks the SAS against public Azure's upload domain, then hands out
    /// the page blob of the disk it names, while that disk is open for
    /// upload.
    fn writer(&self, sas: &SasUrl) -> Result<Arc<dyn PageWriter>> {
        validate_upload_url(sas, FAKE_UPLOAD_HOST_SUFFIX)?;
        let url = reqwest::Url::parse(sas.expose())
            .map_err(|_| Error::UntrustedUploadHost("an unparseable URL".to_string()))?;
        let mut path = url.path_segments().into_iter().flatten();
        let (Some(rg), Some(disk)) = (path.next(), path.next()) else {
            return Err(not_found("BlobNotFound", "The blob"));
        };
        let s = self.lock();
        let d = s
            .gallery
            .upload_disks
            .get(&key(rg, disk))
            .ok_or_else(|| not_found("BlobNotFound", "The blob"))?;
        if d.state != DISK_STATE_ACTIVE_UPLOAD {
            return Err(Error::Forbidden {
                code: "AuthenticationFailed".to_string(),
                message: "The SAS is not valid for this disk now.".to_string(),
            });
        }
        let blob: Arc<dyn PageWriter> = d.blob.clone();
        Ok(blob)
    }
}

#[cfg(test)]
#[path = "fake_tests.rs"]
mod fake_tests;
