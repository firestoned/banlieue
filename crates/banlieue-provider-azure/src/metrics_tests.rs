// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `metrics.rs`: the orphan sweep's series as they appear in
//! the encoded registry.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::*;

    const PROVIDER: &str = "banlieue-system.azure-eastus2";

    fn registered() -> (Metrics, SweepMetrics) {
        let metrics = Metrics::new("banlieue-provider-azure");
        let sweep = SweepMetrics::default();
        sweep.register(&metrics);
        (metrics, sweep)
    }

    fn line(name: &str, result: &str, n: u64) -> String {
        format!(r#"{name}_total{{provider="{PROVIDER}",result="{result}"}} {n}"#)
    }

    #[test]
    fn a_completed_sweep_counts_what_it_found_deleted_failed_and_deferred() {
        let (metrics, sweep) = registered();
        sweep.record(
            PROVIDER,
            &SweepReport {
                found: 5,
                deleted: vec!["a".into(), "b".into()],
                failed: vec![("c".into(), "refused".into())],
                throttled: None,
                deferred: 2,
            },
        );
        let text = metrics.encode().expect("encode");
        for expected in [
            line(SWEEPS_NAME, sweep_result::COMPLETED, 1),
            line(SWEEP_RESOURCES_NAME, resource_result::FOUND, 5),
            line(SWEEP_RESOURCES_NAME, resource_result::DELETED, 2),
            line(SWEEP_RESOURCES_NAME, resource_result::FAILED, 1),
            line(SWEEP_RESOURCES_NAME, resource_result::DEFERRED, 2),
        ] {
            assert!(text.contains(&expected), "missing {expected}:\n{text}");
        }
    }

    #[test]
    fn a_throttled_sweep_is_counted_as_throttled_and_counts_add_up() {
        let (metrics, sweep) = registered();
        let report = SweepReport {
            found: 1,
            deleted: vec!["a".into()],
            throttled: Some(Duration::from_secs(1)),
            ..SweepReport::default()
        };
        sweep.record(PROVIDER, &report);
        sweep.record(PROVIDER, &report);
        let text = metrics.encode().expect("encode");
        assert!(
            text.contains(&line(SWEEPS_NAME, sweep_result::THROTTLED, 2)),
            "{text}"
        );
        assert!(
            text.contains(&line(SWEEP_RESOURCES_NAME, resource_result::DELETED, 2)),
            "{text}"
        );
    }

    #[test]
    fn a_skipped_sweep_judged_nothing() {
        let (metrics, sweep) = registered();
        sweep.record_skipped(PROVIDER);
        let text = metrics.encode().expect("encode");
        assert!(
            text.contains(&line(SWEEPS_NAME, sweep_result::SKIPPED, 1)),
            "{text}"
        );
        assert!(
            !text.contains(&format!("{SWEEP_RESOURCES_NAME}_total{{")),
            "no resource series for a pass that judged nothing: {text}"
        );
    }

    #[test]
    fn an_unregistered_sweep_metrics_records_without_a_registry() {
        // The Context default: tests and callers that never register still
        // record safely.
        let sweep = SweepMetrics::default();
        sweep.record(PROVIDER, &SweepReport::default());
        sweep.record_skipped(PROVIDER);
    }
}
