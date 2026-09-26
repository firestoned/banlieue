// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Build provider-specific infrastructure CRs from a scheduler [`Decision`].
//!
//! Two backends today: `vsphere` ([`build_vsphere_machine`]) and `libvirt`
//! ([`build_libvirt_machine`], ADR-0050). Proxmox is the third and will slot
//! in the same way. Dispatch by `Provider.spec.providerClassRef.name` lives
//! in [`super::virtualmachine`], not here — these builders are pure and know
//! only their own backend.

use std::collections::BTreeMap;

use banlieue_api::banlieue::InstallMode;
use banlieue_api::banlieue::{
    DiskSpec, HardwareOverride, ImagePerProviderStatus, Provider, SubnetShape, VMClass, VMImage,
    VirtualMachine,
};
use banlieue_api::common::{IpamShape, IpamSpec, MachineAddress, StaticIpamConfig};
use banlieue_api::infrastructure::{
    ChBootSource, ChBootSourceKind, ChCpuSpec, ChMemorySpec, ChNicSpec, CloudHypervisorMachine,
    CloudHypervisorMachineSpec, LibvirtBootSource, LibvirtBootSourceKind, LibvirtDiskBus,
    LibvirtDiskSpec, LibvirtMachine, LibvirtMachineSpec, LibvirtNicSource, LibvirtNicSourceKind,
    LibvirtNicSpec, VSphereDiskSpec, VSphereMachine, VSphereMachineSpec, VSphereNicSpec,
};
use kube::ResourceExt;
use kube::api::ObjectMeta;
use kube::core::Resource;

use super::scheduler::Decision;

/// `Provider.spec.providerClassRef.name` for the vSphere backend.
pub const PROVIDER_CLASS_VSPHERE: &str = "vsphere";
/// `Provider.spec.providerClassRef.name` for the libvirt backend.
pub const PROVIDER_CLASS_LIBVIRT: &str = "libvirt";
/// `Provider.spec.providerClassRef.name` for the Cloud Hypervisor backend
/// (ADR-0060).
pub const PROVIDER_CLASS_CLOUD_HYPERVISOR: &str = "cloud-hypervisor";

/// Which infrastructure CR kind a scheduled `VirtualMachine` becomes.
///
/// The one place the controller is allowed to know that backends differ.
/// Everything downstream — apply, delete, status mirror — branches on this
/// rather than on a provider-class string, so adding Proxmox means a variant
/// and a builder, not a new `if` in five places.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfraKind {
    /// `infrastructure.banlieue.io/VSphereMachine`.
    VSphere,
    /// `infrastructure.banlieue.io/LibvirtMachine` (ADR-0050).
    Libvirt,
    /// `infrastructure.banlieue.io/CloudHypervisorMachine` (ADR-0062).
    CloudHypervisor,
}

impl InfraKind {
    /// Map a `Provider.spec.providerClassRef.name` onto its infra kind.
    ///
    /// `None` for a provider class this controller has no builder for. That
    /// is a real state, not an impossible one: a `ProviderClass` can be
    /// installed for a provider binary that exists while the controller-side
    /// builder does not — Proxmox will be exactly that for a while — and it
    /// must surface as a condition rather than a panic or a silent no-op.
    #[must_use]
    pub fn from_provider_class(class: &str) -> Option<Self> {
        match class {
            PROVIDER_CLASS_VSPHERE => Some(Self::VSphere),
            PROVIDER_CLASS_LIBVIRT => Some(Self::Libvirt),
            PROVIDER_CLASS_CLOUD_HYPERVISOR => Some(Self::CloudHypervisor),
            _ => None,
        }
    }

    /// The Kubernetes `kind` string, for `status.infrastructureRef`.
    #[must_use]
    pub fn kind_name(self) -> &'static str {
        match self {
            Self::VSphere => "VSphereMachine",
            Self::Libvirt => "LibvirtMachine",
            Self::CloudHypervisor => "CloudHypervisorMachine",
        }
    }
}

/// Failure-domain raw-attribute key for vSphere datacenter name.
pub const FD_RAW_VSPHERE_DATACENTER: &str = "datacenter";
/// Failure-domain raw-attribute key for vSphere cluster name.
pub const FD_RAW_VSPHERE_CLUSTER: &str = "cluster";

/// Why the infra builder couldn't produce a VSphereMachine.
#[derive(Debug, thiserror::Error)]
pub enum InfraBuildError {
    /// The chosen failure domain didn't carry `datacenter` / `cluster` in
    /// `attributes.raw`. The provider's controller must populate these.
    #[error("failure domain {0} missing raw attribute '{1}'")]
    MissingFdRaw(String, &'static str),

    /// Neither `VMImage.status.perProvider[i].resolved_ref` nor a matching
    /// zone `resolved_ref` was found for the chosen provider + failure domain.
    #[error("VMImage {image} has no resolved_ref for provider {provider} (zone {zone})")]
    MissingResolvedImageRef {
        image: String,
        provider: String,
        zone: String,
    },

    /// Decision lacks a backend_id for a class — should never happen because
    /// the scheduler resolves all classes before returning.
    #[error("decision did not resolve class '{0}' to a backend identifier")]
    UnresolvedClass(String),

    /// The VMClass asks for something this backend does not support yet.
    /// Reported rather than silently dropped, so a VM never comes up with
    /// less than its class promised.
    #[error("{backend} does not support {what} yet")]
    Unsupported { backend: &'static str, what: String },
}

/// Build a [`VSphereMachine`] from the scheduler [`Decision`], the original
/// VM, its class, image, and the chosen [`Provider`]. Owner-reference is set
/// to `vm` so the VSphereMachine is garbage-collected when the parent VM is
/// deleted.
///
/// The `provider` parameter is currently unused by the vSphere builder — the
/// `Decision` already carries the resolved storage / network backend IDs the
/// scheduler computed from `Provider.spec.capabilities`. We accept it on the
/// signature so the contract is right for Phase 1C (Proxmox needs
/// `Provider.spec.connection.endpoint` to target a specific cluster) and
/// Phase 1D (libvirt needs SSH transport settings).
///
/// `provider` is also where each NIC's `networkClass` resolves its
/// per-zone subnet shape (gateway/nameservers/domain) from, once
/// `datacenter`/`cluster` are known below — filling in whatever a per-VM
/// `networkOverrides[].static` entry left unset (ADR-0032).
pub fn build_vsphere_machine(
    vm: &VirtualMachine,
    class: &VMClass,
    image: &VMImage,
    decision: &Decision,
    provider: &Provider,
    rendered_user_data: Option<&str>,
) -> Result<VSphereMachine, InfraBuildError> {
    let datacenter = decision
        .failure_domain_raw
        .get(FD_RAW_VSPHERE_DATACENTER)
        .cloned()
        .ok_or_else(|| {
            InfraBuildError::MissingFdRaw(
                decision.failure_domain_name.clone(),
                FD_RAW_VSPHERE_DATACENTER,
            )
        })?;
    let cluster = decision
        .failure_domain_raw
        .get(FD_RAW_VSPHERE_CLUSTER)
        .cloned()
        .ok_or_else(|| {
            InfraBuildError::MissingFdRaw(
                decision.failure_domain_name.clone(),
                FD_RAW_VSPHERE_CLUSTER,
            )
        })?;

    // OS disk is the first disk; its backend_id is the resolved datastore.
    let os_storage = decision
        .resolved_storage
        .first()
        .ok_or_else(|| InfraBuildError::UnresolvedClass("(no disks)".into()))?;
    let datastore = os_storage.backend_id.clone();

    // Template resolved from the VMImage's per-provider status: its bare
    // display name plus (for a Url-kind, per-zone import) the folder it
    // lives in — kept as two fields, not one encoded string, since every
    // zone's template shares the same name (ADR-0020 Decision #5).
    let (template, template_folder) = resolve_template_ref(
        image,
        &decision.provider_name,
        &decision.failure_domain_name,
    )
    .ok_or_else(|| InfraBuildError::MissingResolvedImageRef {
        image: image.name_any(),
        provider: decision.provider_name.clone(),
        zone: decision.failure_domain_name.clone(),
    })?;

    // Disks: 1:1 with VMClass disks. Storage class resolution is via
    // resolved_storage (same order). sizeGiB is the class's own value
    // unless vm.spec.hardwareOverride.diskOverrides has a matching entry —
    // a VMClass is shared, so a per-VM size bump can only be expressed
    // per-VM.
    let hardware_override = vm.spec.hardware_override.as_ref();
    let disks: Vec<VSphereDiskSpec> = class
        .spec
        .hardware
        .disks
        .iter()
        .map(|d| VSphereDiskSpec {
            name: d.name.clone(),
            size_gi_b: merge_disk_size_override(d, hardware_override),
            provisioning: d.provisioning.clone(),
        })
        .collect();

    // NICs: 1:1 with VMClass NICs. port_group resolved from resolved_networks.
    // ipam is the class's own declaration unless vm.spec.network_overrides
    // has a matching entry (ADR-0024) — a VMClass is shared across many VMs,
    // so a static address can only ever be expressed per-VM. Gateway /
    // nameservers / domain fill in from the Provider's own per-zone subnet
    // shape for this NIC's networkClass, for whatever the override itself
    // left unset (ADR-0032) — never overriding a value the VM did set.
    let nics: Vec<VSphereNicSpec> = class
        .spec
        .network
        .interfaces
        .iter()
        .zip(decision.resolved_networks.iter())
        .map(|(nic, resolved)| {
            let override_ = vm
                .spec
                .network_overrides
                .iter()
                .find(|o| o.name == nic.name)
                .map(|o| &o.static_);
            let zone_subnet = provider
                .spec
                .capabilities
                .network_classes
                .iter()
                .find(|c| c.name == nic.network_class)
                .and_then(|c| c.subnet_for(&datacenter, &cluster));
            VSphereNicSpec {
                name: nic.name.clone(),
                port_group: resolved.backend_id.clone(),
                mac_address: None,
                ipam: merge_ipam_override(&nic.ipam, override_, zone_subnet),
            }
        })
        .collect();

    let spec = VSphereMachineSpec {
        provider_id: None,
        failure_domain: Some(decision.failure_domain_name.clone()),
        provider_ref: banlieue_api::common::LocalObjectReference {
            name: decision.provider_name.clone(),
        },
        template,
        datacenter,
        cluster,
        // Destination for the clone: vm.spec.folder wins outright when
        // set; otherwise default to the same per-zone folder the source
        // template lives in, so clones land organized the same way
        // templates already are.
        folder: vm.spec.folder.clone().or_else(|| template_folder.clone()),
        template_folder,
        datastore,
        resource_pool: None,
        num_cpus: hardware_override
            .and_then(|h| h.cpus)
            .unwrap_or(class.spec.hardware.cpus),
        memory_mi_b: hardware_override
            .and_then(|h| h.memory_mi_b)
            .unwrap_or(class.spec.hardware.memory_mi_b),
        firmware: class.spec.firmware.clone(),
        tpm_enabled: class.spec.tpm_enabled,
        disks,
        network: nics,
        user_data: rendered_user_data.map(str::to_string),
        desired_power_state: vm.spec.desired_power_state.clone(),
    };

    Ok(VSphereMachine {
        metadata: ObjectMeta {
            // Same name + namespace as the parent VM. This is the convention
            // for 1:1 owned infra CRs — keeps the relationship discoverable
            // without indexing.
            name: Some(vm.name_any()),
            namespace: vm.namespace(),
            owner_references: Some(vec![owner_reference_for(vm)]),
            labels: Some(propagate_labels(vm)),
            ..Default::default()
        },
        spec,
        status: None,
    })
}

/// Construct a controller-owning [`OwnerReference`] back to the parent VM.
fn owner_reference_for(
    vm: &VirtualMachine,
) -> k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
    OwnerReference {
        api_version: format!(
            "{}/{}",
            VirtualMachine::group(&()),
            VirtualMachine::version(&())
        ),
        kind: "VirtualMachine".to_string(),
        name: vm.name_any(),
        uid: vm.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    }
}

/// Copy a small set of labels from the parent VM onto the infra CR for
/// discoverability via `kubectl get vspheremachines -l app=db-prod`.
fn propagate_labels(vm: &VirtualMachine) -> BTreeMap<String, String> {
    let mut labels: BTreeMap<String, String> = vm.labels().clone();
    labels.insert("banlieue.io/owned-by".into(), vm.name_any());
    labels
}

/// Merge a `VMClass` NIC's declared [`IpamShape`] with an optional per-VM
/// static override (ADR-0024) and the Provider's per-zone subnet shape for
/// this NIC's resolved failure domain (ADR-0032).
///
/// `address` and `prefix` always come from the per-VM override — a
/// `VMClass` is shared by many VMs, so a concrete address can only ever be
/// per-VM, and `prefix` deliberately stays per-VM too (ADR-0032). Each of
/// `gateway` / `nameservers` / `domain` takes the override's own value when
/// the VM set one, else falls back field-by-field to `zone_subnet` — never
/// silently discarding an explicit per-VM value, but no longer requiring
/// the VM to restate a zone's topology just to set a static address.
///
/// When no override is present the class's `pool` carries through;
/// `static_` is always `None` because [`IpamShape`] deliberately omits the
/// per-VM address.
fn merge_ipam_override(
    class_ipam: &IpamShape,
    override_: Option<&StaticIpamConfig>,
    zone_subnet: Option<&SubnetShape>,
) -> IpamSpec {
    match override_ {
        Some(static_cfg) => IpamSpec {
            static_: Some(StaticIpamConfig {
                address: static_cfg.address.clone(),
                prefix: static_cfg.prefix,
                gateway: static_cfg
                    .gateway
                    .clone()
                    .or_else(|| zone_subnet.and_then(|s| s.gateway.clone())),
                nameservers: if static_cfg.nameservers.is_empty() {
                    zone_subnet
                        .map(|s| s.nameservers.clone())
                        .unwrap_or_default()
                } else {
                    static_cfg.nameservers.clone()
                },
                domain: static_cfg
                    .domain
                    .clone()
                    .or_else(|| zone_subnet.and_then(|s| s.domain.clone())),
            }),
            pool: None,
        },
        None => IpamSpec {
            static_: None,
            pool: class_ipam.pool.clone(),
        },
    }
}

/// Resolve one `VMClass` disk's `sizeGiB`: the per-VM
/// `hardwareOverride.diskOverrides` entry matching this disk's name, if
/// any, else the class's own size unchanged. A `VirtualMachine` with no
/// `hardwareOverride` at all (`None`) always inherits the class verbatim.
fn merge_disk_size_override(disk: &DiskSpec, override_: Option<&HardwareOverride>) -> u32 {
    override_
        .and_then(|h| h.disk_overrides.iter().find(|d| d.name == disk.name))
        .map(|d| d.size_gi_b)
        .unwrap_or(disk.size_gi_b)
}

/// Look up the resolved template reference for the chosen provider and
/// zone: its bare display name, plus the per-zone folder it lives in when
/// there is one.
///
/// `Template`-kind images have a single top-level `resolved_ref` on the
/// provider row and no per-zone folder (`None` — the lookup is
/// datacenter-wide). `Url`-kind images have per-zone `resolved_ref` /
/// `template_folder` entries (one per failure domain). This function
/// checks both, preferring the top-level ref (always set for `Template`
/// sources) and falling back to the zone that matches
/// Failure-domain raw-attribute key for the libvirt host a domain lands on.
/// Optional: a libvirt failure domain is one host, so the domain's placement
/// is the failure domain itself.
pub const FD_RAW_LIBVIRT_HOST: &str = "host";

/// `NetworkClassMapping.target` key naming a libvirt-managed network.
const TARGET_KEY_LIBVIRT_NETWORK: &str = "network";
/// `NetworkClassMapping.target` key naming a raw host bridge.
const TARGET_KEY_LIBVIRT_BRIDGE: &str = "bridge";

/// Build a [`LibvirtMachine`] from the scheduler [`Decision`], the original
/// VM, its class, image, and the chosen [`Provider`] (ADR-0050).
///
/// Owner-reference is set to `vm`, so the LibvirtMachine is garbage-collected
/// with its parent exactly as the vSphere one is.
///
/// # How this differs from [`build_vsphere_machine`]
///
/// - **No datacenter or cluster.** A libvirt failure domain is a single host,
///   so `Decision::failure_domain_raw` is legitimately empty. Treating its
///   absence as an error — as the vSphere builder correctly does — would mean
///   nothing ever schedules onto libvirt.
/// - **The image resolves to a volume, not a vCenter template**, and whether
///   that volume is a backing image or an installer ISO is decided by the
///   `VMImage`'s install mode rather than by anything the provider knows.
/// - **The NIC's `kind` comes off the Provider, not the `Decision`.** The
///   scheduler flattens a network class's target map to its first *value*,
///   which loses the key that distinguishes `{bridge: br0}` from
///   `{network: default}`. The key survives only on
///   `Provider.spec.capabilities`, so that is where this reads it from.
///
/// # Errors
/// [`InfraBuildError::MissingResolvedImageRef`] when the `VMImage` has no
/// resolved volume for this provider, or
/// [`InfraBuildError::UnresolvedClass`] when the class declared no disks.
pub fn build_libvirt_machine(
    vm: &VirtualMachine,
    class: &VMClass,
    image: &VMImage,
    decision: &Decision,
    provider: &Provider,
    rendered_user_data: Option<&str>,
) -> Result<LibvirtMachine, InfraBuildError> {
    // The OS disk's resolved backend id is the storage pool.
    let pool = decision
        .resolved_storage
        .first()
        .ok_or_else(|| InfraBuildError::UnresolvedClass("(no disks)".into()))?
        .backend_id
        .clone();

    // libvirt has no per-zone template folder, so only the name is used.
    let (volume, _folder) = resolve_template_ref(
        image,
        &decision.provider_name,
        &decision.failure_domain_name,
    )
    .ok_or_else(|| InfraBuildError::MissingResolvedImageRef {
        image: image.name_any(),
        provider: decision.provider_name.clone(),
        zone: decision.failure_domain_name.clone(),
    })?;

    let boot_source = LibvirtBootSource {
        kind: boot_source_kind(image),
        volume,
    };

    let hardware_override = vm.spec.hardware_override.as_ref();

    // VMClass disks carry no bus — it is a libvirt-only concept — so every
    // disk gets the default. Every image banlieue builds has virtio drivers;
    // a guest that does not is a per-machine edit, not a class-wide one.
    let disks: Vec<LibvirtDiskSpec> = class
        .spec
        .hardware
        .disks
        .iter()
        .map(|d| LibvirtDiskSpec {
            name: d.name.clone(),
            size_gi_b: merge_disk_size_override(d, hardware_override),
            bus: LibvirtDiskBus::default(),
        })
        .collect();

    let nics: Vec<LibvirtNicSpec> = class
        .spec
        .network
        .interfaces
        .iter()
        .zip(decision.resolved_networks.iter())
        .map(|(nic, resolved)| {
            let override_ = vm
                .spec
                .network_overrides
                .iter()
                .find(|o| o.name == nic.name)
                .map(|o| &o.static_);
            // libvirt failure domains carry no datacenter/cluster, so the
            // per-zone lookup falls through to the mapping's default target
            // — which is the documented behaviour for a backend with no zone
            // hierarchy (ADR-0030).
            let mapping = provider
                .spec
                .capabilities
                .network_classes
                .iter()
                .find(|c| c.name == nic.network_class);
            let zone_subnet = mapping.and_then(|c| c.subnet_for("", ""));
            LibvirtNicSpec {
                name: nic.name.clone(),
                source: LibvirtNicSource {
                    kind: nic_source_kind(mapping),
                    name: resolved.backend_id.clone(),
                },
                model: None,
                mac_address: None,
                ipam: merge_ipam_override(&nic.ipam, override_, zone_subnet),
            }
        })
        .collect();

    let spec = LibvirtMachineSpec {
        provider_id: None,
        failure_domain: Some(decision.failure_domain_name.clone()),
        provider_ref: banlieue_api::common::LocalObjectReference {
            name: decision.provider_name.clone(),
        },
        pool,
        domain_name: domain_name_for(vm),
        boot_source,
        vcpus: hardware_override
            .and_then(|h| h.cpus)
            .unwrap_or(class.spec.hardware.cpus),
        memory_mi_b: hardware_override
            .and_then(|h| h.memory_mi_b)
            .unwrap_or(class.spec.hardware.memory_mi_b),
        firmware: class.spec.firmware.clone(),
        machine_type: None,
        tpm_enabled: class.spec.tpm_enabled,
        disks,
        network: nics,
        user_data: rendered_user_data.map(str::to_string),
        desired_power_state: vm.spec.desired_power_state.clone(),
    };

    Ok(LibvirtMachine {
        metadata: ObjectMeta {
            name: Some(vm.name_any()),
            namespace: vm.namespace(),
            owner_references: Some(vec![owner_reference_for(vm)]),
            labels: Some(propagate_labels(vm)),
            ..Default::default()
        },
        spec,
        status: None,
    })
}

/// The libvirt domain name for `vm`.
///
/// Namespace-qualified, unlike the CR's own name. One libvirt host has a
/// single flat domain namespace, while two Kubernetes namespaces can each
/// hold a `VirtualMachine` called `db-01` — and `DOMAIN_DEFINE_XML` is an
/// upsert, so the second one would silently redefine the first's domain
/// rather than failing. vCenter has no equivalent problem because its
/// inventory is a folder tree.
fn domain_name_for(vm: &VirtualMachine) -> String {
    match vm.namespace() {
        Some(ns) => format!("{ns}-{}", vm.name_any()),
        None => vm.name_any(),
    }
}

/// Which provisioning shape an image's install mode calls for (ADR-0040).
///
/// `Manual` groups with `Deferred` because their mechanics are identical —
/// attach the media and let the guest install itself; the two names exist to
/// distinguish *why*, not *what* (ADR-0040's `InstallMode` doc).
fn boot_source_kind(image: &VMImage) -> LibvirtBootSourceKind {
    let mode = image
        .spec
        .template
        .as_ref()
        .map_or(InstallMode::default(), |t| t.install_mode);
    match mode {
        InstallMode::Immediate => LibvirtBootSourceKind::BackingVolume,
        InstallMode::Deferred | InstallMode::Manual => LibvirtBootSourceKind::InstallMedia,
    }
}

/// Build a [`CloudHypervisorMachine`] from the scheduler [`Decision`], the
/// original VM, its class, image, and the chosen [`Provider`] (ADR-0062).
///
/// Owner-reference is set to `vm`, as for the other backends.
///
/// # How this differs from [`build_libvirt_machine`]
///
/// - **Classes stay names.** The scheduler resolves the VMClass's storage
///   and network classes through `Provider.spec.capabilities`, and on this
///   backend the resolved ids are the host's own class names. The host
///   turns them into a directory and a bridge from its local config; a path
///   or a bridge name never crosses into the cluster (ADR-0062 Decision 4).
/// - **One disk.** The first class disk is the OS disk, grown to its size
///   before first boot. Data disks are not in the first slice, and a class
///   that declares one is refused rather than silently shortened.
/// - **No domain name.** Nothing on the host outlives the VMM process; the
///   provider names everything after the machine's UID.
///
/// # Errors
/// [`InfraBuildError::MissingResolvedImageRef`] when the `VMImage` has no
/// resolved image for this provider, [`InfraBuildError::UnresolvedClass`]
/// when the class declared no disk, and [`InfraBuildError::Unsupported`]
/// when it declared more than one.
pub fn build_cloud_hypervisor_machine(
    vm: &VirtualMachine,
    class: &VMClass,
    image: &VMImage,
    decision: &Decision,
    provider: &Provider,
    rendered_user_data: Option<&str>,
) -> Result<CloudHypervisorMachine, InfraBuildError> {
    let hardware_override = vm.spec.hardware_override.as_ref();

    let disks = &class.spec.hardware.disks;
    let os_disk = disks
        .first()
        .ok_or_else(|| InfraBuildError::UnresolvedClass("(no disks)".into()))?;
    if disks.len() > 1 {
        return Err(InfraBuildError::Unsupported {
            backend: PROVIDER_CLASS_CLOUD_HYPERVISOR,
            what: format!(
                "data disks (class {} declares {})",
                class.name_any(),
                disks.len()
            ),
        });
    }
    let storage_class = decision
        .resolved_storage
        .first()
        .ok_or_else(|| InfraBuildError::UnresolvedClass(os_disk.storage_class.clone()))?
        .backend_id
        .clone();

    let (image_ref, _folder) = resolve_template_ref(
        image,
        &decision.provider_name,
        &decision.failure_domain_name,
    )
    .ok_or_else(|| InfraBuildError::MissingResolvedImageRef {
        image: image.name_any(),
        provider: decision.provider_name.clone(),
        zone: decision.failure_domain_name.clone(),
    })?;

    let nics: Vec<ChNicSpec> = class
        .spec
        .network
        .interfaces
        .iter()
        .zip(decision.resolved_networks.iter())
        .map(|(nic, resolved)| {
            let override_ = vm
                .spec
                .network_overrides
                .iter()
                .find(|o| o.name == nic.name)
                .map(|o| &o.static_);
            // One host is one failure domain, with no zone hierarchy, so the
            // mapping's default subnet applies (ADR-0030), as on libvirt.
            let zone_subnet = provider
                .spec
                .capabilities
                .network_classes
                .iter()
                .find(|c| c.name == nic.network_class)
                .and_then(|c| c.subnet_for("", ""));
            ChNicSpec {
                name: nic.name.clone(),
                network_class: resolved.backend_id.clone(),
                mac_address: None,
                ipam: merge_ipam_override(&nic.ipam, override_, zone_subnet),
            }
        })
        .collect();

    let spec = CloudHypervisorMachineSpec {
        provider_id: None,
        failure_domain: Some(decision.failure_domain_name.clone()),
        provider_ref: banlieue_api::common::LocalObjectReference {
            name: decision.provider_name.clone(),
        },
        cpus: ChCpuSpec {
            boot: hardware_override
                .and_then(|h| h.cpus)
                .unwrap_or(class.spec.hardware.cpus),
            max: None,
        },
        memory: ChMemorySpec {
            size_mi_b: hardware_override
                .and_then(|h| h.memory_mi_b)
                .unwrap_or(class.spec.hardware.memory_mi_b),
            hugepages: false,
        },
        storage_class,
        boot_source: ChBootSource {
            kind: ch_boot_source_kind(image),
            image: image_ref,
        },
        os_disk_size_gi_b: merge_disk_size_override(os_disk, hardware_override),
        nics,
        tpm_enabled: class.spec.tpm_enabled,
        user_data: rendered_user_data.map(str::to_string),
        desired_power_state: vm.spec.desired_power_state.clone(),
    };

    Ok(CloudHypervisorMachine {
        metadata: ObjectMeta {
            name: Some(vm.name_any()),
            namespace: vm.namespace(),
            owner_references: Some(vec![owner_reference_for(vm)]),
            labels: Some(propagate_labels(vm)),
            ..Default::default()
        },
        spec,
        status: None,
    })
}

/// Which provisioning shape an image's install mode calls for on Cloud
/// Hypervisor, the counterpart of [`boot_source_kind`] (ADR-0040, ADR-0065).
fn ch_boot_source_kind(image: &VMImage) -> ChBootSourceKind {
    let mode = image
        .spec
        .template
        .as_ref()
        .map_or(InstallMode::default(), |t| t.install_mode);
    match mode {
        InstallMode::Immediate => ChBootSourceKind::Image,
        InstallMode::Deferred | InstallMode::Manual => ChBootSourceKind::InstallMedia,
    }
}

/// Whether a network class names a bridge or a libvirt-managed network.
///
/// Defaults to a managed network when the mapping declares neither, matching
/// the scheduler's own fallback. Guessing "bridge" would be the worse
/// default: a raw bridge puts the guest directly onto a host L2 segment,
/// while a managed network is the contained option.
fn nic_source_kind(
    mapping: Option<&banlieue_api::banlieue::NetworkClassMapping>,
) -> LibvirtNicSourceKind {
    let target = mapping.and_then(|m| m.target_for("", ""));
    match target {
        Some(t) if t.contains_key(TARGET_KEY_LIBVIRT_BRIDGE) => LibvirtNicSourceKind::Bridge,
        Some(t) if t.contains_key(TARGET_KEY_LIBVIRT_NETWORK) => LibvirtNicSourceKind::Network,
        _ => LibvirtNicSourceKind::Network,
    }
}

/// `failure_domain_name`.
fn resolve_template_ref(
    image: &VMImage,
    provider_name: &str,
    failure_domain_name: &str,
) -> Option<(String, Option<String>)> {
    image.status.as_ref().and_then(|s| {
        let row = s
            .per_provider
            .iter()
            .find(|p: &&ImagePerProviderStatus| p.provider_name == provider_name)?;

        // Template-kind: top-level resolved_ref, no per-zone folder.
        if let Some(ref r) = row.resolved_ref {
            return Some((r.clone(), None));
        }

        // Url-kind: per-zone resolved_ref + template_folder.
        let zone = row.zones.iter().find(|z| z.name == failure_domain_name)?;
        let name = zone.resolved_ref.clone()?;
        Some((name, zone.template_folder.clone()))
    })
}

// Convenience accessors used by status_mirror tests. Suppresses unused-warning
// noise on the trait import once everything is wired.
#[allow(dead_code)]
pub(crate) fn unused_addresses_marker(addrs: &[MachineAddress]) -> usize {
    addrs.len()
}
#[allow(dead_code)]
pub(crate) fn unused_ipam_marker(_i: &IpamSpec) {}

#[cfg(test)]
#[path = "infra_tests.rs"]
mod infra_tests;
