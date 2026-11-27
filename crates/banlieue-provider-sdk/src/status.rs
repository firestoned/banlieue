// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Helpers for managing `metav1.Condition` lists on CR status.
//!
//! These follow the Kubernetes conventions documented at
//! <https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md#typical-status-properties>:
//!
//! - Conditions are keyed by `type`; only one condition of each type may exist.
//! - `lastTransitionTime` updates only when `status` changes.
//! - `observedGeneration` always reflects the controller's view at the time
//!   the condition was set.

use banlieue_api::banlieue::{ImagePerProviderStatus, Provider, ProviderStatus, VMImageStatus};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::ResourceExt;

/// Standard condition status values used across the project. Mirrors
/// `metav1.ConditionStatus` so consumers don't have to import the upstream
/// `k8s_openapi` crate just for the constants.
pub mod condition_status {
    pub const TRUE: &str = "True";
    pub const FALSE: &str = "False";
    pub const UNKNOWN: &str = "Unknown";
}

/// Upsert a condition into `conditions`.
///
/// - If a condition with the same `type_` exists and its `status` differs, the
///   existing entry is replaced and `last_transition_time` is set to `now`.
/// - If a condition with the same `type_` exists and its `status` matches, the
///   entry's `reason`, `message`, and `observed_generation` are updated in
///   place but `last_transition_time` is preserved.
/// - If no condition with `type_` exists, a new entry is appended with
///   `last_transition_time = now`.
///
/// The conditions vector is kept sorted by `type` ascending so that diffs in
/// `kubectl get -o yaml` stay stable.
pub fn set_condition(
    conditions: &mut Vec<Condition>,
    type_: &str,
    status: &str,
    reason: &str,
    message: impl Into<String>,
    observed_generation: i64,
) {
    let message = message.into();
    let now = Time(k8s_openapi::jiff::Timestamp::now());

    if let Some(existing) = conditions.iter_mut().find(|c| c.type_ == type_) {
        if existing.status == status {
            existing.reason = reason.to_string();
            existing.message = message;
            existing.observed_generation = Some(observed_generation);
        } else {
            existing.status = status.to_string();
            existing.reason = reason.to_string();
            existing.message = message;
            existing.observed_generation = Some(observed_generation);
            existing.last_transition_time = now;
        }
    } else {
        conditions.push(Condition {
            type_: type_.to_string(),
            status: status.to_string(),
            reason: reason.to_string(),
            message,
            observed_generation: Some(observed_generation),
            last_transition_time: now,
        });
    }

    conditions.sort_by(|a, b| a.type_.cmp(&b.type_));
}

/// Returns `true` iff a condition with `type_` exists and its status is
/// `"True"`.
pub fn is_condition_true(conditions: &[Condition], type_: &str) -> bool {
    conditions
        .iter()
        .any(|c| c.type_ == type_ && c.status == condition_status::TRUE)
}

/// Returns the condition with `type_`, if any.
pub fn find_condition<'a>(conditions: &'a [Condition], type_: &str) -> Option<&'a Condition> {
    conditions.iter().find(|c| c.type_ == type_)
}

/// The addresses a machine's static interfaces declare, in spec order, as
/// `InternalIP` (ADR-0114, amendment 2026-11-06).
///
/// What a provider reports, labelled as declared, when no source observes
/// an address for a running machine. A DHCP interface declares nothing. The
/// result says what banlieue configured, not that the guest applied it.
#[must_use]
pub fn declared_addresses<'a>(
    ipams: impl IntoIterator<Item = &'a banlieue_api::common::IpamSpec>,
) -> Vec<banlieue_api::common::MachineAddress> {
    ipams
        .into_iter()
        .filter_map(|ipam| ipam.static_.as_ref())
        .map(|cfg| banlieue_api::common::MachineAddress {
            address_type: banlieue_api::common::MachineAddressType::InternalIP,
            address: cfg.address.clone(),
        })
        .collect()
}

/// Condition types every provider writes onto `Provider.status.conditions`.
pub mod provider_condition_types {
    /// Everything declared on the `Provider` is present and usable.
    pub const READY: &str = "Ready";
    /// The provider reached its backend (or, for a host-resident provider,
    /// the API server).
    pub const PROVIDER_REACHABLE: &str = "ProviderReachable";
}

/// The `Provider` status to publish when the backend could not be probed at
/// all: `ProviderReachable` and `Ready` both `False` with the same reason
/// and message.
///
/// `failureDomains` is left **empty** rather than stale: the scheduler reads
/// it to place VMs, and advertising domains nobody could verify would be
/// worse than advertising none. `workload` stays unset because it belongs to
/// banlieue-operator's field manager (ADR-0012).
#[must_use]
pub fn unreachable_provider_status(
    generation: i64,
    reason: &str,
    message: String,
) -> ProviderStatus {
    let mut conditions = Vec::new();
    for type_ in [
        provider_condition_types::PROVIDER_REACHABLE,
        provider_condition_types::READY,
    ] {
        set_condition(
            &mut conditions,
            type_,
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
        ek_ca_certificates: Vec::new(),
    }
}

/// One `VMImage.status.perProvider` row for `provider`, with no zones and no
/// `resolvedRef`. A caller that has either sets it with struct update syntax.
#[must_use]
pub fn image_row(
    provider: &Provider,
    ready: bool,
    reason: &str,
    message: Option<String>,
) -> ImagePerProviderStatus {
    ImagePerProviderStatus {
        provider_name: provider.name_any(),
        provider_namespace: provider.namespace().unwrap_or_default(),
        ready,
        resolved_ref: None,
        reason: Some(reason.to_string()),
        message,
        zones: Vec::new(),
        source_digest: None,
        min_os_disk_size_gib: None,
        superseded_refs: Vec::new(),
        ec2: None,
    }
}

/// The `VMImage` status a provider applies: its own `perProvider` rows and
/// the generation it observed, and nothing else.
///
/// `buildArtifact` is written solely by banlieue-imagebuilder (ADR-0010) and
/// the aggregate `Ready` condition solely by banlieue-controller (ADR-0015):
/// a provider sees only its own rows, so it says nothing about either.
#[must_use]
pub fn provider_rows_status(
    per_provider: Vec<ImagePerProviderStatus>,
    generation: i64,
) -> VMImageStatus {
    VMImageStatus {
        per_provider,
        build_artifact: None,
        conditions: Vec::new(),
        observed_generation: Some(generation),
        first_boot_sealing: None,
    }
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod status_tests;
