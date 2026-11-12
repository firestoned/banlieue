// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `virtualmachinemove.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::banlieue::{FEATURE_MOVE, LABEL_ZONE, RAW_CPU_MODEL, RAW_VMM};
    use kube::{CustomResourceExt, Resource};

    /// The smallest move the schema accepts, as JSON: the wire shape a
    /// requester (faubourg, or a person) actually writes.
    fn minimal_move_json() -> serde_json::Value {
        serde_json::json!({ "vmRef": { "name": "web-0" } })
    }

    // ------------------------------------------------------------------
    // Spec
    // ------------------------------------------------------------------

    /// A move names its VM and nothing else: with no target, banlieue picks
    /// the destination (ADR-0127 Decision 2).
    #[test]
    fn a_minimal_move_names_only_its_vm() {
        let spec: VirtualMachineMoveSpec =
            serde_json::from_value(minimal_move_json()).expect("minimal move must parse");
        assert_eq!(spec.vm_ref.name, "web-0");
        assert!(spec.target.is_none());
        assert_eq!(serde_json::to_value(&spec).unwrap(), minimal_move_json());
    }

    #[test]
    fn vm_ref_is_required() {
        serde_json::from_value::<VirtualMachineMoveSpec>(serde_json::json!({}))
            .expect_err("a move with no vmRef must not parse");
    }

    #[test]
    fn a_target_names_a_provider_or_selects_one() {
        let named: VirtualMachineMoveSpec = serde_json::from_value(serde_json::json!({
            "vmRef": { "name": "web-0" },
            "target": { "providerName": "hv-b" }
        }))
        .unwrap();
        let target = named.target.unwrap();
        assert_eq!(target.provider_name.as_deref(), Some("hv-b"));
        assert!(target.provider_selector.is_none());

        let selected: VirtualMachineMoveSpec = serde_json::from_value(serde_json::json!({
            "vmRef": { "name": "web-0" },
            "target": { "providerSelector": { "matchLabels": { "rack": "r2" } } }
        }))
        .unwrap();
        let target = selected.target.unwrap();
        assert!(target.provider_name.is_none());
        assert!(target.provider_selector.is_some());
    }

    // ------------------------------------------------------------------
    // Phase
    // ------------------------------------------------------------------

    /// A move starts Pending. With the API served and no controller acting on
    /// it (ADR-0127 Decision 13, API first), Pending is also where it stays.
    #[test]
    fn phase_defaults_to_pending() {
        let st = VirtualMachineMoveStatus::default();
        assert_eq!(st.phase, MovePhase::Pending);
        assert!(st.source.is_none());
        assert!(st.destination.is_none());
        assert!(st.downtime_milliseconds.is_none());
    }

    const ALL_PHASES: [MovePhase; 7] = [
        MovePhase::Pending,
        MovePhase::Preparing,
        MovePhase::Copying,
        MovePhase::HandingOver,
        MovePhase::Succeeded,
        MovePhase::Failed,
        MovePhase::Rejected,
    ];

    /// Deleting a move cancels it only before `HandingOver` (Decision 2);
    /// from then on it runs to `Succeeded` or `Failed`. faubourg relies on
    /// this to know which moves it may delete when a drain is cleared.
    #[test]
    fn only_a_move_before_handover_is_cancellable() {
        let cancellable: Vec<_> = ALL_PHASES.iter().filter(|p| p.is_cancellable()).collect();
        assert_eq!(
            cancellable,
            [
                &MovePhase::Pending,
                &MovePhase::Preparing,
                &MovePhase::Copying
            ]
        );
    }

    #[test]
    fn succeeded_failed_and_rejected_are_terminal() {
        let terminal: Vec<_> = ALL_PHASES.iter().filter(|p| p.is_terminal()).collect();
        assert_eq!(
            terminal,
            [
                &MovePhase::Succeeded,
                &MovePhase::Failed,
                &MovePhase::Rejected
            ]
        );
        assert!(
            ALL_PHASES
                .iter()
                .all(|p| !(p.is_terminal() && p.is_cancellable())),
            "a finished move has nothing left to cancel"
        );
    }

    /// The apiserver's YAML 1.1 parser reads bare `on`, `no`, `y` and the like
    /// as booleans and rejects the CRD; no variant may be one.
    #[test]
    fn no_phase_variant_collides_with_a_yaml_boolean() {
        const YAML_BOOLEANS: &[&str] = &[
            "y", "n", "yes", "no", "on", "off", "true", "false", "t", "f",
        ];
        for v in ALL_PHASES {
            let token = serde_json::to_value(v).unwrap();
            let s = token.as_str().expect("variants serialize as strings");
            assert!(
                !YAML_BOOLEANS.contains(&s.to_ascii_lowercase().as_str()),
                "variant {s:?} is a YAML 1.1 boolean"
            );
        }
    }

    // ------------------------------------------------------------------
    // Status wire shape
    // ------------------------------------------------------------------

    #[test]
    fn empty_status_serializes_to_phase_only() {
        let json = serde_json::to_value(VirtualMachineMoveStatus::default()).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj.len(), 1, "expected phase only, got {json}");
        assert_eq!(obj["phase"], "Pending");
    }

    #[test]
    fn a_finished_move_round_trips() {
        let json = serde_json::json!({
            "phase": "Succeeded",
            "source": "hv-a",
            "destination": "hv-b",
            "bytesTotal": 4_294_967_296_u64,
            "bytesSent": 4_294_967_296_u64,
            "downtimeMilliseconds": 3900,
            "conditions": [],
        });
        let st: VirtualMachineMoveStatus = serde_json::from_value(json).unwrap();
        assert_eq!(st.phase, MovePhase::Succeeded);
        assert_eq!(st.destination.as_deref(), Some("hv-b"));
        assert_eq!(st.downtime_milliseconds, Some(3900));
        let back = serde_json::to_value(&st).unwrap();
        assert_eq!(back["bytesSent"], 4_294_967_296_u64);
    }

    /// The rejection reasons ADR-0127 names are API: faubourg reports them to
    /// an operator (faubourg ADR-0017 Decision 4), so they never change.
    #[test]
    fn rejection_reasons_are_stable_strings() {
        assert_eq!(move_condition_types::ADMITTED, "Admitted");
        assert_eq!(move_reasons::MOVE_IN_PROGRESS, "MoveInProgress");
        assert_eq!(move_reasons::VTPM_NOT_MOVABLE, "VTPMNotMovable");
        assert_eq!(
            move_reasons::STORAGE_WITHOUT_REFLINK,
            "StorageWithoutReflink"
        );
        assert_eq!(move_reasons::MIGRATION_POLICY_NEVER, "MigrationPolicyNever");
        assert_eq!(move_reasons::NOT_IN_A_ZONE, "NotInAZone");
        assert_eq!(move_reasons::HOSTS_DIFFER, "HostsDiffer");
        assert_eq!(move_reasons::NO_DESTINATION, "NoDestination");
    }

    // ------------------------------------------------------------------
    // Keys a requester and a host provider share
    // ------------------------------------------------------------------

    #[test]
    fn the_zone_label_feature_and_raw_keys_are_api() {
        assert_eq!(LABEL_ZONE, "banlieue.io/zone");
        assert_eq!(FEATURE_MOVE, "move");
        assert_eq!(RAW_VMM, "vmm");
        assert_eq!(RAW_CPU_MODEL, "cpuModel");
    }

    // ------------------------------------------------------------------
    // CRD
    // ------------------------------------------------------------------

    #[test]
    fn crd_has_the_expected_identity() {
        let crd = VirtualMachineMove::crd();
        assert_eq!(crd.spec.group, "banlieue.io");
        assert_eq!(crd.spec.names.kind, "VirtualMachineMove");
        assert_eq!(crd.spec.names.plural, "virtualmachinemoves");
        assert_eq!(crd.spec.scope, "Namespaced");
        assert_eq!(VirtualMachineMove::api_version(&()), "banlieue.io/v1alpha1");
        let v = &crd.spec.versions[0];
        assert!(v.subresources.as_ref().unwrap().status.is_some());
    }

    fn spec_schema() -> serde_json::Value {
        let crd = VirtualMachineMove::crd();
        let schema = crd.spec.versions[0].schema.clone().unwrap();
        let root = serde_json::to_value(schema.open_api_v3_schema.unwrap()).unwrap();
        root["properties"]["spec"].clone()
    }

    /// A move is about one VM for its whole life. Retargeting a move in
    /// flight would leave two half-moved machines and a status describing
    /// neither, so the apiserver refuses the change (Decision 2).
    #[test]
    fn vm_ref_is_immutable_in_the_crd() {
        let spec = spec_schema();
        let rules = &spec["properties"]["vmRef"]["x-kubernetes-validations"];
        assert_eq!(rules[0]["rule"], "self == oldSelf", "got {spec}");
    }

    /// A target names a provider or selects one, never both and never
    /// neither: the apiserver rejects anything else rather than banlieue
    /// guessing which one was meant.
    #[test]
    fn target_takes_exactly_one_of_name_or_selector() {
        let spec = spec_schema();
        let rules = &spec["properties"]["target"]["x-kubernetes-validations"];
        assert_eq!(
            rules[0]["rule"], "has(self.providerName) != has(self.providerSelector)",
            "got {spec}"
        );
    }

    #[test]
    fn the_crd_is_registered_for_generation() {
        let kinds: Vec<_> = crate::crdgen_support::all_crds()
            .into_iter()
            .map(|c| c.spec.names.kind)
            .collect();
        assert!(kinds.iter().any(|k| k == "VirtualMachineMove"), "{kinds:?}");
    }
}
