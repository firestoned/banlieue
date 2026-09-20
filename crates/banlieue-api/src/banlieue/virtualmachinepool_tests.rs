// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `virtualmachinepool.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    /// The smallest pool the schema accepts, as JSON. Built as JSON rather
    /// than as a struct literal so the tests exercise the *wire* shape —
    /// which is what an operator actually writes and what the apiserver
    /// validates.
    fn minimal_pool_json() -> serde_json::Value {
        serde_json::json!({
            "warmReplicas": 4,
            "maxReplicas": 20,
            "readiness": "InfrastructureReady",
            "template": {
                "spec": {
                    "classRef": { "name": "sandbox" },
                    "imageRef": { "name": "kairos" }
                }
            }
        })
    }

    // ------------------------------------------------------------------
    // readiness: required, no default (ADR-0046 Decision 2)
    // ------------------------------------------------------------------

    /// The decision this CRD exists to encode. Roadmap 70 wanted
    /// `GuestReady` as the default, but nothing publishes that condition
    /// until ADR-0043 lands — so a defaulted pool would sit at zero warm
    /// members forever, reporting no error at all. A field whose wrong value
    /// produces silence rather than a failure has to be stated out loud.
    #[test]
    fn readiness_is_required() {
        let mut json = minimal_pool_json();
        json.as_object_mut().unwrap().remove("readiness");
        let err = serde_json::from_value::<VirtualMachinePoolSpec>(json)
            .expect_err("a pool without readiness must not deserialize");
        assert!(err.to_string().contains("readiness"), "{err}");
    }

    #[test]
    fn readiness_round_trips_both_values() {
        for (wire, want) in [
            ("GuestReady", PoolReadiness::GuestReady),
            ("InfrastructureReady", PoolReadiness::InfrastructureReady),
        ] {
            let mut json = minimal_pool_json();
            json["readiness"] = serde_json::Value::String(wire.to_string());
            let spec: VirtualMachinePoolSpec = serde_json::from_value(json).unwrap();
            assert_eq!(spec.readiness, want);
            assert_eq!(serde_json::to_value(spec.readiness).unwrap(), wire);
        }
    }

    /// The wire values are the Kubernetes **condition type names** they
    /// select — `GuestReady` picks the `GuestReady` condition. So they stay
    /// PascalCase rather than being camelCased like most enums in this
    /// crate: a `readiness: guestReady` that selects a condition called
    /// `GuestReady` is a mismatch an operator has to hold in their head.
    #[test]
    fn readiness_values_are_the_condition_type_names() {
        assert_eq!(
            serde_json::to_value(PoolReadiness::InfrastructureReady).unwrap(),
            crate::common::condition_types::INFRASTRUCTURE_READY
        );
        // GuestReady has no constant yet — ADR-0043 adds it. When it does,
        // this is where the two must be tied together.
        assert_eq!(
            serde_json::to_value(PoolReadiness::GuestReady).unwrap(),
            "GuestReady"
        );
    }

    // ------------------------------------------------------------------
    // Defaults that *are* safe to have
    // ------------------------------------------------------------------

    /// `maxSurge` and `provisioningTimeoutSeconds` default because a wrong
    /// value there is visible: the pool fills slowly, or reaps late. Neither
    /// is silent the way a wrong `readiness` is.
    #[test]
    fn sizing_and_timeout_defaults_are_applied() {
        let spec: VirtualMachinePoolSpec = serde_json::from_value(minimal_pool_json()).unwrap();
        assert_eq!(spec.max_surge, DEFAULT_POOL_MAX_SURGE);
        assert_eq!(
            spec.provisioning_timeout_seconds,
            DEFAULT_POOL_PROVISIONING_TIMEOUT_SECS
        );
    }

    /// Generous on purpose: a Deferred member is not Ready until a full
    /// unattended install plus a reboot have finished (ADR-0040). A short
    /// timeout would reap healthy members mid-install.
    #[test]
    fn provisioning_timeout_default_allows_a_full_install() {
        /// Half an hour. Lowering this reaps healthy members mid-install:
        /// a Deferred member is not Ready until a full unattended install
        /// plus a reboot have finished (ADR-0040), and a reaped member is
        /// replaced by another that will be reaped the same way.
        const EXPECTED_SECS: u64 = 1800;
        assert_eq!(DEFAULT_POOL_PROVISIONING_TIMEOUT_SECS, EXPECTED_SECS);
    }

    #[test]
    fn addressing_and_idle_expiry_default_to_absent() {
        let spec: VirtualMachinePoolSpec = serde_json::from_value(minimal_pool_json()).unwrap();
        assert!(spec.addressing.is_none(), "no addressing means DHCP");
        assert!(spec.max_idle_seconds.is_none());
    }

    /// Image rollout defaults **on**, unlike most booleans in this crate.
    /// A warm member is an already-installed VM: if it does not follow its
    /// `VMImage`, a pool rebuilt nightly for security patches keeps handing
    /// out the unpatched build indefinitely, and nothing says so. Opting out
    /// is a deliberate act; opting in should not have to be.
    #[test]
    fn image_rollout_defaults_on() {
        let spec: VirtualMachinePoolSpec = serde_json::from_value(minimal_pool_json()).unwrap();
        assert!(
            spec.recycle_on_image_change,
            "a pool that ignores image rebuilds serves stale VMs silently"
        );
    }

    // ------------------------------------------------------------------
    // Wire shape
    // ------------------------------------------------------------------

    #[test]
    fn spec_serializes_camel_case() {
        let spec: VirtualMachinePoolSpec = serde_json::from_value(minimal_pool_json()).unwrap();
        let json = serde_json::to_value(&spec).unwrap();
        let obj = json.as_object().unwrap();
        for key in ["warmReplicas", "maxReplicas", "maxSurge", "readiness"] {
            assert!(obj.contains_key(key), "missing {key}: {json}");
        }
        assert!(!obj.contains_key("warm_replicas"), "snake_case leaked");
    }

    #[test]
    fn addressing_round_trips() {
        let mut json = minimal_pool_json();
        json["addressing"] = serde_json::json!({
            "interface": "eth0",
            "rangeStart": "192.0.2.10",
            "rangeEnd": "192.0.2.69",
            "prefix": 24,
            "gateway": "192.0.2.1",
        });
        let spec: VirtualMachinePoolSpec = serde_json::from_value(json).unwrap();
        let a = spec.addressing.as_ref().expect("addressing");
        assert_eq!(a.interface, "eth0");
        assert_eq!(a.range_start, "192.0.2.10");
        assert_eq!(a.range_end, "192.0.2.69");
    }

    // ------------------------------------------------------------------
    // The YAML 1.1 implicit-boolean trap
    // ------------------------------------------------------------------

    /// Go's YAML 1.1 parser (the apiserver's) reads bare `on`/`off`/`yes`/
    /// `no`/`y`/`n`/`true`/`false` in any case as booleans and rejects the
    /// CRD outright. Every variant that reaches an `enum:` in the schema
    /// must land outside that set.
    #[test]
    fn no_readiness_variant_collides_with_a_yaml_boolean() {
        const YAML_BOOLEANS: &[&str] = &[
            "y", "n", "yes", "no", "on", "off", "true", "false", "t", "f",
        ];
        for v in [
            PoolReadiness::GuestReady,
            PoolReadiness::InfrastructureReady,
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
    // Status
    // ------------------------------------------------------------------

    #[test]
    fn status_defaults_to_an_empty_pool() {
        let st = VirtualMachinePoolStatus::default();
        assert_eq!(st.replicas, 0);
        assert_eq!(st.available, 0);
        assert_eq!(st.provisioning, 0);
        assert_eq!(st.claimed, 0);
        assert!(st.conditions.is_empty());
        assert!(st.image_revision.is_none());
    }

    /// An untouched status must not write `conditions: []` into every object.
    #[test]
    fn status_omits_empty_collections() {
        let json = serde_json::to_value(VirtualMachinePoolStatus::default()).unwrap();
        assert!(json.get("conditions").is_none(), "{json}");
        assert!(json.get("imageRevision").is_none(), "{json}");
    }

    // ------------------------------------------------------------------
    // Condition vocabulary
    // ------------------------------------------------------------------

    /// The reason a pool reports when the chosen readiness condition has
    /// never been seen on any member (ADR-0046 Decision 3). Without it,
    /// decision 2 is merely strict; with it, a stuck pool explains itself.
    #[test]
    fn a_reason_exists_for_an_absent_readiness_signal() {
        assert_eq!(
            pool_condition_reasons::READINESS_SIGNAL_ABSENT,
            "ReadinessSignalAbsent"
        );
    }

    #[test]
    fn condition_types_are_stable_strings() {
        assert_eq!(pool_condition_types::WARM, "Warm");
        assert_eq!(pool_condition_types::CAPACITY, "Capacity");
    }

    // ------------------------------------------------------------------
    // CRD generation
    // ------------------------------------------------------------------

    #[test]
    fn crd_has_the_expected_identity() {
        use kube::CustomResourceExt;
        let crd = VirtualMachinePool::crd();
        assert_eq!(crd.spec.group, "banlieue.io");
        assert_eq!(crd.spec.names.kind, "VirtualMachinePool");
        assert_eq!(crd.spec.names.plural, "virtualmachinepools");
        assert_eq!(crd.spec.scope, "Namespaced");
    }

    /// The pool owns its status; the reconciler must be able to patch it
    /// without touching spec.
    #[test]
    fn crd_has_a_status_subresource() {
        use kube::CustomResourceExt;
        let crd = VirtualMachinePool::crd();
        let v = &crd.spec.versions[0];
        assert!(
            v.subresources.as_ref().is_some_and(|s| s.status.is_some()),
            "VirtualMachinePool needs a status subresource"
        );
    }

    /// `readiness` must be required in the generated schema too, not just in
    /// serde — the apiserver is what most operators will hit first.
    #[test]
    fn generated_schema_marks_readiness_required() {
        use kube::CustomResourceExt;
        let crd = VirtualMachinePool::crd();
        let schema = crd.spec.versions[0]
            .schema
            .as_ref()
            .and_then(|s| s.open_api_v3_schema.as_ref())
            .expect("schema");
        let required = schema
            .properties
            .as_ref()
            .and_then(|p| p.get("spec"))
            .and_then(|s| s.required.as_ref())
            .expect("spec.required");
        assert!(
            required.iter().any(|r| r == "readiness"),
            "readiness must be required in the CRD schema; got {required:?}"
        );
    }
}
