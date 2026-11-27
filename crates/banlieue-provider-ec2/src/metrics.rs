// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The EC2 provider's own metrics: the orphan sweep (ADR-0158 Decision 5).
//!
//! | Metric | Type | Labels |
//! | --- | --- | --- |
//! | `banlieue_ec2_orphan_sweeps_total` | counter | `provider`, `result` |
//! | `banlieue_ec2_orphan_sweep_resources_total` | counter | `provider`, `result` |
//!
//! A sweep's `result` is `completed`, `throttled` (EC2 stopped the pass) or
//! `skipped` (the instances or the machines could not be listed, so nothing
//! was judged). A resource's `result` is `found` (an orphan past its grace),
//! `deleted` (its terminate was accepted), `failed` or `deferred` (left for a
//! later pass by the per-pass limit).
//!
//! `provider` is the `banlieue-provider` tag value, `<namespace>.<name>` of a
//! `Provider`: an object an administrator creates, never a tenant's machine
//! name, so cardinality is bounded by the number of Providers. The names
//! match the Azure provider's, with `ec2` for `azure`, so one dashboard
//! query covers both clouds.

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;

use banlieue_provider_sdk::metrics::Metrics;

use crate::reconciler::instances::SweepReport;

/// Sweep passes. The encoder appends `_total`.
pub const SWEEPS_NAME: &str = "banlieue_ec2_orphan_sweeps";
/// Instances a sweep judged. The encoder appends `_total`.
pub const SWEEP_RESOURCES_NAME: &str = "banlieue_ec2_orphan_sweep_resources";

/// `result` values of [`SWEEPS_NAME`].
pub mod sweep_result {
    /// The pass ran to its end.
    pub const COMPLETED: &str = "completed";
    /// EC2 throttled the pass, which stopped.
    pub const THROTTLED: &str = "throttled";
    /// Nothing was judged: a listing failed.
    pub const SKIPPED: &str = "skipped";
}

/// `result` values of [`SWEEP_RESOURCES_NAME`].
pub mod resource_result {
    /// An orphan past its grace period.
    pub const FOUND: &str = "found";
    /// EC2 accepted its terminate.
    pub const DELETED: &str = "deleted";
    /// Its terminate failed; retried next pass.
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
            "Instances the orphan sweep found, terminated, failed to terminate or deferred, by Provider",
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
        let result = if report.throttled {
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
