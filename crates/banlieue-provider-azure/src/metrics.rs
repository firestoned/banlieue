// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The Azure provider's own metrics: the orphan sweep (ADR-0158 Decision 5).
//!
//! | Metric | Type | Labels |
//! | --- | --- | --- |
//! | `banlieue_azure_orphan_sweeps_total` | counter | `provider`, `result` |
//! | `banlieue_azure_orphan_sweep_resources_total` | counter | `provider`, `result` |
//!
//! A sweep's `result` is `completed`, `throttled` (ARM stopped the pass) or
//! `skipped` (the resource group or the machines could not be listed, so
//! nothing was judged). A resource's `result` is `found` (an orphan past its
//! grace), `deleted`, `failed` or `deferred` (left for a later pass by the
//! per-pass limit).
//!
//! `provider` is the `banlieue-provider` tag value, `<namespace>.<name>` of a
//! `Provider`: an object an administrator creates, never a tenant's machine
//! name, so cardinality is bounded by the number of Providers.

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;

use banlieue_provider_sdk::metrics::Metrics;

use crate::reconciler::resources::SweepReport;

/// Sweep passes. The encoder appends `_total`.
pub const SWEEPS_NAME: &str = "banlieue_azure_orphan_sweeps";
/// Resources a sweep judged. The encoder appends `_total`.
pub const SWEEP_RESOURCES_NAME: &str = "banlieue_azure_orphan_sweep_resources";

/// `result` values of [`SWEEPS_NAME`].
pub mod sweep_result {
    /// The pass ran to its end.
    pub const COMPLETED: &str = "completed";
    /// ARM throttled the pass, which stopped.
    pub const THROTTLED: &str = "throttled";
    /// Nothing was judged: a listing failed.
    pub const SKIPPED: &str = "skipped";
}

/// `result` values of [`SWEEP_RESOURCES_NAME`].
pub mod resource_result {
    /// An orphan past its grace period.
    pub const FOUND: &str = "found";
    /// ARM accepted its delete.
    pub const DELETED: &str = "deleted";
    /// Its delete failed; retried next pass.
    pub const FAILED: &str = "failed";
    /// Left for a later pass by the per-pass limit.
    pub const DEFERRED: &str = "deferred";
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct SweepLabels {
    provider: String,
    result: &'static str,
}

/// The orphan sweep's counters. Cheap to clone; clones share series.
#[derive(Debug, Clone, Default)]
pub struct SweepMetrics {
    sweeps: Family<SweepLabels, Counter>,
    resources: Family<SweepLabels, Counter>,
}

impl SweepMetrics {
    /// Adds both families to the process registry, served at `/metrics`.
    pub fn register(&self, metrics: &Metrics) {
        metrics.register(
            SWEEPS_NAME,
            "Orphan sweep passes, by Provider and result",
            self.sweeps.clone(),
        );
        metrics.register(
            SWEEP_RESOURCES_NAME,
            "Resources the orphan sweep found, deleted, failed to delete or deferred, by Provider",
            self.resources.clone(),
        );
    }

    fn add(family: &Family<SweepLabels, Counter>, provider: &str, result: &'static str, n: usize) {
        family
            .get_or_create(&SweepLabels {
                provider: provider.to_string(),
                result,
            })
            .inc_by(u64::try_from(n).unwrap_or(u64::MAX));
    }

    /// Records one sweep pass for `provider` and what it did.
    pub fn record(&self, provider: &str, report: &SweepReport) {
        let result = if report.throttled.is_some() {
            sweep_result::THROTTLED
        } else {
            sweep_result::COMPLETED
        };
        Self::add(&self.sweeps, provider, result, 1);
        for (what, n) in [
            (resource_result::FOUND, report.found),
            (resource_result::DELETED, report.deleted.len()),
            (resource_result::FAILED, report.failed.len()),
            (resource_result::DEFERRED, report.deferred),
        ] {
            Self::add(&self.resources, provider, what, n);
        }
    }

    /// Records a pass for `provider` that judged nothing.
    pub fn record_skipped(&self, provider: &str) {
        Self::add(&self.sweeps, provider, sweep_result::SKIPPED, 1);
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod metrics_tests;
