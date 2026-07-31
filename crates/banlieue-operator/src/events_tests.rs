// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `events.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::reconciler::provider::SkipReason;
    use kube::runtime::events::EventType;

    /// Reasons are matched by `kubectl get events --field-selector`, by
    /// dashboards, and by alert rules. They must be a closed set of stable
    /// CamelCase identifiers — never free-form prose, and never drifting.
    #[test]
    fn every_reason_is_a_stable_camel_case_identifier() {
        for reason in [
            reasons::WORKLOAD_APPLIED,
            reasons::WORKLOAD_PRUNED,
            reasons::WORKLOAD_DELETED,
            reasons::RECONCILE_SKIPPED,
            reasons::CLASS_NOT_FOUND,
        ] {
            assert!(!reason.is_empty());
            assert!(
                !reason.contains(' ') && !reason.contains('-'),
                "{reason} must be a single CamelCase identifier"
            );
            assert!(
                reason.chars().next().is_some_and(char::is_uppercase),
                "{reason} must start uppercase"
            );
        }
    }

    #[test]
    fn applying_a_workload_is_a_normal_event_naming_where_it_landed() {
        let event = workload_applied("banlieue-provider-vsphere-prod-vc", "banlieue-system");
        assert_eq!(event.type_, EventType::Normal);
        assert_eq!(event.reason, reasons::WORKLOAD_APPLIED);

        let note = event.note.expect("a note");
        assert!(note.contains("banlieue-provider-vsphere-prod-vc"), "{note}");
        assert!(note.contains("banlieue-system"), "{note}");
    }

    /// Pruning deletes a running workload that still holds credentials, so it is
    /// a Warning: an operator who did not expect it should see it stand out.
    #[test]
    fn pruning_a_superseded_workload_is_a_warning() {
        let event = workload_pruned("banlieue-provider-vsphere-old-vc");
        assert_eq!(event.type_, EventType::Warning);
        assert_eq!(event.reason, reasons::WORKLOAD_PRUNED);
        assert!(
            event
                .note
                .as_ref()
                .is_some_and(|n| n.contains("banlieue-provider-vsphere-old-vc")),
            "the note must name the object removed: {:?}",
            event.note
        );
    }

    /// A missing class means the Provider can never come up, so it is a Warning
    /// naming the class to create — the single most common misconfiguration.
    #[test]
    fn a_missing_class_is_a_warning_naming_the_class() {
        let event = class_not_found("vsphere");
        assert_eq!(event.type_, EventType::Warning);
        assert_eq!(event.reason, reasons::CLASS_NOT_FOUND);
        assert!(
            event.note.as_ref().is_some_and(|n| n.contains("vsphere")),
            "{:?}",
            event.note
        );
    }

    /// Pausing is intentional, so it is Normal — but the note must say WHICH
    /// object was paused, since a paused class and a paused Provider look
    /// identical from the Provider's side.
    #[test]
    fn skipping_names_the_object_that_caused_it() {
        let by_provider = reconcile_skipped(SkipReason::ProviderPaused);
        assert_eq!(by_provider.type_, EventType::Normal);
        assert_eq!(by_provider.reason, reasons::RECONCILE_SKIPPED);
        assert!(
            by_provider
                .note
                .as_ref()
                .is_some_and(|n| n.contains("Provider")),
            "{:?}",
            by_provider.note
        );

        let by_class = reconcile_skipped(SkipReason::ClassPaused);
        assert!(
            by_class
                .note
                .as_ref()
                .is_some_and(|n| n.contains("ProviderClass")),
            "a paused class must be distinguishable from a paused Provider: {:?}",
            by_class.note
        );
        assert_ne!(by_provider.note, by_class.note);
    }

    /// `action` is a required field on a Kubernetes Event and shows in
    /// `kubectl describe`; an empty one renders as a blank column.
    #[test]
    fn every_event_sets_an_action() {
        for event in [
            workload_applied("w", "ns"),
            workload_pruned("w"),
            workload_deleted("w"),
            reconcile_skipped(SkipReason::ProviderPaused),
            class_not_found("c"),
        ] {
            assert!(!event.action.is_empty(), "{} has no action", event.reason);
        }
    }
}
