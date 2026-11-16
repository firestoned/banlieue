// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `virtualmachineclaim.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    /// The smallest claim the schema accepts, as JSON. Built as JSON rather
    /// than as a struct literal so the tests exercise the *wire* shape —
    /// which is what a consumer actually writes and what the apiserver
    /// validates.
    fn minimal_claim_json() -> serde_json::Value {
        serde_json::json!({
            "poolRef": { "name": "sandbox-pool" },
            "subject": {
                "issuer": "https://issuer.example.com",
                "id": "0c3b7f2e-1d4a-4b6c-9e8f-5a2d1c0b9e7f"
            },
            "ttlSeconds": 900
        })
    }

    #[test]
    fn minimal_claim_round_trips() {
        let spec: VirtualMachineClaimSpec =
            serde_json::from_value(minimal_claim_json()).expect("minimal claim must parse");
        assert_eq!(
            spec.pool_ref.as_ref().map(|r| r.name.as_str()),
            Some("sandbox-pool")
        );
        assert!(spec.pool_selector.is_none());
        assert_eq!(spec.subject.issuer, "https://issuer.example.com");
        assert_eq!(spec.ttl_seconds, 900);
    }

    // ------------------------------------------------------------------
    // ttlSeconds: required, no default (ADR-0047 Decision 5)
    // ------------------------------------------------------------------

    /// A claim without a deadline is a leaked VM waiting to happen, and
    /// there is no number banlieue could pick that is not wrong for
    /// somebody's workload. So there is no default, and omitting it is an
    /// error the apiserver reports rather than a silence.
    #[test]
    fn ttl_seconds_is_required() {
        let mut json = minimal_claim_json();
        json.as_object_mut().unwrap().remove("ttlSeconds");
        let err = serde_json::from_value::<VirtualMachineClaimSpec>(json)
            .expect_err("a claim with no TTL must not parse");
        assert!(
            err.to_string().contains("ttlSeconds"),
            "error should name the missing field, got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // poolRef / poolSelector: exactly one (ADR-0132 Decisions 1 and 2)
    // ------------------------------------------------------------------

    /// A claim may select among pools instead of naming one. `poolRef` is
    /// then absent, which the type must accept: the "exactly one" rule is
    /// the apiserver's (CEL), not serde's.
    #[test]
    fn a_claim_may_select_its_pool_by_label() {
        let mut json = minimal_claim_json();
        let obj = json.as_object_mut().unwrap();
        obj.remove("poolRef");
        obj.insert(
            "poolSelector".into(),
            serde_json::json!({ "matchLabels": { "sandbox.example.com/image": "agent" } }),
        );
        let spec: VirtualMachineClaimSpec =
            serde_json::from_value(json).expect("a selector claim must parse");
        assert!(spec.pool_ref.is_none());
        let selector = spec.pool_selector.expect("selector kept");
        assert_eq!(
            selector
                .match_labels
                .get("sandbox.example.com/image")
                .map(String::as_str),
            Some("agent")
        );
    }

    /// `poolSelector: {}` is how a claim says "any pool in my namespace".
    /// It must survive a round trip as an empty selector, not vanish into
    /// `None`: an absent selector and an empty one mean opposite things.
    #[test]
    fn an_empty_selector_is_kept_rather_than_dropped() {
        let mut json = minimal_claim_json();
        let obj = json.as_object_mut().unwrap();
        obj.remove("poolRef");
        obj.insert("poolSelector".into(), serde_json::json!({}));
        let spec: VirtualMachineClaimSpec = serde_json::from_value(json).expect("parses");
        assert_eq!(
            spec.pool_selector,
            Some(crate::common::LabelSelector::default())
        );

        let back = serde_json::to_value(&spec).unwrap();
        assert_eq!(back["poolSelector"], serde_json::json!({}), "got {back}");
        assert!(back.get("poolRef").is_none(), "got {back}");
    }

    fn spec_schema() -> serde_json::Value {
        use kube::CustomResourceExt as _;
        let crd = VirtualMachineClaim::crd();
        let schema = crd.spec.versions[0].schema.clone().unwrap();
        let root = serde_json::to_value(schema.open_api_v3_schema.unwrap()).unwrap();
        root["properties"]["spec"].clone()
    }

    /// Neither and both are refused by the apiserver, before any controller
    /// sees the claim. Neither must never be read as "any pool": that is
    /// spelled `poolSelector: {}` (ADR-0132 Decision 2).
    #[test]
    fn a_claim_takes_exactly_one_of_pool_ref_or_pool_selector() {
        let spec = spec_schema();
        let rules = spec["x-kubernetes-validations"]
            .as_array()
            .unwrap_or_else(|| panic!("no CEL rules on spec: {spec}"));
        assert!(
            rules
                .iter()
                .any(|r| r["rule"] == "has(self.poolRef) != has(self.poolSelector)"),
            "got {spec}"
        );
        let required = spec["required"].as_array().cloned().unwrap_or_default();
        assert!(
            !required.iter().any(|f| f == "poolRef"),
            "poolRef must no longer be required: {spec}"
        );
    }

    #[test]
    fn subject_is_required() {
        let mut json = minimal_claim_json();
        json.as_object_mut().unwrap().remove("subject");
        serde_json::from_value::<VirtualMachineClaimSpec>(json)
            .expect_err("a claim with no subject must not parse");
    }

    /// Both halves identify the subject; neither is optional. An issuer with
    /// no id names nobody, and an id with no issuer is ambiguous across
    /// identity providers — either one alone makes the audit trail useless.
    #[test]
    fn both_halves_of_subject_are_required() {
        for missing in ["issuer", "id"] {
            let mut json = minimal_claim_json();
            json["subject"].as_object_mut().unwrap().remove(missing);
            assert!(
                serde_json::from_value::<VirtualMachineClaimSpec>(json).is_err(),
                "a subject without {missing} must not parse"
            );
        }
    }

    // ------------------------------------------------------------------
    // Phase
    // ------------------------------------------------------------------

    /// A claim starts Pending, not Bound: it has no member until the
    /// reconciler finds one, and a status that claimed otherwise would let a
    /// consumer read `Bound` with no `virtualMachineRef`.
    #[test]
    fn phase_defaults_to_pending() {
        let st = VirtualMachineClaimStatus::default();
        assert_eq!(st.phase, ClaimPhase::Pending);
        assert!(st.virtual_machine_ref.is_none());
        assert!(st.nonce.is_none());
        assert!(st.bound_at.is_none());
        assert!(st.expires_at.is_none());
    }

    /// Go's YAML 1.1 parser (the apiserver's) reads bare `on`/`off`/`yes`/
    /// `no`/`y`/`n`/`true`/`false` in any case as booleans and rejects the
    /// CRD outright. Every variant that reaches an `enum:` in the schema
    /// must land outside that set.
    #[test]
    fn no_phase_variant_collides_with_a_yaml_boolean() {
        const YAML_BOOLEANS: &[&str] = &[
            "y", "n", "yes", "no", "on", "off", "true", "false", "t", "f",
        ];
        for v in [
            ClaimPhase::Pending,
            ClaimPhase::Bound,
            ClaimPhase::Releasing,
            ClaimPhase::Failed,
        ] {
            let token = serde_json::to_value(v).unwrap();
            let s = token.as_str().expect("variants serialize as strings");
            assert!(
                !YAML_BOOLEANS.contains(&s.to_ascii_lowercase().as_str()),
                "variant {s:?} is a YAML 1.1 boolean; the apiserver will reject the CRD"
            );
        }
    }

    // ------------------------------------------------------------------
    // Status wire shape
    // ------------------------------------------------------------------

    /// An empty status must serialize to just the phase. A consumer polls
    /// this object; emitting `"addresses": []` and a run of nulls on every
    /// read is noise that makes a real binding harder to see.
    #[test]
    fn empty_status_serializes_to_phase_only() {
        let json = serde_json::to_value(VirtualMachineClaimStatus::default()).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj.len(), 1, "expected phase only, got {json}");
        assert_eq!(obj["phase"], "Pending");
    }

    #[test]
    fn status_fields_are_camel_case_on_the_wire() {
        let st = VirtualMachineClaimStatus {
            phase: ClaimPhase::Bound,
            virtual_machine_ref: Some(LocalObjectReference {
                name: "sandbox-pool-9dmtx".into(),
            }),
            tpm_endorsement_certificates: vec!["-----BEGIN CERTIFICATE-----".into()],
            nonce: Some("0123456789abcdef0123456789abcdef".into()),
            ..Default::default()
        };
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(json["phase"], "Bound");
        assert_eq!(json["virtualMachineRef"]["name"], "sandbox-pool-9dmtx");
        assert!(json["tpmEndorsementCertificates"].is_array());
        assert!(json.get("virtual_machine_ref").is_none());
    }

    // ------------------------------------------------------------------
    // The finalizer contract (ADR-0047 Decision 6)
    // ------------------------------------------------------------------

    /// "Claim deleted" must mean "sandbox destroyed", which only holds if
    /// the claim object outlives its member. The finalizer is the mechanism,
    /// and its name is API: changing it strands every claim already carrying
    /// the old one, because nothing is left to remove it.
    #[test]
    fn claim_finalizer_is_a_stable_qualified_name() {
        assert_eq!(CLAIM_FINALIZER, "banlieue.io/claim-protection");
    }

    /// The subject annotations are read by operators and by anything
    /// auditing "who had this VM", so their names are API too. They share
    /// the `claim-` prefix with `banlieue.io/claim` deliberately: a member
    /// carries several `banlieue.io/*` keys and these two belong together.
    #[test]
    fn subject_annotations_are_grouped_under_the_claim_prefix() {
        assert_eq!(
            ANNOTATION_SUBJECT_ISSUER,
            "banlieue.io/claim-subject-issuer"
        );
        assert_eq!(ANNOTATION_SUBJECT_ID, "banlieue.io/claim-subject-id");
    }

    // ------------------------------------------------------------------
    // Printer columns
    // ------------------------------------------------------------------

    /// `EXPIRES` must not be a `date` column.
    ///
    /// kubectl renders `type: date` as *time since* the timestamp, which is
    /// right for `creationTimestamp` and wrong for a deadline: a future
    /// value gives a negative duration and prints literally `<invalid>`.
    /// Observed on a real cluster — the one column whose whole job is to say
    /// when the sandbox dies was the one showing nothing.
    /// With a selector the spec no longer says which pool supplied the VM,
    /// so the column reads the pool the claim actually bound from
    /// (ADR-0132 Decision 6).
    #[test]
    fn the_pool_column_shows_the_pool_bound_from() {
        use kube::CustomResourceExt as _;
        let crd = VirtualMachineClaim::crd();
        let columns = crd.spec.versions[0]
            .additional_printer_columns
            .as_ref()
            .expect("printer columns");
        let pool = columns
            .iter()
            .find(|c| c.name == "Pool")
            .expect("a Pool column");
        assert_eq!(pool.json_path, ".status.poolRef.name");
    }

    #[test]
    fn status_pool_ref_round_trips_and_is_omitted_when_unset() {
        let st = VirtualMachineClaimStatus {
            pool_ref: Some(LocalObjectReference {
                name: "sandbox-pool".into(),
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(json["poolRef"]["name"], "sandbox-pool");
        let empty = serde_json::to_value(VirtualMachineClaimStatus::default()).unwrap();
        assert!(empty.get("poolRef").is_none(), "got {empty}");
    }

    #[test]
    fn the_expires_column_is_not_rendered_as_an_age() {
        use kube::CustomResourceExt as _;
        let crd = VirtualMachineClaim::crd();
        let columns = crd.spec.versions[0]
            .additional_printer_columns
            .as_ref()
            .expect("printer columns");

        let expires = columns
            .iter()
            .find(|c| c.name == "Expires")
            .expect("an Expires column");
        assert_eq!(
            expires.type_, "string",
            "a future deadline rendered as a date prints <invalid>"
        );

        // The contrast that makes the rule memorable: Age *is* a date, and
        // correctly so — it looks backwards.
        let age = columns
            .iter()
            .find(|c| c.name == "Age")
            .expect("an Age column");
        assert_eq!(age.type_, "date");
    }
}
