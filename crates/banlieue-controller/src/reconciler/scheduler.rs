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
//!    `Provider.metadata.labels`.
//! 2. Failure-domain selector — `placement.failureDomainSelector` against
//!    `FailureDomain.labels`.
//! 3. Image readiness — `VMImage.status.perProvider[i].ready == true` for
//!    the candidate Provider.
//! 4. Storage classes — every disk's `storageClass` must appear in
//!    `FailureDomain.attributes.availableStorageClasses`.
//! 5. Network classes — every NIC's `networkClass` must appear in
//!    `FailureDomain.attributes.availableNetworkClasses`.
//! 6. Features — every feature in `VMClass.spec.features` must appear in
//!    `FailureDomain.attributes.features`.
//! 7. Firmware — `efi-secure` requires the feature `efiSecureBoot`; `bios`
//!    and `efi` are assumed universally available (vSphere ≥ 6.5).
//! 8. Anti-affinity (`required`) — drop candidates that would put this VM on
//!    a failure domain sharing a `topologyKey` value with an already-
//!    scheduled VM matching the rule's label selector.
//!
//! ## Tie-break
//!
//! Stable lexicographic order over `(provider_name, failure_domain_name)`.
//! Deterministic and unit-testable; sufficient until a richer score is
//! needed.

use std::collections::BTreeMap;

use banlieue_api::banlieue::{
    AffinityMode, AntiAffinityRule, FailureDomain, NetworkClassMapping, Provider,
    ProviderCapabilities, ResolvedResource, StorageClassMapping, VMClass, VMImage, VirtualMachine,
};
use banlieue_api::common::{Firmware, LabelSelector, LabelSelectorOperator};
use kube::ResourceExt;

/// Feature flag a failure domain must advertise to support EFI secure boot.
pub const FEATURE_EFI_SECURE_BOOT: &str = "efiSecureBoot";

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

    /// Required anti-affinity rule could not be satisfied.
    #[error("required anti-affinity rule on topologyKey={0} left no candidates")]
    AntiAffinityUnsatisfied(String),
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
    /// Required anti-affinity rule could not be satisfied.
    pub const ANTI_AFFINITY_UNSATISFIED: &str = "AntiAffinityUnsatisfied";
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
            ScheduleError::AntiAffinityUnsatisfied(_) => reasons::ANTI_AFFINITY_UNSATISFIED,
        }
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
pub fn schedule(
    vm: &VirtualMachine,
    class: &VMClass,
    image: &VMImage,
    providers: &[Provider],
    existing_vms: &[VirtualMachine],
) -> Result<Decision, ScheduleError> {
    let placement = &vm.spec.placement;

    // Step 1: providerSelector
    let provider_candidates: Vec<&Provider> = providers
        .iter()
        .filter(|p| selector_matches(&placement.provider_selector, p.labels()))
        .collect();
    if provider_candidates.is_empty() {
        return Err(ScheduleError::NoProviderMatched);
    }

    // Walk all (provider, failure_domain) tuples.
    let mut reject_reasons: Vec<String> = Vec::new();
    let mut image_ready_seen = false;
    let mut firmware_unsupported_seen = false;
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

            // Step 3: image readiness
            if !image_ready_for_provider {
                reject_reasons.push(format!(
                    "{}/{}: image {} not ready on this provider",
                    provider.name_any(),
                    fd.name,
                    image.name_any(),
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
        return Err(ScheduleError::NoFailureDomainMatched(
            reject_reasons.join("; "),
        ));
    }

    // Step 8: required anti-affinity.
    let pre_aa_len = survivors.len();
    let mut anti_affinity_offending: Option<String> = None;
    for rule in placement
        .anti_affinity
        .iter()
        .filter(|r| r.mode_is_required())
    {
        let blocked_values = blocked_topology_values(rule, existing_vms);
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

    // Tie-break: alphabetical by (provider_name, failure_domain_name).
    survivors.sort_by(|(a_p, a_fd), (b_p, b_fd)| {
        a_p.name_any()
            .cmp(&b_p.name_any())
            .then_with(|| a_fd.name.cmp(&b_fd.name))
    });
    let _ = pre_aa_len; // (kept for future scoring use)
    let (provider, fd) = survivors.first().expect("survivors non-empty");

    Ok(build_decision(vm, class, provider, fd))
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
/// VMs matching the rule's label selector.
fn blocked_topology_values(
    rule: &AntiAffinityRule,
    existing_vms: &[VirtualMachine],
) -> Vec<String> {
    existing_vms
        .iter()
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

/// Build the final [`Decision`] from the chosen provider + fd. Resolves
/// every storage / network class the VMClass requests against the Provider's
/// declared capability mappings.
fn build_decision(
    _vm: &VirtualMachine,
    class: &VMClass,
    provider: &Provider,
    fd: &FailureDomain,
) -> Decision {
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
/// **first BTreeMap value by key-order** rule. See module docs.
fn first_target_value(
    caps: &ProviderCapabilities,
    class_name: &str,
    kind: StorageOrNetwork,
) -> Option<String> {
    match kind {
        StorageOrNetwork::Storage => caps
            .storage_classes
            .iter()
            .find(|m: &&StorageClassMapping| m.name == class_name)
            .and_then(|m| m.target.values().next().cloned()),
        StorageOrNetwork::Network => caps
            .network_classes
            .iter()
            .find(|m: &&NetworkClassMapping| m.name == class_name)
            .and_then(|m| m.target.values().next().cloned()),
    }
}

/// Evaluate a [`LabelSelector`]. `None` selector matches everything.
fn selector_matches(selector: &Option<LabelSelector>, labels: &BTreeMap<String, String>) -> bool {
    let Some(sel) = selector else {
        return true;
    };
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
}

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod scheduler_tests;
