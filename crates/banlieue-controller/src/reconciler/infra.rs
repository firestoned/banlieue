// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Build provider-specific infrastructure CRs from a scheduler [`Decision`].
//!
//! Six backends: `vsphere` ([`build_vsphere_machine`]), `libvirt`
//! ([`build_libvirt_machine`], ADR-0050), `cloud-hypervisor`
//! ([`build_cloud_hypervisor_machine`], ADR-0062), `proxmox`
//! ([`build_proxmox_machine`], ADR-0075), `azure`
//! ([`build_azure_virtual_machine`], ADR-0164) and `ec2`
//! ([`build_ec2_machine`], ADR-0174). Dispatch by
//! `Provider.spec.providerClassRef.name` lives in [`super::virtualmachine`],
//! not here — these builders are pure and know only their own backend.

use std::collections::BTreeMap;

use banlieue_api::aws;
use banlieue_api::banlieue::InstallMode;
use banlieue_api::banlieue::{
    ANNOTATION_CREATED_BY, DiskSpec, HardwareOverride, ImagePerProviderStatus, ImageSourceKind,
    NetworkClassMapping, NetworkInterfaceSpec, Provider, SubnetShape, VMClass, VMImage,
    VirtualMachine,
};
use banlieue_api::common::{FirstBootSealing, IpamShape, IpamSpec, StaticIpamConfig};
use banlieue_api::infrastructure::{
    AzureVirtualMachine, AzureVirtualMachineSpec, ChBootSource, ChBootSourceKind, ChCpuSpec,
    ChMemorySpec, ChNicSpec, CloudHypervisorMachine, CloudHypervisorMachineSpec,
    DEFAULT_ROOT_DEVICE_NAME, EC2Machine, EC2MachineSpec, Ec2VolumeType, LibvirtBootSource,
    LibvirtBootSourceKind, LibvirtDiskBus, LibvirtDiskSpec, LibvirtMachine, LibvirtMachineSpec,
    LibvirtNicSource, LibvirtNicSourceKind, LibvirtNicSpec, ProxmoxDataDisk, ProxmoxMachine,
    ProxmoxMachineSpec, ProxmoxNicModel, ProxmoxNicSpec, VSphereDiskSpec, VSphereMachine,
    VSphereMachineSpec, VSphereNicSpec,
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
/// `Provider.spec.providerClassRef.name` for the Proxmox VE backend
/// (ADR-0074, ADR-0075).
pub const PROVIDER_CLASS_PROXMOX: &str = "proxmox";
/// `Provider.spec.providerClassRef.name` for the Azure backend (ADR-0158,
/// ADR-0164).
pub const PROVIDER_CLASS_AZURE: &str = "azure";
/// `Provider.spec.providerClassRef.name` for the Amazon EC2 backend
/// (ADR-0174).
pub const PROVIDER_CLASS_EC2: &str = "ec2";

/// Provider classes that are clouds: their failure domains are availability
/// zones and every `VMClass` reaches them through an explicit instance-size
/// mapping (ADR-0158 Decision 3, ADR-0160 Decision 1). The scheduler treats
/// an empty `instanceSizes` on one of these as "nothing is schedulable".
pub const CLOUD_PROVIDER_CLASSES: &[&str] = &[PROVIDER_CLASS_AZURE, PROVIDER_CLASS_EC2];

/// Failure-domain key (in `attributes.raw`, else `labels`) carrying the
/// cloud availability zone a domain stands for (ADR-0158 Decision 3).
pub const FD_ZONE_KEY: &str = "zone";

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
    /// `infrastructure.banlieue.io/ProxmoxMachine` (ADR-0075).
    Proxmox,
    /// `infrastructure.banlieue.io/AzureVirtualMachine` (ADR-0164).
    Azure,
    /// `infrastructure.banlieue.io/EC2Machine` (ADR-0174).
    Ec2,
}

impl InfraKind {
    /// Map a `Provider.spec.providerClassRef.name` onto its infra kind.
    ///
    /// `None` for a provider class this controller has no builder for. That
    /// is a real state, not an impossible one: a `ProviderClass` can name any
    /// class, while the set of builders is closed to these six (see
    /// `docs/src/concepts/providers.md`, "Adding a provider"). It must
    /// surface as a condition rather than a panic or a silent no-op.
    #[must_use]
    pub fn from_provider_class(class: &str) -> Option<Self> {
        match class {
            PROVIDER_CLASS_VSPHERE => Some(Self::VSphere),
            PROVIDER_CLASS_LIBVIRT => Some(Self::Libvirt),
            PROVIDER_CLASS_CLOUD_HYPERVISOR => Some(Self::CloudHypervisor),
            PROVIDER_CLASS_PROXMOX => Some(Self::Proxmox),
            PROVIDER_CLASS_AZURE => Some(Self::Azure),
            PROVIDER_CLASS_EC2 => Some(Self::Ec2),
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
            Self::Proxmox => "ProxmoxMachine",
            Self::Azure => "AzureVirtualMachine",
            Self::Ec2 => "EC2Machine",
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

    /// A Proxmox image's `resolvedRef` is the template's VMID (ADR-0075
    /// Decision 5a), and this one is not a number.
    #[error("VMImage {image} resolvedRef {resolved_ref:?} is not a Proxmox template VMID")]
    InvalidTemplateVmid { image: String, resolved_ref: String },

    /// A Proxmox network class's `vlan` target key is not a VLAN id.
    #[error("network class {class} has vlan {value:?}, which is not a VLAN id")]
    InvalidVlan { class: String, value: String },

    /// The assembled infra spec failed its own cross-field validation.
    #[error("built spec is invalid: {0}")]
    InvalidSpec(String),

    /// A cloud failure domain carries no availability zone in
    /// `attributes.raw.zone` or `labels.zone` (ADR-0158 Decision 3).
    #[error("failure domain {0} carries no availability zone (attributes.raw.zone or labels.zone)")]
    MissingZone(String),

    /// The `Provider` maps no instance size for this `VMClass`
    /// (ADR-0160 Decision 1). The scheduler normally rejects the placement
    /// first; this guards a Provider edited between the two.
    #[error(
        "VMClass {vm_class} has no instance size on Provider {provider} (zone {zone}); add it to spec.capabilities.instanceSizes (ADR-0160)"
    )]
    UnmappedInstanceSize {
        vm_class: String,
        provider: String,
        zone: String,
    },

    /// The `VMImage` has no source for this provider class of the kind the
    /// backend boots from (an `azure` `Template`, ADR-0162 Decision 4).
    #[error("VMImage {image} has no {provider_class} {kind} source (ADR-0162 Decision 4)")]
    MissingImageSource {
        image: String,
        provider_class: &'static str,
        kind: &'static str,
    },

    /// The image source's `ref` is not the cloud resource identifier the
    /// backend needs.
    #[error("VMImage {image} ref {reference:?} is not {expected}")]
    InvalidImageReference {
        image: String,
        reference: String,
        expected: &'static str,
    },

    /// A storage or network class target names a value the backend cannot
    /// take (an EBS volume type it does not create, IOPS that are not a
    /// number).
    #[error("{what} class {class} has {key} {value:?} in zone {zone}, which is not {expected}")]
    InvalidTarget {
        what: &'static str,
        class: String,
        key: &'static str,
        value: String,
        zone: String,
        expected: &'static str,
    },

    /// A storage or network class has no target, or no required key in it,
    /// for the chosen zone.
    #[error("{what} class {class} has no '{key}' target in zone {zone}")]
    MissingTarget {
        what: &'static str,
        class: String,
        key: &'static str,
        zone: String,
    },
}

/// The name of the infrastructure machine `vm` owns (ADR-0127 Decision 4): the one its
/// `status.infrastructureRef` names, else the VM's own name.
///
/// A machine is created under the VM's name, but a moved VM owns its move's destination,
/// `<vm>-<move id>`, and every path that reads, applies, recreates or finalizes a VM's machine
/// goes through here, so none of them finds nothing under the VM's name and makes a second one.
/// A VM whose status names no machine (placed before ADR-0127, or not placed yet) owns the one
/// under its own name.
#[must_use]
pub fn machine_name(vm: &VirtualMachine) -> String {
    vm.status
        .as_ref()
        .and_then(|s| s.infrastructure_ref.as_ref())
        .map(|r| r.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| vm.name_any())
}

/// The `providerRef` for a machine bound for `provider` (ADR-0133 Decision 3).
///
/// A `Provider` in the VM's own namespace is named by its bare name, as it
/// always was. One in another namespace (a shared provider in the provider
/// namespace) is named with its namespace, so the machine can stay beside its
/// VM, pool and claim while the provider reads its own `Provider` and
/// credentials from its own namespace.
fn provider_reference(
    vm: &VirtualMachine,
    provider: &Provider,
    name: &str,
) -> banlieue_api::common::ProviderReference {
    let namespace = provider
        .namespace()
        .filter(|ns| Some(ns.as_str()) != vm.namespace().as_deref());
    banlieue_api::common::ProviderReference {
        name: name.to_string(),
        namespace,
    }
}

/// The metadata every infra CR carries: the machine's name ([`machine_name`]),
/// the parent VM's namespace, a controller owner reference back to the VM, and
/// the VM's labels. Same namespace as the parent VM is the convention for 1:1
/// owned infra CRs: it keeps the relationship discoverable without indexing.
fn infra_meta(vm: &VirtualMachine) -> ObjectMeta {
    ObjectMeta {
        name: Some(machine_name(vm)),
        namespace: vm.namespace(),
        owner_references: Some(vec![owner_reference_for(vm)]),
        labels: Some(propagate_labels(vm)),
        ..Default::default()
    }
}

/// The chosen failure domain's raw attribute `key`, or
/// [`InfraBuildError::MissingFdRaw`] naming the domain and the key.
fn fd_raw(decision: &Decision, key: &'static str) -> Result<String, InfraBuildError> {
    decision
        .failure_domain_raw
        .get(key)
        .cloned()
        .ok_or_else(|| InfraBuildError::MissingFdRaw(decision.failure_domain_name.clone(), key))
}

/// [`resolve_template_ref`] for the decision's provider and failure domain, or
/// [`InfraBuildError::MissingResolvedImageRef`].
fn required_template_ref(
    image: &VMImage,
    decision: &Decision,
) -> Result<(String, Option<String>), InfraBuildError> {
    resolve_template_ref(
        image,
        &decision.provider_name,
        &decision.failure_domain_name,
    )
    .ok_or_else(|| InfraBuildError::MissingResolvedImageRef {
        image: image.name_any(),
        provider: decision.provider_name.clone(),
        zone: decision.failure_domain_name.clone(),
    })
}

/// What every backend looks up per class NIC: the VM's static override for
/// it (ADR-0024), if any, and the Provider's mapping for its network class.
fn nic_inputs<'a>(
    vm: &'a VirtualMachine,
    provider: &'a Provider,
    nic: &NetworkInterfaceSpec,
) -> (
    Option<&'a StaticIpamConfig>,
    Option<&'a NetworkClassMapping>,
) {
    let override_ = vm
        .spec
        .network_overrides
        .iter()
        .find(|o| o.name == nic.name)
        .map(|o| &o.static_);
    let mapping = provider
        .spec
        .capabilities
        .network_classes
        .iter()
        .find(|c| c.name == nic.network_class);
    (override_, mapping)
}

/// An image's install mode (ADR-0040): its template's, or the default for an
/// image with no template.
pub(crate) fn install_mode(image: &VMImage) -> InstallMode {
    image
        .spec
        .template
        .as_ref()
        .map_or(InstallMode::default(), |t| t.install_mode)
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
    let datacenter = fd_raw(decision, FD_RAW_VSPHERE_DATACENTER)?;
    let cluster = fd_raw(decision, FD_RAW_VSPHERE_CLUSTER)?;

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
    let (template, template_folder) = required_template_ref(image, decision)?;
    let template = bare_template_name(&template).to_string();

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
            let (override_, mapping) = nic_inputs(vm, provider, nic);
            let zone_subnet = mapping.and_then(|c| c.subnet_for(&datacenter, &cluster));
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
        provider_ref: provider_reference(vm, provider, &decision.provider_name),
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
        num_cpus: effective_cpus(class, hardware_override),
        memory_mi_b: effective_memory_mi_b(class, hardware_override),
        firmware: class.spec.firmware.clone(),
        tpm_enabled: class.spec.tpm_enabled,
        disks,
        network: nics,
        user_data: rendered_user_data.map(str::to_string),
        desired_power_state: vm.spec.desired_power_state.clone(),
        // ADR-0089: present only when the optional created-by
        // MutatingAdmissionPolicy stamped the annotation on this VM.
        created_by: vm.annotations().get(ANNOTATION_CREATED_BY).cloned(),
        first_boot_sealing: first_boot_sealing_for(class, image),
    };

    Ok(VSphereMachine {
        metadata: infra_meta(vm),
        spec,
        status: None,
    })
}

/// Construct a controller-owning [`OwnerReference`] back to the parent VM.
/// The first-boot sealing a machine's guest must report before `GuestReady`
/// (ADR-0088): the image's declaration, on a vTPM machine only. The pairing
/// check has already refused a declaration without a vTPM, so the `tpmEnabled`
/// test here is belt and braces: a machine with nothing to seal to must never
/// wait for a report that cannot come.
fn first_boot_sealing_for(class: &VMClass, image: &VMImage) -> Option<FirstBootSealing> {
    if !class.spec.tpm_enabled {
        return None;
    }
    image.spec.first_boot_sealing.clone()
}

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
pub(crate) fn merge_disk_size_override(
    disk: &DiskSpec,
    override_: Option<&HardwareOverride>,
) -> u32 {
    override_
        .and_then(|h| h.disk_overrides.iter().find(|d| d.name == disk.name))
        .map(|d| d.size_gi_b)
        .unwrap_or(disk.size_gi_b)
}

/// The VM's vCPUs: its `hardwareOverride.cpus` when set, else the class's.
/// The scheduler sizes a placement with this and every infra builder
/// provisions with it, so the two never disagree.
pub(crate) fn effective_cpus(class: &VMClass, override_: Option<&HardwareOverride>) -> u32 {
    override_
        .and_then(|h| h.cpus)
        .unwrap_or(class.spec.hardware.cpus)
}

/// The VM's memory, MiB: its `hardwareOverride.memoryMiB` when set, else
/// the class's. See [`effective_cpus`].
pub(crate) fn effective_memory_mi_b(class: &VMClass, override_: Option<&HardwareOverride>) -> u32 {
    override_
        .and_then(|h| h.memory_mi_b)
        .unwrap_or(class.spec.hardware.memory_mi_b)
}

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
    let (volume, _folder) = required_template_ref(image, decision)?;

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
            // libvirt failure domains carry no datacenter/cluster, so the
            // per-zone lookup falls through to the mapping's default target,
            // which is the documented behaviour for a backend with no zone
            // hierarchy (ADR-0030).
            let (override_, mapping) = nic_inputs(vm, provider, nic);
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
        provider_ref: provider_reference(vm, provider, &decision.provider_name),
        pool,
        domain_name: domain_name_for(vm),
        boot_source,
        vcpus: effective_cpus(class, hardware_override),
        memory_mi_b: effective_memory_mi_b(class, hardware_override),
        firmware: class.spec.firmware.clone(),
        machine_type: None,
        tpm_enabled: class.spec.tpm_enabled,
        disks,
        network: nics,
        user_data: rendered_user_data.map(str::to_string),
        desired_power_state: vm.spec.desired_power_state.clone(),
        first_boot_sealing: first_boot_sealing_for(class, image),
    };

    Ok(LibvirtMachine {
        metadata: infra_meta(vm),
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
    match install_mode(image) {
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

    let (image_ref, _folder) = required_template_ref(image, decision)?;

    let nics: Vec<ChNicSpec> = class
        .spec
        .network
        .interfaces
        .iter()
        .zip(decision.resolved_networks.iter())
        .map(|(nic, resolved)| {
            // One host is one failure domain, with no zone hierarchy, so the
            // mapping's default subnet applies (ADR-0030), as on libvirt.
            let (override_, mapping) = nic_inputs(vm, provider, nic);
            let zone_subnet = mapping.and_then(|c| c.subnet_for("", ""));
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
        provider_ref: provider_reference(vm, provider, &decision.provider_name),
        cpus: ChCpuSpec {
            boot: effective_cpus(class, hardware_override),
            max: None,
        },
        memory: ChMemorySpec {
            size_mi_b: effective_memory_mi_b(class, hardware_override),
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
        first_boot_sealing: first_boot_sealing_for(class, image),
        identity: None,
        move_in: None,
        move_out: None,
    };

    Ok(CloudHypervisorMachine {
        metadata: infra_meta(vm),
        spec,
        status: None,
    })
}

/// Failure-domain raw-attribute key for the Proxmox node a domain is.
pub const FD_RAW_PROXMOX_NODE: &str = "node";
/// Name of the `Provider` storage class that holds the NoCloud seed ISO
/// (ADR-0075 Decision 3). Looked up only when the VM has user data.
///
/// The seed's storage is a Provider-level fact, not a per-VM disk, so it is
/// not a `VMClass` disk class; a well-known class name on
/// `Provider.spec.capabilities` needs no new API field.
pub const PROXMOX_SEED_STORAGE_CLASS: &str = "seed-iso";
/// Target key naming a Proxmox storage id.
const TARGET_KEY_PROXMOX_STORAGE: &str = "storage";
/// Target key naming a Proxmox bridge.
const TARGET_KEY_PROXMOX_BRIDGE: &str = "bridge";
/// Target key carrying an optional 802.1Q tag.
const TARGET_KEY_PROXMOX_VLAN: &str = "vlan";
/// CPU sockets per VM. The class declares total vCPUs, so all of them go in
/// `cores` on one socket.
const PROXMOX_SOCKETS: u32 = 1;

/// Build a [`ProxmoxMachine`] from the scheduler [`Decision`], the original
/// VM, its class, image, and the chosen [`Provider`] (ADR-0075).
///
/// Owner-reference is set to `vm`, as for the other backends.
///
/// # Where each resolved field comes from
///
/// - **`node`**: the failure domain's raw `node` attribute (one node is one
///   failure domain, roadmap 06).
/// - **`templateVmid`**: the `VMImage`'s per-provider `resolvedRef`, parsed
///   as a VMID, exactly as vSphere takes its template name from the same
///   field. The Proxmox provider's image reconciler is what publishes it.
/// - **`storage`**: the OS disk's resolved backend id (`{storage: <id>}`).
///   Class disks after the first become `dataDisks`, on their own storage
///   when it differs from the OS disk's.
/// - **NIC `bridge` / `vlan`**: read off the Provider's network class target
///   by key. The scheduler flattens a target map to its first value, which
///   would lose `vlan`, so the mapping itself is consulted (as libvirt does
///   for `bridge` vs `network`).
/// - **`isoStorage`**: the Provider's `seed-iso` storage class, only when
///   there is user data to deliver.
///
/// # Errors
/// [`InfraBuildError::MissingFdRaw`] without a `node`;
/// [`InfraBuildError::MissingResolvedImageRef`] /
/// [`InfraBuildError::InvalidTemplateVmid`] for the image;
/// [`InfraBuildError::UnresolvedClass`] for a class with no disks or user
/// data without a `seed-iso` class; [`InfraBuildError::InvalidVlan`]; and
/// [`InfraBuildError::InvalidSpec`] when the result fails
/// [`ProxmoxMachineSpec::validate`].
pub fn build_proxmox_machine(
    vm: &VirtualMachine,
    class: &VMClass,
    image: &VMImage,
    decision: &Decision,
    provider: &Provider,
    rendered_user_data: Option<&str>,
) -> Result<ProxmoxMachine, InfraBuildError> {
    let node = fd_raw(decision, FD_RAW_PROXMOX_NODE)?;

    let disks = &class.spec.hardware.disks;
    let os_disk = disks
        .first()
        .ok_or_else(|| InfraBuildError::UnresolvedClass("(no disks)".into()))?;
    let storage = decision
        .resolved_storage
        .first()
        .ok_or_else(|| InfraBuildError::UnresolvedClass(os_disk.storage_class.clone()))?
        .backend_id
        .clone();

    // The template is a VMID, published as the image's resolvedRef; the
    // per-zone folder vSphere needs has no Proxmox counterpart.
    let (resolved_ref, _folder) = required_template_ref(image, decision)?;
    let template_vmid =
        resolved_ref
            .parse::<u32>()
            .map_err(|_| InfraBuildError::InvalidTemplateVmid {
                image: image.name_any(),
                resolved_ref: resolved_ref.clone(),
            })?;

    let hardware_override = vm.spec.hardware_override.as_ref();

    let data_disks = disks
        .iter()
        .zip(decision.resolved_storage.iter())
        .skip(1)
        .map(|(d, resolved)| ProxmoxDataDisk {
            name: d.name.clone(),
            size_gi_b: merge_disk_size_override(d, hardware_override),
            storage: (resolved.backend_id != storage).then(|| resolved.backend_id.clone()),
            iothread: false,
            discard: false,
            ssd: false,
        })
        .collect::<Vec<_>>();
    if data_disks.len() + 1 < disks.len() {
        let unresolved = &disks[decision.resolved_storage.len()];
        return Err(InfraBuildError::UnresolvedClass(
            unresolved.storage_class.clone(),
        ));
    }

    let nics = class
        .spec
        .network
        .interfaces
        .iter()
        .zip(decision.resolved_networks.iter())
        .map(|(nic, resolved)| {
            // A Proxmox node has no datacenter/cluster hierarchy, so the
            // mapping's default target and subnet apply (ADR-0030).
            let (override_, mapping) = nic_inputs(vm, provider, nic);
            let target = mapping.and_then(|m| m.target_for("", ""));
            let bridge = target
                .and_then(|t| t.get(TARGET_KEY_PROXMOX_BRIDGE))
                .cloned()
                .unwrap_or_else(|| resolved.backend_id.clone());
            let vlan = match target.and_then(|t| t.get(TARGET_KEY_PROXMOX_VLAN)) {
                None => None,
                Some(v) => Some(v.parse::<u16>().map_err(|_| InfraBuildError::InvalidVlan {
                    class: nic.network_class.clone(),
                    value: v.clone(),
                })?),
            };
            let zone_subnet = mapping.and_then(|c| c.subnet_for("", ""));
            Ok(ProxmoxNicSpec {
                name: nic.name.clone(),
                bridge,
                vlan,
                model: ProxmoxNicModel::default(),
                mac_address: None,
                ipam: merge_ipam_override(&nic.ipam, override_, zone_subnet),
            })
        })
        .collect::<Result<Vec<_>, InfraBuildError>>()?;

    // The seed ISO's storage is only needed when there is user data.
    let iso_storage = match rendered_user_data {
        None => None,
        Some(_) => Some(seed_storage(provider)?),
    };

    let spec = ProxmoxMachineSpec {
        provider_id: None,
        failure_domain: Some(decision.failure_domain_name.clone()),
        provider_ref: provider_reference(vm, provider, &decision.provider_name),
        node,
        template_vmid,
        storage,
        pool: None,
        cores: effective_cpus(class, hardware_override),
        sockets: PROXMOX_SOCKETS,
        memory_mi_b: effective_memory_mi_b(class, hardware_override),
        cpu_type: None,
        firmware: class.spec.firmware.clone(),
        tpm_enabled: class.spec.tpm_enabled,
        os_disk_size_gi_b: merge_disk_size_override(os_disk, hardware_override),
        data_disks,
        nics,
        iso_storage,
        user_data: rendered_user_data.map(str::to_string),
        desired_power_state: vm.spec.desired_power_state.clone(),
    };
    spec.validate().map_err(InfraBuildError::InvalidSpec)?;

    Ok(ProxmoxMachine {
        metadata: infra_meta(vm),
        spec,
        status: None,
    })
}

/// Target key naming an Azure managed-disk SKU (ADR-0160 Decision 4).
const TARGET_KEY_AZURE_SKU: &str = "sku";
/// Target key naming an Azure subnet resource ID (ADR-0160 Decision 4).
const TARGET_KEY_AZURE_SUBNET_ID: &str = "subnetId";
/// Target key enabling accelerated networking when its value is
/// [`TARGET_VALUE_TRUE`].
const TARGET_KEY_AZURE_ACCELERATED_NETWORKING: &str = "acceleratedNetworking";
/// The one target value that turns a boolean key on.
const TARGET_VALUE_TRUE: &str = "true";
/// Prefix of every ARM resource ID.
const ARM_RESOURCE_ID_PREFIX: &str = "/subscriptions/";
/// NICs an Azure VM takes in v1 (ADR-0164).
const AZURE_MAX_NICS: usize = 1;

/// Build an [`AzureVirtualMachine`] from the scheduler [`Decision`], the
/// original VM, its class, image, and the chosen [`Provider`] (ADR-0164).
///
/// Owner-reference is set to `vm`, as for the other backends. Every field is
/// resolved here so the provider interprets nothing.
///
/// # Where each resolved field comes from
///
/// - **`zone`**: the failure domain's raw `zone` attribute, else its `zone`
///   label (ADR-0158 Decision 3).
/// - **`size`**: `Provider.spec.capabilities.instanceSizes` for the
///   `VMClass` in that zone, a per-zone entry winning (ADR-0160 Decision 1).
/// - **`imageVersionId`**: the `ref` of the `VMImage`'s `azure` `Template`
///   source, or for a `Url` source the version banlieue published into this
///   `Provider` (its row's `resolvedRef`), a Compute Gallery image version
///   resource ID (ADR-0162 Decision 4).
/// - **`osDiskSizeGiB`**: the class's OS disk, after the VM's disk override.
/// - **`osDiskSku`**: the OS disk's storage class target `sku` in the zone.
/// - **`subnetId` / `acceleratedNetworking`**: the single NIC's network
///   class target in the zone.
/// - **`trustedLaunch`**: the class's `tpmEnabled` (ADR-0165).
///
/// # Errors
/// [`InfraBuildError::Unsupported`] for an install-media image (ADR-0162
/// Decision 7), data disks, more than one NIC, a per-VM vCPU, memory or
/// static address override; [`InfraBuildError::UnresolvedClass`] for a class
/// with no disk or no NIC; [`InfraBuildError::MissingZone`];
/// [`InfraBuildError::UnmappedInstanceSize`];
/// [`InfraBuildError::MissingImageSource`] /
/// [`InfraBuildError::InvalidImageReference`] for the image, and
/// [`InfraBuildError::MissingResolvedImageRef`] for a `Url` image not yet
/// published into this `Provider` for its current build;
/// [`InfraBuildError::MissingTarget`] for a class with no `sku` or
/// `subnetId`; and [`InfraBuildError::InvalidSpec`] when the result fails
/// [`AzureVirtualMachineSpec::validate`].
pub fn build_azure_virtual_machine(
    vm: &VirtualMachine,
    class: &VMClass,
    image: &VMImage,
    decision: &Decision,
    provider: &Provider,
    rendered_user_data: Option<&str>,
) -> Result<AzureVirtualMachine, InfraBuildError> {
    let unsupported = |what: String| InfraBuildError::Unsupported {
        backend: PROVIDER_CLASS_AZURE,
        what,
    };

    // ADR-0162 Decision 7: a cloud boots a published image; there is no
    // install-media path for a per-VM install.
    let mode = install_mode(image);
    if mode != InstallMode::Immediate {
        return Err(unsupported(format!(
            "installMode {mode:?} (no install-media path on a cloud, ADR-0162 Decision 7)"
        )));
    }

    let hardware_override = vm.spec.hardware_override.as_ref();
    if hardware_override.is_some_and(|h| h.cpus.is_some() || h.memory_mi_b.is_some()) {
        return Err(unsupported(
            "hardwareOverride cpus or memoryMiB (the instance size fixes them, ADR-0160)".into(),
        ));
    }
    if !vm.spec.network_overrides.is_empty() {
        return Err(unsupported(
            "static address overrides (the NIC's private address is assigned by Azure)".into(),
        ));
    }

    let disks = &class.spec.hardware.disks;
    let os_disk = disks
        .first()
        .ok_or_else(|| InfraBuildError::UnresolvedClass("(no disks)".into()))?;
    if disks.len() > 1 {
        return Err(unsupported(format!(
            "data disks (class {} declares {})",
            class.name_any(),
            disks.len()
        )));
    }
    let interfaces = &class.spec.network.interfaces;
    let nic = interfaces
        .first()
        .ok_or_else(|| InfraBuildError::UnresolvedClass("(no network interfaces)".into()))?;
    if interfaces.len() > AZURE_MAX_NICS {
        return Err(unsupported(format!(
            "more than {AZURE_MAX_NICS} network interface (class {} declares {})",
            class.name_any(),
            interfaces.len()
        )));
    }

    let zone = decision
        .failure_domain_raw
        .get(FD_ZONE_KEY)
        .or_else(|| decision.failure_domain_labels.get(FD_ZONE_KEY))
        .cloned()
        .ok_or_else(|| InfraBuildError::MissingZone(decision.failure_domain_name.clone()))?;

    let capabilities = &provider.spec.capabilities;
    let size = capabilities
        .instance_size_for(&class.name_any(), &zone)
        .ok_or_else(|| InfraBuildError::UnmappedInstanceSize {
            vm_class: class.name_any(),
            provider: decision.provider_name.clone(),
            zone: zone.clone(),
        })?
        .to_string();

    let image_version_id = azure_image_version_id(image, decision)?;

    let os_disk_sku = capabilities
        .storage_classes
        .iter()
        .find(|c| c.name == os_disk.storage_class)
        .and_then(|c| c.target_for_zone(&zone))
        .and_then(|t| t.get(TARGET_KEY_AZURE_SKU))
        .cloned()
        .ok_or_else(|| InfraBuildError::MissingTarget {
            what: "storage",
            class: os_disk.storage_class.clone(),
            key: TARGET_KEY_AZURE_SKU,
            zone: zone.clone(),
        })?;

    let network_target = capabilities
        .network_classes
        .iter()
        .find(|c| c.name == nic.network_class)
        .and_then(|c| c.target_for_zone(&zone));
    let subnet_id = network_target
        .and_then(|t| t.get(TARGET_KEY_AZURE_SUBNET_ID))
        .cloned()
        .ok_or_else(|| InfraBuildError::MissingTarget {
            what: "network",
            class: nic.network_class.clone(),
            key: TARGET_KEY_AZURE_SUBNET_ID,
            zone: zone.clone(),
        })?;
    let accelerated_networking = network_target
        .and_then(|t| t.get(TARGET_KEY_AZURE_ACCELERATED_NETWORKING))
        .is_some_and(|v| v == TARGET_VALUE_TRUE);

    let spec = AzureVirtualMachineSpec {
        provider_id: None,
        failure_domain: Some(decision.failure_domain_name.clone()),
        provider_ref: provider_reference(vm, provider, &decision.provider_name),
        zone,
        size,
        image_version_id,
        os_disk_size_gi_b: merge_disk_size_override(os_disk, hardware_override),
        os_disk_sku,
        subnet_id,
        accelerated_networking,
        trusted_launch: class.spec.tpm_enabled,
        user_data: rendered_user_data.map(str::to_string),
        desired_power_state: vm.spec.desired_power_state.clone(),
    };
    spec.validate().map_err(InfraBuildError::InvalidSpec)?;

    Ok(AzureVirtualMachine {
        metadata: infra_meta(vm),
        spec,
        status: None,
    })
}

/// The Compute Gallery image version an `azure` VM boots (ADR-0162
/// Decision 4):
///
/// - a `Template` source: its `ref`, an image version an admin published;
/// - a `Url` source: the version banlieue published into the chosen
///   `Provider`, which is that `Provider`'s ready `status.perProvider` row's
///   `resolvedRef`. When the build reports a checksum, the row's
///   `sourceDigest` must match it (Decision 6): a row still naming an
///   earlier build's version is not booted.
///
/// Either way the result must be an ARM resource ID.
fn azure_image_version_id(image: &VMImage, decision: &Decision) -> Result<String, InfraBuildError> {
    let source = image
        .spec
        .sources
        .iter()
        .find(|s| {
            s.provider_class == PROVIDER_CLASS_AZURE
                && matches!(s.kind, ImageSourceKind::Template | ImageSourceKind::Url)
        })
        .ok_or_else(|| InfraBuildError::MissingImageSource {
            image: image.name_any(),
            provider_class: PROVIDER_CLASS_AZURE,
            kind: "Template or Url",
        })?;
    let reference = match source.kind {
        ImageSourceKind::Url => published_image_ref(image, decision)?,
        _ => source.reference.clone(),
    };
    if !reference.starts_with(ARM_RESOURCE_ID_PREFIX) {
        return Err(InfraBuildError::InvalidImageReference {
            image: image.name_any(),
            reference,
            expected: "a Compute Gallery image version resource ID (/subscriptions/...)",
        });
    }
    Ok(reference)
}

/// What banlieue published for `image` into the decision's `Provider` (a
/// Compute Gallery image version, an AMI): its ready row's `resolvedRef`,
/// for the current build.
fn published_image_ref(image: &VMImage, decision: &Decision) -> Result<String, InfraBuildError> {
    let missing = || InfraBuildError::MissingResolvedImageRef {
        image: image.name_any(),
        provider: decision.provider_name.clone(),
        zone: decision.failure_domain_name.clone(),
    };
    let status = image.status.as_ref().ok_or_else(missing)?;
    let row = status
        .per_provider
        .iter()
        .find(|r| {
            r.provider_name == decision.provider_name
                && r.provider_namespace == decision.provider_namespace
        })
        .filter(|r| r.ready)
        .ok_or_else(missing)?;
    let built = status
        .build_artifact
        .as_ref()
        .and_then(|a| a.checksum.as_deref());
    if let Some(built) = built
        && !row
            .source_digest
            .as_deref()
            .is_some_and(|d| d.eq_ignore_ascii_case(built))
    {
        return Err(missing());
    }
    row.resolved_ref.clone().ok_or_else(missing)
}

/// Target key naming an EBS volume type (ADR-0160 Decision 4).
const TARGET_KEY_EC2_VOLUME_TYPE: &str = "volumeType";
/// Target key naming provisioned IOPS for the root volume.
const TARGET_KEY_EC2_IOPS: &str = "iops";
/// Target key naming gp3 throughput, MiB/s, for the root volume.
const TARGET_KEY_EC2_THROUGHPUT: &str = "throughput";
/// Target key naming the subnet ID of the primary network interface.
const TARGET_KEY_EC2_SUBNET_ID: &str = "subnetId";
/// Target key naming the comma-separated security group IDs.
const TARGET_KEY_EC2_SECURITY_GROUP_IDS: &str = "securityGroupIds";
/// Separator of a list-valued target.
const TARGET_LIST_SEPARATOR: char = ',';
/// NICs an EC2 instance takes in v1 (ADR-0174).
const EC2_MAX_NICS: usize = 1;
/// Every EBS volume type an `EC2Machine` creates.
const EC2_VOLUME_TYPES: [Ec2VolumeType; 4] = [
    Ec2VolumeType::Gp3,
    Ec2VolumeType::Gp2,
    Ec2VolumeType::Io1,
    Ec2VolumeType::Io2,
];

/// Build an [`EC2Machine`] from the scheduler [`Decision`], the original VM,
/// its class, image, and the chosen [`Provider`] (ADR-0174).
///
/// Owner-reference is set to `vm`, as for the other backends. Every field is
/// resolved here so the provider interprets nothing.
///
/// # Where each resolved field comes from
///
/// - **`zone`**: the failure domain's raw `zone` attribute, else its `zone`
///   label (ADR-0158 Decision 3): the account's zone **name**.
/// - **`instanceType`**: `Provider.spec.capabilities.instanceSizes` for the
///   `VMClass` in that zone, a per-zone entry winning (ADR-0160 Decision 1).
/// - **`imageId`**: the `ref` of the `VMImage`'s `ec2` `Template` source (a
///   hand-published AMI), or for a `Url` source the AMI banlieue published
///   into this `Provider` for the current build (its ready row's
///   `resolvedRef`, ADR-0162 Decision 9).
/// - **`rootDeviceName`**: the AMI's own, as the EC2 provider recorded it
///   from `DescribeImages` on the image's row for this `Provider`; else
///   `/dev/xvda`, the name banlieue registers AMIs with.
/// - **`rootVolumeGiB`**: the class's OS disk, after the VM's disk override.
/// - **`volumeType` / `iops` / `throughput`**: the OS disk's storage class
///   target in the zone (`volumeType` required, the other two optional).
/// - **`kmsKeyId`**: the `Provider`'s `spec.connection.ec2.kmsKeyId`.
/// - **`subnetId` / `securityGroupIds`**: the single NIC's network class
///   target in the zone (`securityGroupIds` comma-separated).
/// - **`tpmEnabled`**: the class's `tpmEnabled` (NitroTPM, ADR-0167).
///
/// # Errors
/// [`InfraBuildError::Unsupported`] for an install-media image (ADR-0162
/// Decision 7), a `trustedBoot` image or `efi-secure` firmware (ADR-0167
/// Decision 5), data disks, more than one NIC, a per-VM vCPU, memory or
/// static address override; [`InfraBuildError::UnresolvedClass`] for a class
/// with no disk or no NIC; [`InfraBuildError::MissingZone`];
/// [`InfraBuildError::UnmappedInstanceSize`];
/// [`InfraBuildError::MissingImageSource`] /
/// [`InfraBuildError::InvalidImageReference`] for the image, and
/// [`InfraBuildError::MissingResolvedImageRef`] for a `Url` image not yet
/// published into this `Provider` for its current build;
/// [`InfraBuildError::MissingTarget`] / [`InfraBuildError::InvalidTarget`]
/// for a storage or network target; and [`InfraBuildError::InvalidSpec`]
/// when the result fails [`EC2MachineSpec::validate`] (an identifier of the
/// wrong shape, user data over EC2's 16,384 bytes).
pub fn build_ec2_machine(
    vm: &VirtualMachine,
    class: &VMClass,
    image: &VMImage,
    decision: &Decision,
    provider: &Provider,
    rendered_user_data: Option<&str>,
) -> Result<EC2Machine, InfraBuildError> {
    let unsupported = |what: String| InfraBuildError::Unsupported {
        backend: PROVIDER_CLASS_EC2,
        what,
    };

    // ADR-0162 Decision 7: a cloud boots a published image.
    let mode = install_mode(image);
    if mode != InstallMode::Immediate {
        return Err(unsupported(format!(
            "installMode {mode:?} (no install-media path on a cloud, ADR-0162 Decision 7)"
        )));
    }
    // ADR-0167 Decision 5: UEFI Secure Boot needs the AMI's own UEFI variable
    // store, which no banlieue publisher writes.
    if image.spec.trusted_boot.is_some() {
        return Err(unsupported(
            "trustedBoot (UEFI Secure Boot needs the AMI's UEFI variable store, ADR-0167 Decision 5)"
                .into(),
        ));
    }
    if class.spec.firmware == banlieue_api::common::Firmware::EfiSecure {
        return Err(unsupported(
            "firmware efi-secure (UEFI Secure Boot needs the AMI's UEFI variable store, ADR-0167 Decision 5)"
                .into(),
        ));
    }

    let hardware_override = vm.spec.hardware_override.as_ref();
    if hardware_override.is_some_and(|h| h.cpus.is_some() || h.memory_mi_b.is_some()) {
        return Err(unsupported(
            "hardwareOverride cpus or memoryMiB (the instance type fixes them, ADR-0160)".into(),
        ));
    }
    if !vm.spec.network_overrides.is_empty() {
        return Err(unsupported(
            "static address overrides (the primary private address is assigned by EC2)".into(),
        ));
    }

    let disks = &class.spec.hardware.disks;
    let os_disk = disks
        .first()
        .ok_or_else(|| InfraBuildError::UnresolvedClass("(no disks)".into()))?;
    if disks.len() > 1 {
        return Err(unsupported(format!(
            "data disks (class {} declares {})",
            class.name_any(),
            disks.len()
        )));
    }
    let interfaces = &class.spec.network.interfaces;
    let nic = interfaces
        .first()
        .ok_or_else(|| InfraBuildError::UnresolvedClass("(no network interfaces)".into()))?;
    if interfaces.len() > EC2_MAX_NICS {
        return Err(unsupported(format!(
            "more than {EC2_MAX_NICS} network interface (class {} declares {})",
            class.name_any(),
            interfaces.len()
        )));
    }

    let zone = decision
        .failure_domain_raw
        .get(FD_ZONE_KEY)
        .or_else(|| decision.failure_domain_labels.get(FD_ZONE_KEY))
        .cloned()
        .ok_or_else(|| InfraBuildError::MissingZone(decision.failure_domain_name.clone()))?;

    let capabilities = &provider.spec.capabilities;
    let instance_type = capabilities
        .instance_size_for(&class.name_any(), &zone)
        .ok_or_else(|| InfraBuildError::UnmappedInstanceSize {
            vm_class: class.name_any(),
            provider: decision.provider_name.clone(),
            zone: zone.clone(),
        })?
        .to_string();

    let image_id = ec2_image_id(image, decision)?;

    let storage_target = capabilities
        .storage_classes
        .iter()
        .find(|c| c.name == os_disk.storage_class)
        .and_then(|c| c.target_for_zone(&zone));
    let volume = ec2_volume(storage_target, &os_disk.storage_class, &zone)?;

    let network_target = capabilities
        .network_classes
        .iter()
        .find(|c| c.name == nic.network_class)
        .and_then(|c| c.target_for_zone(&zone));
    let missing_network = |key: &'static str| InfraBuildError::MissingTarget {
        what: "network",
        class: nic.network_class.clone(),
        key,
        zone: zone.clone(),
    };
    let subnet_id = network_target
        .and_then(|t| t.get(TARGET_KEY_EC2_SUBNET_ID))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| missing_network(TARGET_KEY_EC2_SUBNET_ID))?;
    let security_group_ids: Vec<String> = network_target
        .and_then(|t| t.get(TARGET_KEY_EC2_SECURITY_GROUP_IDS))
        .map(|v| split_list(v))
        .unwrap_or_default();
    if security_group_ids.is_empty() {
        return Err(missing_network(TARGET_KEY_EC2_SECURITY_GROUP_IDS));
    }

    let kms_key_id = provider
        .spec
        .connection
        .ec2
        .as_ref()
        .and_then(|c| c.kms_key_id.clone());

    let spec = EC2MachineSpec {
        provider_id: None,
        failure_domain: Some(decision.failure_domain_name.clone()),
        provider_ref: provider_reference(vm, provider, &decision.provider_name),
        zone,
        instance_type,
        image_id,
        root_device_name: ec2_root_device_name(image, decision),
        root_volume_gi_b: merge_disk_size_override(os_disk, hardware_override),
        volume_type: volume.volume_type,
        iops: volume.iops,
        throughput: volume.throughput,
        kms_key_id,
        subnet_id,
        security_group_ids,
        tpm_enabled: class.spec.tpm_enabled,
        user_data: rendered_user_data.map(str::to_string),
        desired_power_state: vm.spec.desired_power_state.clone(),
    };
    spec.validate().map_err(InfraBuildError::InvalidSpec)?;

    Ok(EC2Machine {
        metadata: infra_meta(vm),
        spec,
        status: None,
    })
}

/// The root device name of the image's AMI on the decision's `Provider`, as
/// recorded by `DescribeImages`, else [`DEFAULT_ROOT_DEVICE_NAME`].
fn ec2_root_device_name(image: &VMImage, decision: &Decision) -> String {
    image
        .status
        .as_ref()
        .and_then(|s| {
            s.per_provider.iter().find(|r| {
                r.provider_name == decision.provider_name
                    && r.provider_namespace == decision.provider_namespace
            })
        })
        .and_then(|r| r.ec2.as_ref())
        .map_or_else(
            || DEFAULT_ROOT_DEVICE_NAME.to_string(),
            |a| a.root_device_name.clone(),
        )
}

/// The root volume settings a storage class target resolves to.
struct Ec2Volume {
    volume_type: Ec2VolumeType,
    iops: Option<u32>,
    throughput: Option<u32>,
}

/// The EBS settings of `class`'s `target` in `zone`: `volumeType` required,
/// `iops` and `throughput` optional, each parsed.
fn ec2_volume(
    target: Option<&BTreeMap<String, String>>,
    class: &str,
    zone: &str,
) -> Result<Ec2Volume, InfraBuildError> {
    let raw_type = target
        .and_then(|t| t.get(TARGET_KEY_EC2_VOLUME_TYPE))
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| InfraBuildError::MissingTarget {
            what: "storage",
            class: class.to_string(),
            key: TARGET_KEY_EC2_VOLUME_TYPE,
            zone: zone.to_string(),
        })?;
    let invalid =
        |key: &'static str, value: &str, expected: &'static str| InfraBuildError::InvalidTarget {
            what: "storage",
            class: class.to_string(),
            key,
            value: value.to_string(),
            zone: zone.to_string(),
            expected,
        };
    let volume_type = EC2_VOLUME_TYPES
        .into_iter()
        .find(|t| t.as_str() == raw_type)
        .ok_or_else(|| invalid(TARGET_KEY_EC2_VOLUME_TYPE, raw_type, "gp3, gp2, io1 or io2"))?;
    let number = |key: &'static str| -> Result<Option<u32>, InfraBuildError> {
        let Some(value) = target.and_then(|t| t.get(key)) else {
            return Ok(None);
        };
        value
            .trim()
            .parse::<u32>()
            .map(Some)
            .map_err(|_| invalid(key, value, "a whole number"))
    };
    Ok(Ec2Volume {
        volume_type,
        iops: number(TARGET_KEY_EC2_IOPS)?,
        throughput: number(TARGET_KEY_EC2_THROUGHPUT)?,
    })
}

/// The non-empty, trimmed entries of a comma-separated target value.
fn split_list(value: &str) -> Vec<String> {
    value
        .split(TARGET_LIST_SEPARATOR)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// The AMI an `ec2` VM boots:
///
/// - a `Template` source: its `ref`, an AMI an admin published by hand;
/// - a `Url` source: the AMI banlieue published into the chosen `Provider`
///   for the current build (roadmap 24 phase D writes that row).
///
/// Either way the result must be an AMI ID.
fn ec2_image_id(image: &VMImage, decision: &Decision) -> Result<String, InfraBuildError> {
    let source = image
        .spec
        .sources
        .iter()
        .find(|s| {
            s.provider_class == PROVIDER_CLASS_EC2
                && matches!(s.kind, ImageSourceKind::Template | ImageSourceKind::Url)
        })
        .ok_or_else(|| InfraBuildError::MissingImageSource {
            image: image.name_any(),
            provider_class: PROVIDER_CLASS_EC2,
            kind: "Template or Url",
        })?;
    let reference = match source.kind {
        ImageSourceKind::Url => published_image_ref(image, decision)?,
        _ => source.reference.trim().to_string(),
    };
    if !aws::is_ami_id(&reference) {
        return Err(InfraBuildError::InvalidImageReference {
            image: image.name_any(),
            reference,
            expected: "an AMI ID (ami- and 8 or 17 hex digits)",
        });
    }
    Ok(reference)
}

/// The storage id the NoCloud seed ISO is uploaded to: the `storage` key of
/// the Provider's [`PROXMOX_SEED_STORAGE_CLASS`] class.
fn seed_storage(provider: &Provider) -> Result<String, InfraBuildError> {
    provider
        .spec
        .capabilities
        .storage_classes
        .iter()
        .find(|c| c.name == PROXMOX_SEED_STORAGE_CLASS)
        .and_then(|c| c.target_for("", ""))
        .and_then(|t| t.get(TARGET_KEY_PROXMOX_STORAGE))
        .cloned()
        .ok_or_else(|| InfraBuildError::UnresolvedClass(PROXMOX_SEED_STORAGE_CLASS.into()))
}

/// Which provisioning shape an image's install mode calls for on Cloud
/// Hypervisor, the counterpart of [`boot_source_kind`] (ADR-0040, ADR-0065).
fn ch_boot_source_kind(image: &VMImage) -> ChBootSourceKind {
    match install_mode(image) {
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
fn nic_source_kind(mapping: Option<&NetworkClassMapping>) -> LibvirtNicSourceKind {
    let target = mapping.and_then(|m| m.target_for("", ""));
    match target {
        Some(t) if t.contains_key(TARGET_KEY_LIBVIRT_BRIDGE) => LibvirtNicSourceKind::Bridge,
        _ => LibvirtNicSourceKind::Network,
    }
}

/// Look up the resolved template reference for the chosen provider and
/// zone: its bare display name, plus the per-zone folder it lives in when
/// there is one.
///
/// vSphere images (`Url` and, since ADR-0104, `Template`) have per-zone
/// `resolved_ref` / `template_folder` entries, one per failure domain.
/// libvirt and Proxmox rows have a single top-level `resolved_ref` and no
/// per-zone folder (`None`). The zone entry matching `failure_domain_name` is
/// preferred; the top-level ref is the fallback.
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

        // Per-zone entry first (vSphere `Url` and `Template`, ADR-0104):
        // bare resolved_ref + the zone's template_folder.
        if let Some(zone) = row.zones.iter().find(|z| z.name == failure_domain_name) {
            let name = zone.resolved_ref.clone()?;
            return Some((name, zone.template_folder.clone()));
        }

        // No zone entry: the provider's single top-level ref (libvirt volume,
        // Proxmox VMID, or a legacy vSphere row), with no per-zone folder.
        row.resolved_ref.clone().map(|r| (r, None))
    })
}

/// Strip the `[<datacenter(s)>] ` display prefix that vSphere providers
/// before ADR-0104 wrote into a `Template`-kind image's top-level
/// `resolvedRef`. `VSphereMachine.spec.template` is matched by exact name, so
/// it must only ever carry the bare name. A ref without the prefix is
/// returned unchanged.
fn bare_template_name(resolved_ref: &str) -> &str {
    resolved_ref
        .strip_prefix('[')
        .and_then(|rest| rest.split_once("] "))
        .map_or(resolved_ref, |(_, name)| name)
}

#[cfg(test)]
#[path = "infra_tests.rs"]
mod infra_tests;
