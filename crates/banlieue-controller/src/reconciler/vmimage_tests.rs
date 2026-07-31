// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `VMImage` aggregate-readiness reconciler.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_provider_sdk::status::condition_status;

    fn row(name: &str, ready: bool, reason: Option<&str>) -> ImagePerProviderStatus {
        ImagePerProviderStatus {
            provider_name: name.to_string(),
            provider_namespace: "banlieue-system".to_string(),
            ready,
            resolved_ref: None,
            reason: reason.map(str::to_string),
            message: None,
            zones: vec![],
        }
    }

    #[test]
    fn no_rows_means_unknown_not_false() {
        // Nothing has reported yet. False would claim knowledge the controller
        // does not have, and would show a red READY column for an image whose
        // providers simply have not reconciled.
        let agg = aggregate_ready(&[]);
        assert_eq!(agg.status, condition_status::UNKNOWN);
        assert_eq!(agg.reason, reasons::NO_PROVIDERS);
    }

    #[test]
    fn ready_only_when_every_provider_is_ready() {
        let agg = aggregate_ready(&[row("vc-1", true, None), row("kvm-1", true, None)]);
        assert_eq!(agg.status, condition_status::TRUE);
        assert_eq!(agg.reason, reasons::RECONCILED);
        assert!(agg.message.contains('2'), "{}", agg.message);
    }

    #[test]
    fn one_unready_provider_makes_the_whole_image_unready() {
        // This is the case the old per-provider aggregation got wrong: vSphere
        // saw only its own ready row and published Ready=True while libvirt
        // was still importing.
        let agg = aggregate_ready(&[
            row("vc-1", true, None),
            row("kvm-1", false, Some("Importing")),
        ]);
        assert_eq!(agg.status, condition_status::FALSE);
    }

    #[test]
    fn the_aggregate_reason_is_inherited_from_a_blocking_provider() {
        // An operator reading `Ready=False` needs somewhere to look next, so
        // the aggregate borrows a blocking provider's own reason.
        //
        // *Which* one is chosen by provider identity, not list position:
        // perProvider is merge-keyed (ADR-0015), so the apiserver returns it
        // in no guaranteed order and "the first" is not a stable concept.
        // Ordering by (namespace, name) makes the condition stop flapping
        // between two equally-blocking providers.
        let agg = aggregate_ready(&[
            row("vc-1", false, Some("TemplateNotFound")),
            row("kvm-1", false, Some("Importing")),
        ]);
        assert_eq!(agg.reason, "Importing", "kvm-1 sorts before vc-1");
        assert!(agg.message.contains("2 of 2"), "{}", agg.message);
    }

    #[test]
    fn the_aggregate_reason_does_not_depend_on_list_order() {
        let forward = aggregate_ready(&[
            row("vc-1", false, Some("TemplateNotFound")),
            row("kvm-1", false, Some("Importing")),
        ]);
        let reversed = aggregate_ready(&[
            row("kvm-1", false, Some("Importing")),
            row("vc-1", false, Some("TemplateNotFound")),
        ]);
        assert_eq!(forward.reason, reversed.reason);
    }

    #[test]
    fn an_unready_row_without_a_reason_still_yields_a_usable_one() {
        let agg = aggregate_ready(&[row("vc-1", false, None)]);
        assert_eq!(agg.status, condition_status::FALSE);
        assert_eq!(agg.reason, reasons::NOT_READY);
    }

    #[test]
    fn aggregation_ignores_provider_ordering() {
        // perProvider is a merge-keyed map now (ADR-0015), so the apiserver
        // gives no ordering guarantee across managers.
        let a = aggregate_ready(&[row("a", true, None), row("b", false, Some("Importing"))]);
        let b = aggregate_ready(&[row("b", false, Some("Importing")), row("a", true, None)]);
        assert_eq!(a.status, b.status);
        assert_eq!(a.reason, b.reason);
    }

    // ---------- what the controller actually writes -----------------------

    #[test]
    fn the_patch_carries_conditions_and_nothing_else() {
        // The whole point of ADR-0015: this manager owns `conditions` alone.
        // Touching perProvider or rawDiskArtifact here would re-create the
        // contention the ADR removed.
        let patch = build_status_patch("img", &aggregate_ready(&[row("a", true, None)]), 7);
        let status = patch["status"].as_object().expect("status object");
        assert!(status.contains_key("conditions"));
        assert!(
            !status.contains_key("perProvider"),
            "perProvider belongs to the providers"
        );
        assert!(
            !status.contains_key("rawDiskArtifact"),
            "rawDiskArtifact belongs to banlieue-imagebuilder"
        );
    }

    #[test]
    fn the_patch_records_the_generation_it_observed() {
        let patch = build_status_patch("img", &aggregate_ready(&[row("a", true, None)]), 7);
        assert_eq!(
            patch["status"]["conditions"][0]["observedGeneration"], 7,
            "a stale condition must be detectable"
        );
        assert_eq!(patch["status"]["conditions"][0]["type"], "Ready");
    }
}
