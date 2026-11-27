// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Scheduler — the pure placement function.
//!
//! Given a [`VirtualMachine`], its [`VMClass`] / [`VMImage`], the set of
//! [`Provider`]s in the namespace, and the existing scheduled
//! [`VirtualMachine`]s (for anti-affinity), choose a `(provider, failure
//! domain)` pair and return a [`Decision`].
//!
//! This module is **deliberately a pure function** — no kube client, no I/O,
//! no async. That makes scheduling reproducible, table-test-friendly, and
//! easy to reason about. The reconciler is responsible for fetching the
//! inputs and applying the decision.
//!
//! ## Filter chain (in order)
//!
//! 1. Provider selector — `placement.providerSelector` against
//!    `Provider.metadata.labels`. Then a Provider labelled
//!    `banlieue.io/unschedulable: "true"` (a cordoned host) is dropped
//!    (ADR-0124). Then liveness (ADR-0102): a Provider whose
//!    `status.workload.heartbeat` is `Stale` is dropped; `Absent` and an
//!    unevaluated heartbeat are kept.
//! 2. Failure-domain selector — `placement.failureDomainSelector` against
//!    `FailureDomain.labels`.
//! 3. Image readiness — `VMImage.status.perProvider[i].ready == true` for
//!    the candidate Provider. A `Template`-kind source that reports
//!    `zones[]` is additionally gated per failure domain: the zone's own
//!    entry must be ready (ADR-0104).
//!    3c. Instance size (ADR-0160 Decision 1): a Provider that maps classes
//!    to instance sizes (any that declares `capabilities.instanceSizes`,
//!    and every cloud class such as `azure` even when it declares none)
//!    takes a VM only when its `VMClass` is mapped in the failure domain's
//!    zone. Nothing is guessed from vCPU and memory.
//!    3d. ec2 (roadmap 24 phase E): on an `ec2` Provider, from what the
//!    provider read from `DescribeInstanceTypes` and published on the
//!    failure domain, a new placement is refused when the image sets
//!    `trustedBoot` (ADR-0167 Decision 5), the class's type is not offered
//!    in the zone (`vmClasses`), it does not run the AMI's architecture
//!    (`amd64VmClasses` / `arm64VmClasses`; the architecture `DescribeImages`
//!    recorded on the image's row for the Provider, else the `VMImage`'s),
//!    or the class sets `tpmEnabled` and the AMI was recorded without
//!    `TpmSupport=v2.0` and `BootMode=uefi` or the type lacks NitroTPM
//!    (`tpmVmClasses`). A list the provider did not publish is unknown and
//!    filters nothing, except `tpmVmClasses`: a TPM is never assumed. On ec2
//!    this replaces step 8's `vtpm` feature.
//! 4. Storage classes — every disk's `storageClass` must appear in
//!    `FailureDomain.attributes.availableStorageClasses`.
//! 5. Network classes — every NIC's `networkClass` must appear in
//!    `FailureDomain.attributes.availableNetworkClasses`.
//! 6. Features — every feature in `VMClass.spec.features` must appear in
//!    `FailureDomain.attributes.features`.
//! 7. Firmware — `efi-secure` requires the feature `efiSecureBoot`; `bios`
//!    and `efi` are assumed universally available (vSphere ≥ 6.5).
//! 8. vTPM (ADR-0039) — `VMClass.spec.tpmEnabled` requires the feature
//!    `vtpm`; unset is universally acceptable (no vTPM is requested).
//! 9. Anti-affinity (`required`) — drop candidates that would put this VM on
//!    a failure domain sharing a `topologyKey` value with an already-
//!    scheduled VM matching the rule's label selector. The VM itself is
//!    never one of them (ADR-0125).
//!
//! ## A placed VM keeps a placement that still fits (ADR-0125)
//!
//! The reconciler re-runs this function every pass and treats a different
//! answer as drift, which the default `migrationPolicy` answers by
//! recreating the VM. So for a VM with `status.scheduled`, when its recorded
//! `(provider, failure domain)` survives every filter above, that is the
//! answer: the spread below never moves it to an emptier domain. Image
//! readiness (steps 3 and 3b) is not checked for it: the image decides where
//! a new disk can be made, and the VM already has its disk.
//!
//! ## Spread (ADR-0098)
//!
//! The survivors of a new placement (or of a placed VM whose placement no
//! longer fits) are ordered, not just tie-broken, so placement spreads
//! evenly across failure domains (D-023):
//!
//! 1. fewest already-scheduled VMs of the VM's spread group on the domain
//!    (the other members of its pool, or every VM when it is in none);
//! 2. then fewest scheduled VMs overall;
//! 3. then the highest rendezvous hash of `(vm, provider, failure domain)`,
//!    so members scheduled concurrently, before they can see each other's
//!    placement, still spread instead of following one fixed order.
//!
//! Load comes from `status.scheduled` of the VMs passed in; a VM never
//! counts against itself. Still a pure, deterministic function.
//!
//! ## Capacity (ADR-0107, ADR-0128)
//!
//! After the vTPM filter, a failure domain that publishes `capacity` must
//! hold the VM. ADR-0107's reservation check counts the classes of the VMs
//! scheduled there: memory strictly, vCPUs up to the Provider's overcommit
//! ratio. ADR-0128 adds the measured figures, for a new placement only (a
//! placed VM is already inside them): `memoryAvailableMiB`, and each storage
//! class's `freeGiB` less `storageReservePercent` of its `totalGiB`, both less
//! what in-flight VMs (scheduled, infrastructure not yet provisioned) will
//! take. A store marked `shared` counts in-flight disks across all the
//! Provider's failure domains. A figure that is not published is not checked.

use std::collections::{BTreeMap, HashMap};

use banlieue_api::banlieue::{
    AffinityMode, AntiAffinityRule, Architecture, DEFAULT_CPU_OVERCOMMIT_RATIO,
    DEFAULT_STORAGE_RESERVE_PERCENT, Ec2ImageAttributes, FailureDomain, FailureDomainCapacity,
    Heartbeat, ImageSourceKind, LABEL_POOL, LABEL_UNSCHEDULABLE, NetworkClassMapping, Provider,
    ProviderCapabilities, ProviderScheduling, ResolvedResource, StorageClassMapping, VMClass,
    VMImage, VirtualMachine,
};
use banlieue_api::common::{Firmware, LabelSelector, LabelSelectorOperator};

use kube::ResourceExt;
use tracing::warn;

use super::infra::{
    CLOUD_PROVIDER_CLASSES, FD_ZONE_KEY, PROVIDER_CLASS_EC2, effective_cpus, effective_memory_mi_b,
    merge_disk_size_override,
};

/// Feature flag a failure domain must advertise to support EFI secure boot.
pub const FEATURE_EFI_SECURE_BOOT: &str = "efiSecureBoot";

/// Feature flag a failure domain must advertise to support attaching a
/// vTPM device (`VMClass.spec.tpmEnabled`, ADR-0039). Set by hand on
/// `Provider.spec.capabilities.features` once the operator confirms KMS +
/// vTPM actually work end-to-end in that vCenter — never auto-discovered.
pub const FEATURE_VTPM: &str = "vtpm";

/// Failure-domain raw key an `ec2` provider publishes: the comma-separated
/// `VMClass`es whose mapped instance type is offered in the zone.
pub const FD_RAW_EC2_VM_CLASSES: &str = "vmClasses";
/// Raw key: those whose type supports NitroTPM 2.0 and UEFI boot
/// (ADR-0167 Decision 5).
pub const FD_RAW_EC2_TPM_VM_CLASSES: &str = "tpmVmClasses";
/// Raw key: those whose type runs `x86_64` (an `amd64` image).
pub const FD_RAW_EC2_AMD64_VM_CLASSES: &str = "amd64VmClasses";
/// Raw key: those whose type runs `arm64` (an `arm64` image).
pub const FD_RAW_EC2_ARM64_VM_CLASSES: &str = "arm64VmClasses";
/// Separator of a list-valued raw attribute.
const RAW_LIST_SEPARATOR: char = ',';

/// Divisor turning `storageReservePercent` into a share of a store.
const PERCENT: u64 = 100;

/// Maximum number of per-candidate reject reasons kept for the
/// `NoFailureDomainMatched` condition message. At admin-scale topology the
/// full list can exceed etcd's status-size limit, getting the status patch
/// rejected and wedging the VM in a requeue loop (SEC-015). Ten reasons
/// (~1 KB) is enough to debug placement while staying far under the limit;
/// the remainder is summarized as "… and M more".
const MAX_REJECT_REASONS: usize = 10;

/// The output of a successful scheduling pass.
///
/// All fields are owned (no lifetimes) so the [`Decision`] can be cached,
/// compared, or fed to a builder without keeping the input slices alive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// Name of the chosen [`Provider`].
    pub provider_name: String,
    /// Namespace of the chosen [`Provider`].
    pub provider_namespace: String,
    /// `Provider.spec.providerClassRef.name` — denormalized for printer columns.
    pub provider_class: String,
    /// Chosen `FailureDomain.name`.
    pub failure_domain_name: String,
    /// Per-disk class → backend_id resolution. Same order as `VMClass.spec.hardware.disks`.
    pub resolved_storage: Vec<ResolvedResource>,
    /// Per-NIC class → backend_id resolution. Same order as `VMClass.spec.network.interfaces`.
    pub resolved_networks: Vec<ResolvedResource>,
    /// Raw failure-domain attributes (e.g. vSphere `datacenter`, `cluster`).
    /// Opaque to the scheduler; consumed by provider-specific infra builders.
    pub failure_domain_raw: BTreeMap<String, String>,
    /// Failure-domain labels — used downstream for anti-affinity comparisons
    /// against future scheduling passes.
    pub failure_domain_labels: BTreeMap<String, String>,
}

/// Why a scheduling attempt failed. Each variant maps to a stable condition
/// reason on the `VirtualMachine`'s `Scheduled` condition.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ScheduleError {
    /// No provider passed the `providerSelector` filter.
    #[error("no providers matched providerSelector")]
    NoProviderMatched,

    /// No `(provider, failure domain)` pair survived the full filter chain.
    /// The accompanying string lists per-candidate reject reasons for
    /// observability.
    #[error("no failure domain matched all constraints: {0}")]
    NoFailureDomainMatched(String),

    /// Image `VMImage.status.perProvider[provider].ready` was `false` (or
    /// the entry was absent) for every otherwise-viable candidate.
    #[error("image not ready on any candidate provider")]
    ImageNotReady,

    /// Required `efiSecureBoot` feature is not advertised by any candidate
    /// failure domain.
    #[error(
        "efi-secure firmware requires the efiSecureBoot feature; none of the candidate failure domains advertise it"
    )]
    FirmwareUnsupported,

    /// Required `vtpm` feature is not advertised by any candidate failure
    /// domain, but `VMClass.spec.tpmEnabled` is `true` (ADR-0039).
    #[error(
        "tpmEnabled requires the vtpm feature; none of the candidate failure domains advertise it"
    )]
    TpmUnsupported,

    /// Required anti-affinity rule could not be satisfied.
    #[error("required anti-affinity rule on topologyKey={0} left no candidates")]
    AntiAffinityUnsatisfied(String),

    /// Every Provider the selector matched has a `Stale` heartbeat: they
    /// stopped renewing their Leases, so nothing would reconcile the VM
    /// (ADR-0102).
    #[error("every matching provider has stopped renewing its Lease")]
    ProviderNotAlive,

    /// The VM could only go to Providers labelled `banlieue.io/unschedulable:
    /// "true"` (ADR-0124): every Provider the selector matched is cordoned, or
    /// the placement fails with them excluded and would succeed with them
    /// (ADR-0124 Decision 2, amended 2026-11-09). Carries the cordoned
    /// Providers' names, comma-separated.
    #[error("only cordoned providers (banlieue.io/unschedulable) could take this VM: {0}")]
    ProviderUnschedulable(String),

    /// Every candidate failure domain was rejected because its Provider maps
    /// classes to instance sizes and has no entry for this `VMClass`
    /// (ADR-0160 Decision 1). Carries the `VMClass` name.
    #[error(
        "VMClass {0} has no instance size on any candidate provider; map it in the Provider's spec.capabilities.instanceSizes (ADR-0160)"
    )]
    InstanceSizeUnmapped(String),

    /// Every candidate failure domain was rejected because it is an `ec2`
    /// zone where the class's instance type is not offered (its
    /// `vmClasses` omits the class). Carries the `VMClass` name.
    #[error(
        "VMClass {0}'s instance type is not offered in any candidate ec2 zone (the failure domains' vmClasses omit it)"
    )]
    InstanceTypeNotOffered(String),

    /// Every candidate failure domain was rejected because the class sets
    /// `tpmEnabled` and its instance type there lacks NitroTPM 2.0 with UEFI
    /// boot (ADR-0167 Decision 5). Carries the `VMClass` name.
    #[error(
        "VMClass {0} sets tpmEnabled, but its instance type supports no NitroTPM 2.0 with UEFI boot on any candidate ec2 failure domain (ADR-0167 Decision 5)"
    )]
    NitroTpmUnsupported(String),

    /// Every candidate failure domain was rejected because the class sets
    /// `tpmEnabled` and the AMI was recorded (by `DescribeImages`) without
    /// `TpmSupport=v2.0` and `BootMode=uefi` (ADR-0167 Decision 5). Carries
    /// the `VMImage` name.
    #[error(
        "VMImage {0}'s AMI is not registered with TpmSupport=v2.0 and BootMode=uefi on any candidate ec2 Provider, and the class sets tpmEnabled (ADR-0167 Decision 5)"
    )]
    AmiTpmUnsupported(String),

    /// Every candidate was an `ec2` failure domain and the image sets
    /// `trustedBoot` (ADR-0167 Decision 5).
    #[error(
        "the image sets trustedBoot, which ec2 refuses: UEFI Secure Boot needs the AMI's own UEFI variable store, which banlieue does not write (ADR-0167 Decision 5)"
    )]
    TrustedBootUnsupported,

    /// Every candidate failure domain was rejected because the class's
    /// instance type there does not run the image's architecture.
    #[error(
        "VMClass {vm_class} maps to no instance type that runs {architecture} on any candidate ec2 failure domain"
    )]
    ArchitectureMismatch {
        /// The `VMClass` name.
        vm_class: String,
        /// The image's architecture (`amd64`, `arm64`).
        architecture: String,
    },
}

/// Stable condition reasons. Mapped onto the `Scheduled` condition.
pub mod reasons {
    /// Scheduling succeeded.
    pub const SCHEDULED: &str = "Scheduled";
    /// `placement.providerSelector` matched zero Providers.
    pub const NO_PROVIDER: &str = "NoProviderMatched";
    /// No surviving failure domain (compound rejection).
    pub const NO_FAILURE_DOMAIN: &str = "NoFailureDomainMatched";
    /// Image not ready on any candidate provider.
    pub const IMAGE_NOT_READY: &str = "ImageNotReady";
    /// `efi-secure` firmware unsupported.
    pub const FIRMWARE_UNSUPPORTED: &str = "FirmwareUnsupported";
    /// `tpmEnabled` requested but no candidate advertises `vtpm`.
    pub const TPM_UNSUPPORTED: &str = "TpmUnsupported";
    /// Required anti-affinity rule could not be satisfied.
    pub const ANTI_AFFINITY_UNSATISFIED: &str = "AntiAffinityUnsatisfied";
    /// Every matching Provider's heartbeat is `Stale` (ADR-0102).
    pub const PROVIDER_NOT_ALIVE: &str = "ProviderNotAlive";
    /// Every matching Provider is labelled unschedulable (ADR-0124).
    pub const PROVIDER_UNSCHEDULABLE: &str = "ProviderUnschedulable";
    /// The `VMClass` has no instance size on any candidate Provider that
    /// maps classes to sizes (ADR-0160 Decision 1).
    pub const INSTANCE_SIZE_UNMAPPED: &str = "InstanceSizeUnmapped";
    /// The class's ec2 instance type is offered in no candidate zone.
    pub const INSTANCE_TYPE_NOT_OFFERED: &str = "InstanceTypeNotOffered";
    /// `tpmEnabled` on an ec2 instance type without NitroTPM
    /// (ADR-0167 Decision 5).
    pub const NITRO_TPM_UNSUPPORTED: &str = "NitroTpmUnsupported";
    /// `tpmEnabled` on an AMI recorded without NitroTPM support
    /// (ADR-0167 Decision 5).
    pub const AMI_TPM_UNSUPPORTED: &str = "AmiTpmUnsupported";
    /// `trustedBoot` on ec2 (ADR-0167 Decision 5).
    pub const TRUSTED_BOOT_UNSUPPORTED: &str = "TrustedBootUnsupported";
    /// The class's ec2 instance type does not run the image's architecture.
    pub const ARCHITECTURE_MISMATCH: &str = "ArchitectureMismatch";
}

impl ScheduleError {
    /// Returns the stable reason string for this error, suitable for use as
    /// a Kubernetes condition `reason` field.
    pub fn reason(&self) -> &'static str {
        match self {
            ScheduleError::NoProviderMatched => reasons::NO_PROVIDER,
            ScheduleError::NoFailureDomainMatched(_) => reasons::NO_FAILURE_DOMAIN,
            ScheduleError::ImageNotReady => reasons::IMAGE_NOT_READY,
            ScheduleError::FirmwareUnsupported => reasons::FIRMWARE_UNSUPPORTED,
            ScheduleError::TpmUnsupported => reasons::TPM_UNSUPPORTED,
            ScheduleError::AntiAffinityUnsatisfied(_) => reasons::ANTI_AFFINITY_UNSATISFIED,
            ScheduleError::ProviderNotAlive => reasons::PROVIDER_NOT_ALIVE,
            ScheduleError::ProviderUnschedulable(_) => reasons::PROVIDER_UNSCHEDULABLE,
            ScheduleError::InstanceSizeUnmapped(_) => reasons::INSTANCE_SIZE_UNMAPPED,
            ScheduleError::InstanceTypeNotOffered(_) => reasons::INSTANCE_TYPE_NOT_OFFERED,
            ScheduleError::NitroTpmUnsupported(_) => reasons::NITRO_TPM_UNSUPPORTED,
            ScheduleError::AmiTpmUnsupported(_) => reasons::AMI_TPM_UNSUPPORTED,
            ScheduleError::TrustedBootUnsupported => reasons::TRUSTED_BOOT_UNSUPPORTED,
            ScheduleError::ArchitectureMismatch { .. } => reasons::ARCHITECTURE_MISMATCH,
        }
    }
}

/// Whether `provider` is cordoned: labelled `banlieue.io/unschedulable:
/// "true"`, exactly (ADR-0124).
pub(crate) fn is_unschedulable(provider: &Provider) -> bool {
    provider
        .labels()
        .get(LABEL_UNSCHEDULABLE)
        .map(String::as_str)
        == Some("true")
}

/// Whether `provider`'s heartbeat, as the operator last judged it, is
/// `Stale`. `Absent` and an unevaluated heartbeat are not (ADR-0102).
pub(crate) fn is_stale(provider: &Provider) -> bool {
    provider
        .status
        .as_ref()
        .and_then(|s| s.workload.as_ref())
        .and_then(|w| w.heartbeat)
        == Some(Heartbeat::Stale)
}

/// Bounded collector for per-candidate reject reasons. Keeps the first
/// [`MAX_REJECT_REASONS`] entries and counts the rest, so the rendered
/// condition message stays bounded no matter how large the topology is.
#[derive(Default)]
struct RejectReasons {
    kept: Vec<String>,
    omitted: usize,
}

impl RejectReasons {
    fn push(&mut self, reason: String) {
        if self.kept.len() < MAX_REJECT_REASONS {
            self.kept.push(reason);
        } else {
            self.omitted += 1;
        }
    }

    /// How many reasons were pushed, kept or not.
    fn len(&self) -> usize {
        self.kept.len() + self.omitted
    }

    /// Render the kept reasons joined by `"; "`, with a trailing
    /// `"… and M more"` summary when reasons were dropped.
    fn render(&self) -> String {
        let mut msg = self.kept.join("; ");
        if self.omitted > 0 {
            msg.push_str(&format!("; … and {} more", self.omitted));
        }
        msg
    }
}

/// Pick a `(provider, failure domain)` placement for `vm`, or return a typed
/// reason it can't be scheduled.
///
/// Inputs are slices, not collections — the caller (the reconciler) owns the
/// underlying storage.
///
/// # Errors
/// See [`ScheduleError`] variants.
// The placement decision is its own span (ADR-0092 Decision 4). Inputs are
// counted, never recorded: the VM spec carries user-data.
#[tracing::instrument(
    name = "scheduler.schedule",
    skip_all,
    fields(providers = providers.len(), existing_vms = existing_vms.len())
)]
pub fn schedule(
    vm: &VirtualMachine,
    class: &VMClass,
    image: &VMImage,
    providers: &[Provider],
    existing_vms: &[VirtualMachine],
    classes: &[VMClass],
) -> Result<Decision, ScheduleError> {
    let err = match place(vm, class, image, providers, existing_vms, classes, true) {
        Ok(decision) => return Ok(decision),
        Err(err) => err,
    };
    // ADR-0124 Decision 2, amended: when a later step fails, ask whether the
    // cordon is what stopped the VM. A VM pinned to a cordoned host by its
    // failure-domain selector otherwise failed as `NoFailureDomainMatched`,
    // with a message naming every host but the cordoned one.
    if matches!(err, ScheduleError::ProviderUnschedulable(_)) {
        return Err(err);
    }
    let cordoned = cordoned_candidates(vm, providers);
    if cordoned.is_empty() {
        return Err(err);
    }
    match place(vm, class, image, providers, existing_vms, classes, false) {
        Ok(_) => Err(ScheduleError::ProviderUnschedulable(cordoned.join(", "))),
        Err(_) => Err(err),
    }
}

/// The Providers the VM's selector matches that the cordon drops (not its own
/// Provider, which stays a candidate: ADR-0124 Decision 3), by name, sorted.
fn cordoned_candidates(vm: &VirtualMachine, providers: &[Provider]) -> Vec<String> {
    let own = own_provider_name(vm);
    let mut names: Vec<String> = providers
        .iter()
        .filter(|p| selector_matches(&vm.spec.placement.provider_selector, p.labels()))
        .filter(|p| is_unschedulable(p) && own != Some(p.name_any().as_str()))
        .map(ResourceExt::name_any)
        .collect();
    names.sort();
    names
}

/// The Provider the VM is already placed on, if any.
fn own_provider_name(vm: &VirtualMachine) -> Option<&str> {
    vm.status
        .as_ref()
        .and_then(|s| s.scheduled.as_ref())
        .map(|s| s.provider_name.as_str())
}

/// The placement itself. `honour_cordon` is false only for [`schedule`]'s
/// question "would the cordon alone have stopped this VM?".
fn place(
    vm: &VirtualMachine,
    class: &VMClass,
    image: &VMImage,
    providers: &[Provider],
    existing_vms: &[VirtualMachine],
    classes: &[VMClass],
    honour_cordon: bool,
) -> Result<Decision, ScheduleError> {
    let placement = &vm.spec.placement;
    let classes_by_name: HashMap<String, &VMClass> =
        classes.iter().map(|c| (c.name_any(), c)).collect();

    // Step 1: providerSelector
    let provider_candidates: Vec<&Provider> = providers
        .iter()
        .filter(|p| selector_matches(&placement.provider_selector, p.labels()))
        .collect();
    if provider_candidates.is_empty() {
        return Err(ScheduleError::NoProviderMatched);
    }

    // A VM already placed keeps its own Provider as a candidate through the
    // cordon and liveness filters below: the reconciler re-runs this function
    // every pass, and dropping the VM's own Provider would be drift, which the
    // default migrationPolicy answers by recreating the VM elsewhere (ADR-0124
    // decision 3, ADR-0102). Both filters are about new placements.
    let own_provider = own_provider_name(vm);
    let is_own = |p: &Provider| own_provider == Some(p.name_any().as_str());

    // ADR-0125: the exact placement the VM holds, if any.
    let current = vm
        .status
        .as_ref()
        .and_then(|s| s.scheduled.as_ref())
        .map(|s| (s.provider_name.as_str(), s.failure_domain.as_str()));
    let is_current = |p: &Provider, fd: &FailureDomain| {
        current.is_some_and(|(name, domain)| name == p.name_any() && domain == fd.name)
    };

    // ADR-0124: a cordoned provider takes no new VM.
    let provider_candidates: Vec<&Provider> = provider_candidates
        .into_iter()
        .filter(|p| !honour_cordon || is_own(p) || !is_unschedulable(p))
        .collect();
    if provider_candidates.is_empty() {
        return Err(ScheduleError::ProviderUnschedulable(
            cordoned_candidates(vm, providers).join(", "),
        ));
    }

    // ADR-0102: a provider that stopped renewing its Lease takes no new VM.
    let provider_candidates: Vec<&Provider> = provider_candidates
        .into_iter()
        .filter(|p| is_own(p) || !is_stale(p))
        .collect();
    if provider_candidates.is_empty() {
        return Err(ScheduleError::ProviderNotAlive);
    }

    // A vTPM VM whose image seals on first boot (ADR-0088) is placed on any
    // backend: Cloud Hypervisor, once excluded, seals a private reflink of
    // the image with a fresh swtpm and reports over vsock (ADR-0112).

    // Walk all (provider, failure_domain) tuples.
    let mut reject_reasons = RejectReasons::default();
    let mut image_ready_seen = false;
    let mut firmware_unsupported_seen = false;
    let mut tpm_unsupported_seen = false;
    let mut size_rejects: usize = 0;
    let mut ec2_rejects = Ec2RejectCounts::default();
    let class_name = class.name_any();
    let mut survivors: Vec<(&Provider, &FailureDomain)> = Vec::new();

    for provider in &provider_candidates {
        let image_ready_for_provider = image_ready_on_provider(image, &provider.name_any());
        if image_ready_for_provider {
            image_ready_seen = true;
        }

        let fds: &[FailureDomain] = provider
            .status
            .as_ref()
            .map(|s| s.failure_domains.as_slice())
            .unwrap_or(&[]);
        for fd in fds {
            // Step 2: failureDomainSelector
            if !selector_matches(&placement.failure_domain_selector, &fd.labels) {
                reject_reasons.push(format!(
                    "{}/{}: failureDomainSelector did not match",
                    provider.name_any(),
                    fd.name
                ));
                continue;
            }

            // Image readiness is about making a new disk; the VM's own
            // placement already has one (ADR-0125 decision 2).
            let here = is_current(provider, fd);

            // Step 3: image readiness
            if !here && !image_ready_for_provider {
                reject_reasons.push(format!(
                    "{}/{}: image {} not ready on this provider",
                    provider.name_any(),
                    fd.name,
                    image.name_any(),
                ));
                continue;
            }

            // Step 3b: per-zone image readiness (Template-kind sources only)
            if !here && !image_ready_in_zone(image, provider, &fd.name) {
                reject_reasons.push(format!(
                    "{}/{}: image {} has no ready template in this zone",
                    provider.name_any(),
                    fd.name,
                    image.name_any(),
                ));
                continue;
            }

            // Step 3c: instance size (ADR-0160 Decision 1). A placed VM keeps
            // running at the size it was created with, so removing a mapping
            // stops new placements without moving it (ADR-0125).
            if !here && instance_size_unmapped(provider, &class_name, fd) {
                size_rejects += 1;
                reject_reasons.push(format!(
                    "{}/{}: VMClass '{}' has no instanceSizes entry on this provider (zone {})",
                    provider.name_any(),
                    fd.name,
                    class_name,
                    failure_domain_zone(fd).unwrap_or("unset"),
                ));
                continue;
            }

            // Step 3d: ec2 (roadmap 24 phase E). New placements only, as 3c.
            if !here
                && is_ec2(provider)
                && let Some(reject) = ec2_reject(class, image, provider, fd)
            {
                ec2_rejects.count(reject);
                if reject == Ec2Reject::Architecture {
                    ec2_rejects.note_architecture(ec2_image_architecture(image, provider));
                }
                reject_reasons.push(format!(
                    "{}/{}: {}",
                    provider.name_any(),
                    fd.name,
                    reject.describe(&class_name, image, provider),
                ));
                continue;
            }

            // Step 4: storage classes
            if let Some(missing) = first_missing(
                class.spec.hardware.disks.iter().map(|d| &d.storage_class),
                &fd.attributes.available_storage_classes,
            ) {
                reject_reasons.push(format!(
                    "{}/{}: storage class '{}' not available",
                    provider.name_any(),
                    fd.name,
                    missing
                ));
                continue;
            }

            // Step 5: network classes
            if let Some(missing) = first_missing(
                class
                    .spec
                    .network
                    .interfaces
                    .iter()
                    .map(|n| &n.network_class),
                &fd.attributes.available_network_classes,
            ) {
                reject_reasons.push(format!(
                    "{}/{}: network class '{}' not available",
                    provider.name_any(),
                    fd.name,
                    missing
                ));
                continue;
            }

            // Step 6: features
            if let Some(missing) =
                first_missing(class.spec.features.iter(), &fd.attributes.features)
            {
                reject_reasons.push(format!(
                    "{}/{}: feature '{}' not available",
                    provider.name_any(),
                    fd.name,
                    missing
                ));
                continue;
            }

            // Step 7: firmware
            if class.spec.firmware == Firmware::EfiSecure
                && !fd
                    .attributes
                    .features
                    .iter()
                    .any(|f| f == FEATURE_EFI_SECURE_BOOT)
            {
                firmware_unsupported_seen = true;
                reject_reasons.push(format!(
                    "{}/{}: efi-secure firmware requested but '{}' feature absent",
                    provider.name_any(),
                    fd.name,
                    FEATURE_EFI_SECURE_BOOT,
                ));
                continue;
            }

            // Step 8: vTPM (ADR-0039). On ec2, step 3d judged NitroTPM from
            // the instance type instead.
            if class.spec.tpm_enabled
                && !is_ec2(provider)
                && !fd.attributes.features.iter().any(|f| f == FEATURE_VTPM)
            {
                tpm_unsupported_seen = true;
                reject_reasons.push(format!(
                    "{}/{}: tpmEnabled requested but '{}' feature absent",
                    provider.name_any(),
                    fd.name,
                    FEATURE_VTPM,
                ));
                continue;
            }

            // Step 8a': quota (ADR-0161). A cloud Provider's `capacity.cpus`
            // is the vCPU quota headroom the cloud reported: VMs already
            // running are inside it, it is regional (every zone repeats the
            // same figure) and it is never overcommitted. So only VMs placed
            // on this Provider and not yet provisioned, in any of its zones,
            // are subtracted, and a VM already placed here is not re-judged.
            let quota = is_quota_capacity(provider);
            if quota
                && !here
                && let Some(capacity) = &fd.attributes.capacity
            {
                let in_provider = in_flight_on(
                    vm,
                    existing_vms,
                    &classes_by_name,
                    &provider.name_any(),
                    None,
                    0,
                );
                if let Some(short) = quota_shortfall(
                    effective_cpus(class, vm.spec.hardware_override.as_ref()),
                    &in_provider,
                    capacity,
                ) {
                    reject_reasons.push(format!(
                        "{}/{}: insufficient quota: {short}",
                        provider.name_any(),
                        fd.name,
                    ));
                    continue;
                }
            }

            // Step 8b: capacity (ADR-0107). Memory strictly, vCPUs up to the
            // Provider's overcommit ratio; a domain publishing no capacity, or
            // no figure for a resource, is not checked on it. Not for a quota
            // Provider, whose figures step 8a' already judged.
            if !quota && let Some(capacity) = &fd.attributes.capacity {
                let overhead = capacity.memory_overhead_mi_b_per_vm.unwrap_or(0);
                let used = usage_on(
                    vm,
                    existing_vms,
                    &classes_by_name,
                    &provider.name_any(),
                    &fd.name,
                    overhead,
                );
                let ratio = provider.spec.scheduling.as_ref().map_or(
                    DEFAULT_CPU_OVERCOMMIT_RATIO,
                    ProviderScheduling::cpu_overcommit_ratio,
                );
                if let Some(short) = capacity_shortfall(
                    effective_cpus(class, vm.spec.hardware_override.as_ref()),
                    u64::from(effective_memory_mi_b(
                        class,
                        vm.spec.hardware_override.as_ref(),
                    )) + overhead,
                    used,
                    capacity,
                    ratio,
                ) {
                    reject_reasons.push(format!(
                        "{}/{}: insufficient capacity: {short}",
                        provider.name_any(),
                        fd.name,
                    ));
                    continue;
                }

                // Step 8c: measured capacity (ADR-0128), new placements only:
                // the VM's own placement is already in the measurement.
                if !here {
                    let provider_name = provider.name_any();
                    let in_domain = in_flight_on(
                        vm,
                        existing_vms,
                        &classes_by_name,
                        &provider_name,
                        Some(&fd.name),
                        overhead,
                    );
                    let in_provider = in_flight_on(
                        vm,
                        existing_vms,
                        &classes_by_name,
                        &provider_name,
                        None,
                        overhead,
                    );
                    let reserve = provider.spec.scheduling.as_ref().map_or(
                        DEFAULT_STORAGE_RESERVE_PERCENT,
                        ProviderScheduling::storage_reserve_percent,
                    );
                    if let Some(short) = measured_shortfall(
                        u64::from(effective_memory_mi_b(
                            class,
                            vm.spec.hardware_override.as_ref(),
                        )) + overhead,
                        &disk_sizes(class, vm),
                        &in_domain,
                        &in_provider,
                        capacity,
                        reserve,
                    ) {
                        reject_reasons.push(format!(
                            "{provider_name}/{}: insufficient capacity: {short}",
                            fd.name,
                        ));
                        continue;
                    }
                }
            }

            survivors.push((provider, fd));
        }
    }

    if survivors.is_empty() {
        if !image_ready_seen {
            return Err(ScheduleError::ImageNotReady);
        }
        if firmware_unsupported_seen {
            return Err(ScheduleError::FirmwareUnsupported);
        }
        if tpm_unsupported_seen {
            return Err(ScheduleError::TpmUnsupported);
        }
        // Only when the size is the one thing in the way: a mix of reasons
        // keeps the compound message, which names this one among them.
        if size_rejects > 0 && size_rejects == reject_reasons.len() {
            return Err(ScheduleError::InstanceSizeUnmapped(class_name));
        }
        if let Some(err) = ec2_rejects.sole_cause(reject_reasons.len(), &class_name, image) {
            return Err(err);
        }
        return Err(ScheduleError::NoFailureDomainMatched(
            reject_reasons.render(),
        ));
    }

    // Step 9: required anti-affinity.
    let pre_aa_len = survivors.len();
    let mut anti_affinity_offending: Option<String> = None;
    for rule in placement
        .anti_affinity
        .iter()
        .filter(|r| r.mode_is_required())
    {
        let blocked_values = blocked_topology_values(rule, vm, existing_vms);
        survivors.retain(|(_, fd)| {
            fd.labels
                .get(&rule.topology_key)
                .map(|v| !blocked_values.contains(v))
                .unwrap_or(true)
        });
        if survivors.is_empty() {
            anti_affinity_offending = Some(rule.topology_key.clone());
            break;
        }
    }

    if survivors.is_empty() {
        let key = anti_affinity_offending.unwrap_or_default();
        return Err(ScheduleError::AntiAffinityUnsatisfied(key));
    }

    let _ = pre_aa_len; // (kept for future scoring use)

    // ADR-0125: a placement that still fits is kept. Only a new VM, or one
    // whose placement failed a filter above, is spread.
    if let Some((provider, fd)) = survivors.iter().find(|(p, fd)| is_current(p, fd)) {
        return Ok(build_decision(vm, class, provider, fd));
    }

    // Spread (ADR-0098): least loaded for the VM's group, then overall, then
    // rendezvous hash.
    let (provider, fd) = survivors
        .iter()
        .min_by_key(|(p, fd)| {
            let (group, total) = placement_load(vm, existing_vms, &p.name_any(), &fd.name);
            let rendezvous = fnv1a(&[&vm.name_any(), &p.name_any(), &fd.name]);
            (group, total, std::cmp::Reverse(rendezvous))
        })
        .expect("survivors non-empty");

    Ok(build_decision(vm, class, provider, fd))
}

/// [`schedule`] with the `(provider, failure domain)` pairs in `excluded`
/// removed from the candidates, and **no** fallback: `Err` when nothing else
/// fits. The VM's own recorded placement is excluded like any other pair,
/// so a VM refused there is spread anew (ADR-0173 Decision 2).
///
/// # Errors
/// The [`ScheduleError`] of the pruned candidate set.
pub fn schedule_avoiding(
    vm: &VirtualMachine,
    class: &VMClass,
    image: &VMImage,
    providers: &[Provider],
    existing_vms: &[VirtualMachine],
    classes: &[VMClass],
    excluded: &[(String, String)],
) -> Result<Decision, ScheduleError> {
    let pruned: Vec<Provider> = providers
        .iter()
        .cloned()
        .map(|mut provider| {
            let name = provider.name_any();
            if let Some(status) = provider.status.as_mut() {
                status
                    .failure_domains
                    .retain(|fd| !excluded.iter().any(|(p, d)| *p == name && *d == fd.name));
            }
            provider
        })
        .collect();
    schedule(vm, class, image, &pruned, existing_vms, classes)
}

/// [`schedule_avoiding`], falling back to [`schedule`] when the exclusions
/// leave nothing: an exclusion is a preference and never makes a VM
/// unschedulable (ADR-0173 Decision 4). Empty `excluded` is [`schedule`].
///
/// # Errors
/// The [`ScheduleError`] of the full candidate set.
pub fn schedule_excluding(
    vm: &VirtualMachine,
    class: &VMClass,
    image: &VMImage,
    providers: &[Provider],
    existing_vms: &[VirtualMachine],
    classes: &[VMClass],
    excluded: &[(String, String)],
) -> Result<Decision, ScheduleError> {
    if excluded.is_empty() {
        return schedule(vm, class, image, providers, existing_vms, classes);
    }
    schedule_avoiding(vm, class, image, providers, existing_vms, classes, excluded)
        .or_else(|_| schedule(vm, class, image, providers, existing_vms, classes))
}

/// FNV-1a 64-bit offset basis and prime.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over `parts`, each followed by a NUL so `("ab", "c")` and
/// `("a", "bc")` differ. Fixed, unlike `std`'s hasher, so a placement does
/// not change with the Rust version.
fn fnv1a(parts: &[&str]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for part in parts {
        for byte in part.bytes().chain(std::iter::once(0)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    hash
}

/// `(group, total)`: how many VMs other than `vm` are scheduled on
/// `(provider, failure_domain)`, counting only `vm`'s spread group, then all
/// of them. A pool member's group is its pool (`LABEL_POOL`); any other
/// VM's group is every VM.
fn placement_load(
    vm: &VirtualMachine,
    existing_vms: &[VirtualMachine],
    provider: &str,
    failure_domain: &str,
) -> (usize, usize) {
    let me = vm.name_any();
    let pool = vm.labels().get(LABEL_POOL);
    existing_vms
        .iter()
        .filter(|other| other.name_any() != me)
        .filter(|other| {
            other
                .status
                .as_ref()
                .and_then(|s| s.scheduled.as_ref())
                .is_some_and(|s| s.provider_name == provider && s.failure_domain == failure_domain)
        })
        .fold((0, 0), |(group, total), other| {
            let same_group = pool.is_none() || other.labels().get(LABEL_POOL) == pool;
            (group + usize::from(same_group), total + 1)
        })
}

/// vCPUs and memory (MiB, VMM overhead included) already placed on one
/// failure domain (ADR-0107).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// vCPUs of the VMs placed there.
    pub cpus: u64,
    /// Their memory plus the per-VM overhead, MiB.
    pub memory_mi_b: u64,
}

/// What VMs other than `vm` scheduled on `(provider, failure_domain)` use,
/// from their classes with their `hardwareOverride` applied. A VM whose class is not in `classes` counts at zero
/// (and is logged): a deleted class must not block every placement.
fn usage_on(
    vm: &VirtualMachine,
    existing_vms: &[VirtualMachine],
    classes: &HashMap<String, &VMClass>,
    provider: &str,
    failure_domain: &str,
    overhead_mi_b: u64,
) -> Usage {
    let me = vm.name_any();
    existing_vms
        .iter()
        .filter(|other| other.name_any() != me)
        .filter(|other| {
            other
                .status
                .as_ref()
                .and_then(|s| s.scheduled.as_ref())
                .is_some_and(|s| s.provider_name == provider && s.failure_domain == failure_domain)
        })
        .fold(Usage::default(), |used, other| {
            let Some(class) = classes.get(&other.spec.class_ref.name) else {
                warn!(
                    vm = %other.name_any(),
                    class = %other.spec.class_ref.name,
                    "placed VM's class not found; not counted against capacity"
                );
                return used;
            };
            let override_ = other.spec.hardware_override.as_ref();
            Usage {
                cpus: used.cpus + u64::from(effective_cpus(class, override_)),
                memory_mi_b: used.memory_mi_b
                    + u64::from(effective_memory_mi_b(class, override_))
                    + overhead_mi_b,
            }
        })
}

/// Why a VM needing `cpus` vCPUs and `memory_mi_b` MiB (overhead included)
/// does not fit beside `used`, or `None` when it fits. Memory first: it is
/// the resource whose overflow kills a guest. A figure `capacity` lacks is
/// not checked.
#[must_use]
pub fn capacity_shortfall(
    cpus: u32,
    memory_mi_b: u64,
    used: Usage,
    capacity: &FailureDomainCapacity,
    cpu_overcommit_ratio: u32,
) -> Option<String> {
    if let Some(available) = capacity.memory_mi_b {
        let needed = used.memory_mi_b + memory_mi_b;
        if needed > available {
            return Some(format!("memory {needed} MiB > {available} MiB"));
        }
    }
    if let Some(host_cpus) = capacity.cpus {
        let needed = used.cpus + u64::from(cpus);
        let allowed = u64::from(host_cpus) * u64::from(cpu_overcommit_ratio);
        if needed > allowed {
            return Some(format!(
                "vCPUs {needed} > {host_cpus} CPUs x {cpu_overcommit_ratio}"
            ));
        }
    }
    None
}

/// What VMs scheduled on a failure domain but not yet provisioned will take
/// (ADR-0128): they are promised there but not yet in its measured figures.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InFlight {
    /// Their vCPUs (counted against a cloud Provider's quota, ADR-0161).
    pub cpus: u64,
    /// Their memory plus the per-VM overhead, MiB.
    pub memory_mi_b: u64,
    /// Their disks, GiB, by storage class.
    pub disks_gi_b: BTreeMap<String, u64>,
}

/// Whether `provider` publishes cloud quota headroom as its capacity
/// (ADR-0161 Decision 1) rather than a host's measured resources.
fn is_quota_capacity(provider: &Provider) -> bool {
    CLOUD_PROVIDER_CLASSES.contains(&provider.spec.provider_class_ref.name.as_str())
}

/// Why a VM needing `cpus` vCPUs does not fit a cloud Provider's quota
/// headroom once the VMs placed on the Provider but not yet provisioned
/// (`in_provider`) are counted, or `None` when it fits or no headroom is
/// published (ADR-0161). Never overcommitted: a quota is a hard limit.
#[must_use]
pub fn quota_shortfall(
    cpus: u32,
    in_provider: &InFlight,
    capacity: &FailureDomainCapacity,
) -> Option<String> {
    let headroom = u64::from(capacity.cpus?);
    let needed = in_provider.cpus + u64::from(cpus);
    if needed <= headroom {
        return None;
    }
    Some(format!(
        "vCPUs {needed} (in flight {}) > quota headroom {headroom}",
        in_provider.cpus
    ))
}

/// Whether the VM's infrastructure reports itself provisioned.
fn is_provisioned(vm: &VirtualMachine) -> bool {
    vm.status
        .as_ref()
        .and_then(|s| s.initialization.provisioned)
        .unwrap_or(false)
}

/// The VM's disks, GiB, summed by storage class, with its
/// `hardwareOverride.diskOverrides` applied.
fn disk_sizes(class: &VMClass, vm: &VirtualMachine) -> BTreeMap<String, u64> {
    let override_ = vm.spec.hardware_override.as_ref();
    class
        .spec
        .hardware
        .disks
        .iter()
        .fold(BTreeMap::new(), |mut sizes, disk| {
            *sizes.entry(disk.storage_class.clone()).or_default() +=
                u64::from(merge_disk_size_override(disk, override_));
            sizes
        })
}

/// What in-flight VMs other than `vm` hold on `provider`: on one failure
/// domain, or on any of the Provider's when `failure_domain` is `None` (for a
/// shared store). A VM whose class is not found counts at zero, as in
/// [`usage_on`].
fn in_flight_on(
    vm: &VirtualMachine,
    existing_vms: &[VirtualMachine],
    classes: &HashMap<String, &VMClass>,
    provider: &str,
    failure_domain: Option<&str>,
    overhead_mi_b: u64,
) -> InFlight {
    let me = vm.name_any();
    existing_vms
        .iter()
        .filter(|other| other.name_any() != me && !is_provisioned(other))
        .filter(|other| {
            other
                .status
                .as_ref()
                .and_then(|s| s.scheduled.as_ref())
                .is_some_and(|s| {
                    s.provider_name == provider
                        && failure_domain.is_none_or(|fd| s.failure_domain == fd)
                })
        })
        .fold(InFlight::default(), |mut held, other| {
            let Some(class) = classes.get(&other.spec.class_ref.name) else {
                return held;
            };
            held.cpus += u64::from(effective_cpus(class, other.spec.hardware_override.as_ref()));
            held.memory_mi_b += u64::from(effective_memory_mi_b(
                class,
                other.spec.hardware_override.as_ref(),
            )) + overhead_mi_b;
            for (storage_class, gi_b) in disk_sizes(class, other) {
                *held.disks_gi_b.entry(storage_class).or_default() += gi_b;
            }
            held
        })
}

/// Why a new VM needing `memory_mi_b` (overhead included) and `disks_gi_b`
/// by storage class does not fit the failure domain's measured figures, or
/// `None` when it fits (ADR-0128). Memory is checked against
/// `memoryAvailableMiB` less `in_domain`; each storage class against its
/// store's `freeGiB` less `reserve_percent` of `totalGiB` (no reserve when
/// the total is unknown) and less the in-flight disks: `in_provider`'s for a
/// shared store, `in_domain`'s otherwise. A figure `capacity` lacks is not
/// checked.
#[must_use]
pub fn measured_shortfall(
    memory_mi_b: u64,
    disks_gi_b: &BTreeMap<String, u64>,
    in_domain: &InFlight,
    in_provider: &InFlight,
    capacity: &FailureDomainCapacity,
    reserve_percent: u32,
) -> Option<String> {
    if let Some(available) = capacity.memory_available_mi_b {
        let free = available.saturating_sub(in_domain.memory_mi_b);
        if memory_mi_b > free {
            return Some(format!(
                "memory available {free} MiB < {memory_mi_b} MiB (measured, less in-flight VMs)"
            ));
        }
    }
    for (storage_class, needed) in disks_gi_b {
        let Some(store) = capacity.storage_for(storage_class) else {
            continue;
        };
        let Some(free) = store.free_gi_b else {
            continue;
        };
        let reserve = store
            .total_gi_b
            .map_or(0, |total| total * u64::from(reserve_percent) / PERCENT);
        let held = if store.shared { in_provider } else { in_domain };
        let in_flight = held.disks_gi_b.get(storage_class).copied().unwrap_or(0);
        let usable = free.saturating_sub(reserve).saturating_sub(in_flight);
        if *needed > usable {
            return Some(format!(
                "storage class '{storage_class}': {needed} GiB > {usable} GiB \
                 ({free} free less {reserve} reserve and {in_flight} in flight)"
            ));
        }
    }
    None
}

/// Trait extension so we can call `mode_is_required()` directly on a rule.
trait AntiAffinityRuleExt {
    fn mode_is_required(&self) -> bool;
}
impl AntiAffinityRuleExt for AntiAffinityRule {
    fn mode_is_required(&self) -> bool {
        matches!(self.mode, AffinityMode::Required)
    }
}

/// Compute the set of topology-key values that are already taken by sibling
/// VMs matching the rule's label selector. `vm` itself is never its own
/// sibling: a placed VM whose labels match its rule must not repel itself
/// from its own domain (ADR-0125).
fn blocked_topology_values(
    rule: &AntiAffinityRule,
    me: &VirtualMachine,
    existing_vms: &[VirtualMachine],
) -> Vec<String> {
    let me = me.name_any();
    existing_vms
        .iter()
        .filter(|vm| vm.name_any() != me)
        .filter(|vm| selector_matches(&Some(rule.label_selector.clone()), vm.labels()))
        .filter_map(|vm| {
            let scheduled = vm.status.as_ref()?.scheduled.as_ref()?;
            // Use the FAILURE DOMAIN's labels from the already-scheduled VM.
            // We can't see them directly from another VM, so we look at the
            // value the rule's topology_key was matched against — stored on
            // VirtualMachineStatus.scheduled is not enough. Approximation:
            // for v1 we treat the failure_domain *name* as the topology
            // value. Real label-based lookups land with the scheduler cache
            // in iteration 3.
            Some(scheduled.failure_domain.clone())
        })
        .collect()
}

/// Returns the first element of `requested` that is NOT in `available`.
fn first_missing<'a, I, S>(requested: I, available: &[String]) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a S>,
    S: AsRef<str> + 'a + ?Sized,
{
    requested
        .into_iter()
        .map(|s| s.as_ref())
        .find(|req| !available.iter().any(|a| a == req))
}

/// Check `VMImage.status.per_provider[i]` for `provider_name`.
fn image_ready_on_provider(image: &VMImage, provider_name: &str) -> bool {
    image.status.as_ref().is_some_and(|s| {
        s.per_provider
            .iter()
            .any(|p| p.provider_name == provider_name && p.ready)
    })
}

/// Per-zone image readiness for a `Template`-kind source (ADR-0104).
///
/// A vsphere `Template` source publishes one `zones[]` entry per failure
/// domain, and a zone whose template is missing must not receive a VM even
/// though the provider-level row is ready (other zones have it). Applies only
/// when the image's source for this provider's class is `Template` **and** the
/// row has `zones[]`; every other case (`Url`, libvirt / Proxmox rows with no
/// zones, a legacy row) keeps the provider-level check alone, so `Url`
/// scheduling is unchanged.
fn image_ready_in_zone(image: &VMImage, provider: &Provider, zone: &str) -> bool {
    let class = &provider.spec.provider_class_ref.name;
    let template_sourced = image
        .spec
        .sources
        .iter()
        .any(|s| &s.provider_class == class && s.kind == ImageSourceKind::Template);
    if !template_sourced {
        return true;
    }
    let provider_name = provider.name_any();
    let Some(row) = image.status.as_ref().and_then(|s| {
        s.per_provider
            .iter()
            .find(|p| p.provider_name == provider_name)
    }) else {
        return true;
    };
    if row.zones.is_empty() {
        return true;
    }
    row.zones.iter().any(|z| z.name == zone && z.ready)
}

/// Whether `provider` is of the `ec2` class (ADR-0174).
fn is_ec2(provider: &Provider) -> bool {
    provider.spec.provider_class_ref.name == PROVIDER_CLASS_EC2
}

/// Why step 3d refused an `ec2` failure domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ec2Reject {
    /// The image sets `trustedBoot` (ADR-0167 Decision 5).
    TrustedBoot,
    /// The class's instance type is not offered in the zone.
    NotOffered,
    /// The class's instance type does not run the image's architecture.
    Architecture,
    /// The class sets `tpmEnabled` and the AMI lacks NitroTPM support.
    AmiTpm,
    /// The class sets `tpmEnabled` and its type lacks NitroTPM.
    NitroTpm,
}

impl Ec2Reject {
    /// The per-candidate reject reason.
    fn describe(self, class_name: &str, image: &VMImage, provider: &Provider) -> String {
        match self {
            Self::TrustedBoot => "image sets trustedBoot, which ec2 refuses (ADR-0167)".to_string(),
            Self::NotOffered => {
                format!("VMClass '{class_name}' instance type not offered in this zone")
            }
            Self::Architecture => format!(
                "VMClass '{class_name}' instance type does not run {}",
                architecture_name(&ec2_image_architecture(image, provider))
            ),
            Self::AmiTpm => format!(
                "tpmEnabled requested but image '{}' AMI is not registered with TpmSupport=v2.0 and BootMode=uefi",
                image.name_any()
            ),
            Self::NitroTpm => format!(
                "tpmEnabled requested but VMClass '{class_name}' instance type has no NitroTPM here"
            ),
        }
    }
}

/// How many candidates step 3d refused, by reason.
#[derive(Default)]
struct Ec2RejectCounts {
    trusted_boot: usize,
    not_offered: usize,
    architecture: usize,
    ami_tpm: usize,
    nitro_tpm: usize,
    /// The architecture last refused, for the error.
    refused_architecture: Option<Architecture>,
}

impl Ec2RejectCounts {
    fn count(&mut self, reject: Ec2Reject) {
        let slot = match reject {
            Ec2Reject::TrustedBoot => &mut self.trusted_boot,
            Ec2Reject::NotOffered => &mut self.not_offered,
            Ec2Reject::Architecture => &mut self.architecture,
            Ec2Reject::AmiTpm => &mut self.ami_tpm,
            Ec2Reject::NitroTpm => &mut self.nitro_tpm,
        };
        *slot += 1;
    }

    /// The typed error when one ec2 reason refused every one of `total`
    /// rejected candidates; `None` for a mix, which keeps the compound
    /// message.
    fn sole_cause(&self, total: usize, class_name: &str, image: &VMImage) -> Option<ScheduleError> {
        if total == 0 {
            return None;
        }
        if self.trusted_boot == total {
            return Some(ScheduleError::TrustedBootUnsupported);
        }
        if self.not_offered == total {
            return Some(ScheduleError::InstanceTypeNotOffered(
                class_name.to_string(),
            ));
        }
        if self.architecture == total {
            let architecture = self
                .refused_architecture
                .clone()
                .unwrap_or_else(|| image.spec.architecture.clone());
            return Some(ScheduleError::ArchitectureMismatch {
                vm_class: class_name.to_string(),
                architecture: architecture_name(&architecture).to_string(),
            });
        }
        if self.ami_tpm == total {
            return Some(ScheduleError::AmiTpmUnsupported(image.name_any()));
        }
        if self.nitro_tpm == total {
            return Some(ScheduleError::NitroTpmUnsupported(class_name.to_string()));
        }
        None
    }

    /// Remembers which architecture an [`Ec2Reject::Architecture`] refused.
    fn note_architecture(&mut self, architecture: Architecture) {
        self.refused_architecture = Some(architecture);
    }
}

/// What `DescribeImages` recorded of the image's AMI for `provider`: the
/// `ec2` attributes on the image's row for it, if any.
pub(crate) fn ec2_image_attributes<'a>(
    image: &'a VMImage,
    provider: &Provider,
) -> Option<&'a Ec2ImageAttributes> {
    let name = provider.name_any();
    let namespace = provider.namespace().unwrap_or_default();
    image
        .status
        .as_ref()?
        .per_provider
        .iter()
        .find(|r| r.provider_name == name && r.provider_namespace == namespace)?
        .ec2
        .as_ref()
}

/// The architecture an ec2 placement must run: the AMI's, as recorded for
/// `provider`, else the `VMImage`'s declared one.
fn ec2_image_architecture(image: &VMImage, provider: &Provider) -> Architecture {
    ec2_image_attributes(image, provider).map_or_else(
        || image.spec.architecture.clone(),
        |a| a.architecture.clone(),
    )
}

/// The CRD spelling of an image architecture.
fn architecture_name(architecture: &Architecture) -> &'static str {
    match architecture {
        Architecture::Amd64 => "amd64",
        Architecture::Arm64 => "arm64",
    }
}

/// The raw key listing the classes whose ec2 type runs `architecture`.
fn architecture_key(architecture: &Architecture) -> &'static str {
    match architecture {
        Architecture::Amd64 => FD_RAW_EC2_AMD64_VM_CLASSES,
        Architecture::Arm64 => FD_RAW_EC2_ARM64_VM_CLASSES,
    }
}

/// Whether `class_name` is in `fd`'s comma-separated raw list `key`:
/// `None` when the provider did not publish the key (unknown).
fn raw_list_contains(fd: &FailureDomain, key: &str, class_name: &str) -> Option<bool> {
    fd.attributes.raw.get(key).map(|list| {
        list.split(RAW_LIST_SEPARATOR)
            .map(str::trim)
            .any(|c| c == class_name)
    })
}

/// Step 3d for one `ec2` failure domain (roadmap 24 phase E): the first
/// reason it cannot take a new VM of `class` and `image`, or `None`.
///
/// Every list but `tpmVmClasses` filters only when published: a provider
/// that did not say is not taken to have said no. A TPM is never assumed,
/// so an unpublished `tpmVmClasses` refuses a `tpmEnabled` class.
///
/// The AMI's architecture and TPM support come from the `ec2` attributes the
/// provider recorded with `DescribeImages` on the image's row for this
/// Provider. A row without them (an AMI not read yet, or published before
/// the provider recorded them) falls back to the `VMImage`'s declared
/// architecture and does not filter on TPM support.
pub(crate) fn ec2_reject(
    class: &VMClass,
    image: &VMImage,
    provider: &Provider,
    fd: &FailureDomain,
) -> Option<Ec2Reject> {
    let class_name = class.name_any();
    if image.spec.trusted_boot.is_some() {
        return Some(Ec2Reject::TrustedBoot);
    }
    if raw_list_contains(fd, FD_RAW_EC2_VM_CLASSES, &class_name) == Some(false) {
        return Some(Ec2Reject::NotOffered);
    }
    let arch_key = architecture_key(&ec2_image_architecture(image, provider));
    if raw_list_contains(fd, arch_key, &class_name) == Some(false) {
        return Some(Ec2Reject::Architecture);
    }
    if class.spec.tpm_enabled
        && ec2_image_attributes(image, provider).is_some_and(|a| !a.supports_nitro_tpm())
    {
        return Some(Ec2Reject::AmiTpm);
    }
    if class.spec.tpm_enabled
        && raw_list_contains(fd, FD_RAW_EC2_TPM_VM_CLASSES, &class_name) != Some(true)
    {
        return Some(Ec2Reject::NitroTpm);
    }
    None
}

/// The availability zone a failure domain stands for: its raw `zone`
/// attribute, else its `zone` label (ADR-0158 Decision 3). `None` on a
/// backend with no zones.
pub(crate) fn failure_domain_zone(fd: &FailureDomain) -> Option<&str> {
    fd.attributes
        .raw
        .get(FD_ZONE_KEY)
        .or_else(|| fd.labels.get(FD_ZONE_KEY))
        .map(String::as_str)
}

/// Whether `provider` maps classes to instance sizes and has none for
/// `class_name` in `fd`'s zone (ADR-0160 Decision 1). A Provider maps
/// classes when it declares `instanceSizes`, or is a cloud class
/// ([`CLOUD_PROVIDER_CLASSES`]), where an empty list means nothing is
/// schedulable rather than "no mapping needed".
fn instance_size_unmapped(provider: &Provider, class_name: &str, fd: &FailureDomain) -> bool {
    let caps = &provider.spec.capabilities;
    let maps_sizes = !caps.instance_sizes.is_empty()
        || CLOUD_PROVIDER_CLASSES.contains(&provider.spec.provider_class_ref.name.as_str());
    if !maps_sizes {
        return false;
    }
    caps.instance_size_for(class_name, failure_domain_zone(fd).unwrap_or_default())
        .is_none()
}

/// Build the final [`Decision`] from the chosen provider + fd. Resolves
/// every storage / network class the VMClass requests against the Provider's
/// declared capability mappings.
fn build_decision(
    _vm: &VirtualMachine,
    class: &VMClass,
    provider: &Provider,
    fd: &FailureDomain,
) -> Decision {
    // Backends with a (datacenter, cluster) concept (vSphere) carry both in
    // `attributes.raw`; a backend without one (libvirt's single-host failure
    // domains) simply never populates `per_zone` on its mappings either, so
    // an empty string here never matches anything and `target_for` falls
    // through to the mapping's default `target` (ADR-0030).
    let datacenter = fd.attributes.raw.get("datacenter").map_or("", |s| s);
    let cluster = fd.attributes.raw.get("cluster").map_or("", |s| s);

    let resolved_storage = class
        .spec
        .hardware
        .disks
        .iter()
        .map(|disk| ResolvedResource {
            class_name: disk.storage_class.clone(),
            backend_id: first_target_value(
                &provider.spec.capabilities,
                &disk.storage_class,
                StorageOrNetwork::Storage,
                datacenter,
                cluster,
            )
            .unwrap_or_else(|| disk.storage_class.clone()),
        })
        .collect();

    let resolved_networks = class
        .spec
        .network
        .interfaces
        .iter()
        .map(|nic| ResolvedResource {
            class_name: nic.network_class.clone(),
            backend_id: first_target_value(
                &provider.spec.capabilities,
                &nic.network_class,
                StorageOrNetwork::Network,
                datacenter,
                cluster,
            )
            .unwrap_or_else(|| nic.network_class.clone()),
        })
        .collect();

    Decision {
        provider_name: provider.name_any(),
        provider_namespace: provider
            .namespace()
            .unwrap_or_else(|| "default".to_string()),
        provider_class: provider.spec.provider_class_ref.name.clone(),
        failure_domain_name: fd.name.clone(),
        resolved_storage,
        resolved_networks,
        failure_domain_raw: fd.attributes.raw.clone(),
        failure_domain_labels: fd.labels.clone(),
    }
}

/// Disambiguator for [`first_target_value`].
enum StorageOrNetwork {
    Storage,
    Network,
}

/// Resolve a class name to a single concrete backend identifier using the
/// **first BTreeMap value by key-order** rule. See module docs. The target
/// itself is resolved for this specific `(datacenter, cluster)` zone —
/// an exact per-zone override if the mapping has one, else its default
/// target (ADR-0030) — so the same class name can back a different
/// concrete resource per failure domain of the same Provider.
fn first_target_value(
    caps: &ProviderCapabilities,
    class_name: &str,
    kind: StorageOrNetwork,
    datacenter: &str,
    cluster: &str,
) -> Option<String> {
    match kind {
        StorageOrNetwork::Storage => caps
            .storage_classes
            .iter()
            .find(|m: &&StorageClassMapping| m.name == class_name)
            .and_then(|m| m.target_for(datacenter, cluster))
            .and_then(|t| t.values().next().cloned()),
        StorageOrNetwork::Network => caps
            .network_classes
            .iter()
            .find(|m: &&NetworkClassMapping| m.name == class_name)
            .and_then(|m| m.target_for(datacenter, cluster))
            .and_then(|t| t.values().next().cloned()),
    }
}

/// Evaluate a [`LabelSelector`]. `None` selector matches everything.
///
/// Shared with the `vsphere_cluster` reconciler (Provider selection and
/// control-plane failure-domain eligibility).
pub(crate) fn selector_matches(
    selector: &Option<LabelSelector>,
    labels: &BTreeMap<String, String>,
) -> bool {
    let Some(sel) = selector else {
        return true;
    };
    label_selector_matches(sel, labels)
}

/// Evaluate a present [`LabelSelector`]. The empty selector matches
/// everything, as in Kubernetes.
///
/// Shared with the claim planner, which matches `spec.poolSelector` against
/// pool labels (ADR-0132).
pub(crate) fn label_selector_matches(
    sel: &LabelSelector,
    labels: &BTreeMap<String, String>,
) -> bool {
    for (k, v) in &sel.match_labels {
        if labels.get(k) != Some(v) {
            return false;
        }
    }
    for req in &sel.match_expressions {
        let present = labels.get(&req.key);
        let ok = match req.operator {
            LabelSelectorOperator::In => present.is_some_and(|v| req.values.iter().any(|x| x == v)),
            LabelSelectorOperator::NotIn => {
                present.is_none_or(|v| !req.values.iter().any(|x| x == v))
            }
            LabelSelectorOperator::Exists => present.is_some(),
            LabelSelectorOperator::DoesNotExist => present.is_none(),
        };
        if !ok {
            return false;
        }
    }
    true
}

// Convenience accessor used by tests + reconciler.
impl Decision {
    /// Convert this decision into a [`banlieue_api::banlieue::ScheduledPlacement`]
    /// suitable for writing to `VirtualMachineStatus.scheduled`.
    pub fn to_scheduled_placement(
        &self,
        now: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time,
    ) -> banlieue_api::banlieue::ScheduledPlacement {
        banlieue_api::banlieue::ScheduledPlacement {
            provider_name: self.provider_name.clone(),
            provider_class: self.provider_class.clone(),
            failure_domain: self.failure_domain_name.clone(),
            resolved_storage: self.resolved_storage.clone(),
            resolved_networks: self.resolved_networks.clone(),
            scheduled_at: Some(now),
        }
    }

    /// As [`Self::to_scheduled_placement`], but an unchanged decision keeps the time it
    /// was made.
    ///
    /// `scheduledAt` is "the time the placement decision was made". The reconciler runs this
    /// on every pass, and stamping `now` each time rewrote the status of every VM on every
    /// reconcile: a new revision and a watch event on every control-plane node, and the event
    /// woke the next reconcile, so the write fed itself (about four seconds a VM, about 25
    /// events a second over 77 VMs, which overflowed a watch subscriber in kine). So `previous`
    /// wins whenever the new placement equals it in everything but the time, and `now` is used
    /// only for a first placement, a changed one, or a previous status that never recorded a
    /// time.
    ///
    /// # Arguments
    /// * `previous` - the placement already on the VM's status, if any.
    /// * `now` - the time to record for a new or changed decision.
    pub fn to_scheduled_placement_keeping(
        &self,
        previous: Option<&banlieue_api::banlieue::ScheduledPlacement>,
        now: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time,
    ) -> banlieue_api::banlieue::ScheduledPlacement {
        let mut next = self.to_scheduled_placement(now);
        if let Some(previous) = previous.filter(|p| p.scheduled_at.is_some()) {
            // Equal in every field but the time, including any field added later.
            let same = banlieue_api::banlieue::ScheduledPlacement {
                scheduled_at: next.scheduled_at.clone(),
                ..previous.clone()
            } == next;
            if same {
                next.scheduled_at = previous.scheduled_at.clone();
            }
        }
        next
    }
}

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod scheduler_tests;
