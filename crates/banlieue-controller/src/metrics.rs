// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The controller's domain gauges (ADR-0091 Decision 3).
//!
//! - `banlieue_virtualmachines{phase}`: `VirtualMachine`s per phase, read
//!   from the VirtualMachine controller's own reflector store at scrape time.
//! - `banlieue_provider_failure_domains{provider,kind}`: failure domains each
//!   `Provider` reports in its status, read from a Provider reflector store.
//!   `kind` is the Provider's `spec.providerClassRef.name` (`vsphere`,
//!   `libvirt`, ...).
//! - `banlieue_provider_heartbeat{provider,kind,state}`: `1` for the
//!   heartbeat the operator last recorded in `status.workload` (ADR-0102),
//!   `0` for the other states; no series until it has recorded one.
//!
//! Both are [`Collector`]s, so a scrape always reflects the cache and a
//! deleted object's series disappears with it. Label values are a fixed
//! phase list, Provider names and ProviderClass names: objects an
//! administrator creates, never tenant `VirtualMachine` names or namespaces.
//!
//! The counting is pure ([`count_phases`], [`failure_domain_counts`]) and
//! unit-tested in `metrics_tests.rs`.

use std::collections::BTreeMap;

use banlieue_api::banlieue::{Heartbeat, Provider, VirtualMachine};
use banlieue_api::common::condition_types;
use banlieue_provider_sdk::status::is_condition_true;
use kube::ResourceExt;
use kube::runtime::reflector::Store;
use prometheus_client::collector::Collector;
use prometheus_client::encoding::{DescriptorEncoder, EncodeLabelSet, EncodeMetric};
use prometheus_client::metrics::MetricType;
use prometheus_client::metrics::gauge::ConstGauge;

/// Gauge name for VirtualMachines per phase.
const VIRTUALMACHINES_NAME: &str = "banlieue_virtualmachines";

/// Gauge name for failure domains per Provider.
const FAILURE_DOMAINS_NAME: &str = "banlieue_provider_failure_domains";

/// Gauge name for each Provider's heartbeat state (ADR-0102).
const HEARTBEAT_NAME: &str = "banlieue_provider_heartbeat";

/// Every heartbeat state, so each evaluated Provider has all three series.
const HEARTBEAT_STATES: [Heartbeat; 3] = [Heartbeat::Alive, Heartbeat::Stale, Heartbeat::Absent];

/// The `state` label value of `heartbeat`.
fn heartbeat_label(heartbeat: Heartbeat) -> &'static str {
    match heartbeat {
        Heartbeat::Alive => "Alive",
        Heartbeat::Stale => "Stale",
        Heartbeat::Absent => "Absent",
    }
}

/// The phase a `VirtualMachine` is counted under.
///
/// `VirtualMachine` has no `status.phase` field; the phase is derived from
/// the status it does carry, first match wins: being deleted, then
/// `spec.paused`, then a `Ready=True` condition, then a placement
/// (`status.scheduled`), else pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VmPhase {
    /// Not yet placed on a provider.
    Pending,
    /// Placed, not yet Ready.
    Provisioning,
    /// `Ready=True`.
    Ready,
    /// `spec.paused`: reconciliation suspended.
    Paused,
    /// `metadata.deletionTimestamp` set.
    Deleting,
}

impl VmPhase {
    /// Every phase, so each always has a series (zero included).
    pub const ALL: &'static [Self] = &[
        Self::Pending,
        Self::Provisioning,
        Self::Ready,
        Self::Paused,
        Self::Deleting,
    ];

    /// The `phase` label value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Provisioning => "Provisioning",
            Self::Ready => "Ready",
            Self::Paused => "Paused",
            Self::Deleting => "Deleting",
        }
    }
}

/// The phase `vm` is counted under. Pure.
pub fn vm_phase(vm: &VirtualMachine) -> VmPhase {
    if vm.metadata.deletion_timestamp.is_some() {
        return VmPhase::Deleting;
    }
    if vm.spec.paused {
        return VmPhase::Paused;
    }
    let Some(status) = vm.status.as_ref() else {
        return VmPhase::Pending;
    };
    if is_condition_true(&status.conditions, condition_types::READY) {
        return VmPhase::Ready;
    }
    if status.scheduled.is_some() {
        return VmPhase::Provisioning;
    }
    VmPhase::Pending
}

/// Count `vms` per phase. Every phase is present, zero when empty.
pub fn count_phases<'a>(
    vms: impl IntoIterator<Item = &'a VirtualMachine>,
) -> BTreeMap<VmPhase, i64> {
    let mut counts: BTreeMap<VmPhase, i64> = VmPhase::ALL.iter().map(|p| (*p, 0)).collect();
    for vm in vms {
        *counts.entry(vm_phase(vm)).or_default() += 1;
    }
    counts
}

/// Failure domains per `(provider name, provider class)`. A Provider with no
/// status yet reports zero.
pub fn failure_domain_counts<'a>(
    providers: impl IntoIterator<Item = &'a Provider>,
) -> BTreeMap<(String, String), i64> {
    let mut counts = BTreeMap::new();
    for provider in providers {
        let domains = provider
            .status
            .as_ref()
            .map_or(0, |s| s.failure_domains.len());
        let key = (
            provider.name_any(),
            provider.spec.provider_class_ref.name.clone(),
        );
        *counts.entry(key).or_default() += i64::try_from(domains).unwrap_or(i64::MAX);
    }
    counts
}

/// Heartbeat series per `(provider name, provider class, state)`: `1` for
/// the recorded state, `0` for the others. A Provider whose heartbeat the
/// operator has not recorded yet has none. Pure.
pub fn heartbeat_series<'a>(
    providers: impl IntoIterator<Item = &'a Provider>,
) -> BTreeMap<(String, String, String), i64> {
    let mut series = BTreeMap::new();
    for provider in providers {
        let Some(current) = provider
            .status
            .as_ref()
            .and_then(|s| s.workload.as_ref())
            .and_then(|w| w.heartbeat)
        else {
            continue;
        };
        for state in HEARTBEAT_STATES {
            let key = (
                provider.name_any(),
                provider.spec.provider_class_ref.name.clone(),
                heartbeat_label(state).to_string(),
            );
            series.insert(key, i64::from(state == current));
        }
    }
    series
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct PhaseLabels {
    phase: &'static str,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ProviderLabels {
    provider: String,
    kind: String,
}

/// `banlieue_virtualmachines{phase}`, from the VirtualMachine store.
pub struct VirtualMachineCollector {
    store: Store<VirtualMachine>,
}

impl VirtualMachineCollector {
    /// A collector over the VirtualMachine controller's reflector store.
    pub fn new(store: Store<VirtualMachine>) -> Self {
        Self { store }
    }
}

impl std::fmt::Debug for VirtualMachineCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualMachineCollector")
            .finish_non_exhaustive()
    }
}

impl Collector for VirtualMachineCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let vms = self.store.state();
        let counts = count_phases(vms.iter().map(AsRef::as_ref));
        let mut family = encoder.encode_descriptor(
            VIRTUALMACHINES_NAME,
            "VirtualMachines by phase (derived from deletion, spec.paused, Ready and placement)",
            None,
            MetricType::Gauge,
        )?;
        for (phase, count) in counts {
            let labels = PhaseLabels {
                phase: phase.as_str(),
            };
            ConstGauge::new(count).encode(family.encode_family(&labels)?)?;
        }
        Ok(())
    }
}

/// `banlieue_provider_failure_domains{provider,kind}`, from a Provider store.
pub struct FailureDomainCollector {
    store: Store<Provider>,
}

impl FailureDomainCollector {
    /// A collector over a Provider reflector store.
    pub fn new(store: Store<Provider>) -> Self {
        Self { store }
    }
}

impl std::fmt::Debug for FailureDomainCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FailureDomainCollector")
            .finish_non_exhaustive()
    }
}

impl Collector for FailureDomainCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let providers = self.store.state();
        let counts = failure_domain_counts(providers.iter().map(AsRef::as_ref));
        let mut family = encoder.encode_descriptor(
            FAILURE_DOMAINS_NAME,
            "Failure domains each Provider reports in its status",
            None,
            MetricType::Gauge,
        )?;
        for ((provider, kind), count) in counts {
            let labels = ProviderLabels { provider, kind };
            ConstGauge::new(count).encode(family.encode_family(&labels)?)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct HeartbeatLabels {
    provider: String,
    kind: String,
    state: String,
}

/// `banlieue_provider_heartbeat{provider,kind,state}`, from a Provider store.
pub struct HeartbeatCollector {
    store: Store<Provider>,
}

impl HeartbeatCollector {
    /// A collector over a Provider reflector store.
    pub fn new(store: Store<Provider>) -> Self {
        Self { store }
    }
}

impl std::fmt::Debug for HeartbeatCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeartbeatCollector").finish_non_exhaustive()
    }
}

impl Collector for HeartbeatCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let providers = self.store.state();
        let series = heartbeat_series(providers.iter().map(AsRef::as_ref));
        let mut family = encoder.encode_descriptor(
            HEARTBEAT_NAME,
            "1 for each Provider's heartbeat state, from its Lease (ADR-0102)",
            None,
            MetricType::Gauge,
        )?;
        for ((provider, kind, state), value) in series {
            let labels = HeartbeatLabels {
                provider,
                kind,
                state,
            };
            ConstGauge::new(value).encode(family.encode_family(&labels)?)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod metrics_tests;
