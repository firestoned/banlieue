// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `firecracker_machine.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn sample_nic(name: &str) -> FcNicSpec {
        FcNicSpec {
            name: name.to_string(),
            network_class: "sandbox".to_string(),
            mac_address: None,
            ipam: IpamSpec::default(),
        }
    }

    fn minimal_spec() -> FirecrackerMachineSpec {
        FirecrackerMachineSpec {
            provider_id: None,
            failure_domain: None,
            provider_ref: crate::common::ProviderReference {
                name: "fc-host-a".to_string(),
                namespace: None,
            },
            vcpus: 2,
            memory_mi_b: 512,
            image: format!("registry.internal:5000/banlieue/sandbox@{DIGEST}"),
            nics: vec![sample_nic("eth0")],
            user_data: None,
            desired_power_state: PowerState::PoweredOn,
        }
    }

    #[test]
    fn spec_round_trips_through_json() {
        let spec = minimal_spec();
        let json = serde_json::to_value(&spec).unwrap();
        let back: FirecrackerMachineSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn spec_serializes_camel_case() {
        let json = serde_json::to_value(minimal_spec()).unwrap();
        for key in [
            "providerRef",
            "vcpus",
            "memoryMiB",
            "image",
            "nics",
            "desiredPowerState",
        ] {
            assert!(json.get(key).is_some(), "missing {key}: {json}");
        }
        assert_eq!(json["nics"][0]["networkClass"], "sandbox");
    }

    /// ADR-0137 Decision 7 declares these absent: the type must not carry a
    /// field a request could set.
    #[test]
    fn declared_absences_have_no_field() {
        let json = serde_json::to_value(minimal_spec()).unwrap();
        for key in [
            "tpmEnabled",
            "devices",
            "hotplug",
            "cpus",
            "memory",
            "storageClass",
            "osDiskSizeGiB",
            "bootSource",
            "kernelArgs",
            "cmdline",
            "identity",
            "moveIn",
            "moveOut",
            "firstBootSealing",
        ] {
            assert!(json.get(key).is_none(), "{key} must not exist: {json}");
        }
    }

    #[test]
    fn provider_ref_is_shared_capable() {
        let mut spec = minimal_spec();
        spec.provider_ref.namespace = Some("platform".to_string());
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["providerRef"]["namespace"], "platform");
    }

    #[test]
    fn provider_id_uses_the_capi_spelling() {
        let mut spec = minimal_spec();
        spec.provider_id = Some("firecracker://fc-host-a/0f3c9a1e".to_string());
        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("providerID").is_some(), "{json}");
        assert!(json.get("providerId").is_none(), "{json}");
    }

    #[test]
    fn provider_id_is_scheme_provider_uid() {
        assert_eq!(
            firecracker_provider_id("fc-host-a", "0f3c9a1e-0000-4000-8000-000000000001"),
            "firecracker://fc-host-a/0f3c9a1e-0000-4000-8000-000000000001"
        );
    }

    #[test]
    fn optional_fields_default_when_absent() {
        let json = serde_json::json!({
            "providerRef": { "name": "fc-host-a" },
            "vcpus": 1,
            "memoryMiB": 256,
            "image": format!("registry.internal:5000/x@{DIGEST}"),
            "nics": [],
        });
        let s: FirecrackerMachineSpec = serde_json::from_value(json).unwrap();
        assert!(s.user_data.is_none());
        assert_eq!(s.desired_power_state, PowerState::PoweredOn);
    }

    // ------------------------------------------------------------------
    // Status
    // ------------------------------------------------------------------

    #[test]
    fn status_defaults_are_empty_and_not_provisioned() {
        let st = FirecrackerMachineStatus::default();
        assert_ne!(st.initialization.provisioned, Some(true));
        assert!(st.addresses.is_empty());
        assert!(st.conditions.is_empty());
        assert!(st.host_uid.is_none());
        assert!(st.attestation_subject.is_none());
    }

    #[test]
    fn status_omits_empty_collections_and_unobserved_fields() {
        let json = serde_json::to_value(FirecrackerMachineStatus::default()).unwrap();
        for key in [
            "addresses",
            "conditions",
            "guestInstalled",
            "hostUid",
            "attestationSubject",
            "observedPowerState",
            "tpmEndorsementCertificates",
        ] {
            assert!(json.get(key).is_none(), "{key} present: {json}");
        }
    }

    /// ADR-0143 Decision 3: the status publishes the exact subject CN.
    #[test]
    fn status_round_trips_with_attestation_subject() {
        let st = FirecrackerMachineStatus {
            initialization: InitializationStatus {
                provisioned: Some(true),
            },
            addresses: vec![MachineAddress {
                address_type: MachineAddressType::InternalIP,
                address: "192.0.2.24".to_string(),
            }],
            host_uid: Some(2_000_001),
            attestation_subject: Some("0f3c9a1e-0000-4000-8000-000000000001".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(
            json["attestationSubject"],
            "0f3c9a1e-0000-4000-8000-000000000001"
        );
        assert_eq!(json["hostUid"], 2_000_001);
        let back: FirecrackerMachineStatus = serde_json::from_value(json).unwrap();
        assert_eq!(st, back);
    }

    // ------------------------------------------------------------------
    // CRD generation
    // ------------------------------------------------------------------

    #[test]
    fn crd_has_the_expected_identity() {
        use kube::CustomResourceExt;
        let crd = FirecrackerMachine::crd();
        assert_eq!(crd.spec.group, "infrastructure.banlieue.io");
        assert_eq!(crd.spec.names.kind, "FirecrackerMachine");
        assert_eq!(crd.spec.names.plural, "firecrackermachines");
        assert_eq!(crd.spec.scope, "Namespaced");
    }

    #[test]
    fn crd_has_a_status_subresource() {
        use kube::CustomResourceExt;
        let crd = FirecrackerMachine::crd();
        let v = &crd.spec.versions[0];
        assert!(v.subresources.as_ref().is_some_and(|s| s.status.is_some()));
    }

    #[test]
    fn template_crd_has_the_expected_identity() {
        use kube::CustomResourceExt;
        let crd = FirecrackerMachineTemplate::crd();
        assert_eq!(crd.spec.names.kind, "FirecrackerMachineTemplate");
        assert_eq!(crd.spec.names.plural, "firecrackermachinetemplates");
    }

    #[test]
    fn template_wraps_a_machine_spec() {
        let t = FirecrackerMachineTemplateSpec {
            template: FirecrackerMachineTemplateResource {
                spec: minimal_spec(),
            },
        };
        let json = serde_json::to_value(&t).unwrap();
        assert!(json["template"]["spec"]["image"].is_string(), "{json}");
    }

    #[test]
    fn both_crds_are_registered_and_carry_the_capi_label() {
        let crds = crate::crdgen_support::all_crds();
        for kind in ["FirecrackerMachine", "FirecrackerMachineTemplate"] {
            let crd = crds
                .iter()
                .find(|c| c.spec.names.kind == kind)
                .unwrap_or_else(|| panic!("{kind} is not in all_crds()"));
            let labels = crd.metadata.labels.as_ref().expect("labels");
            assert_eq!(
                labels.get("cluster.x-k8s.io/v1beta2").map(String::as_str),
                Some("v1alpha1"),
                "{kind} lacks the CAPI contract label"
            );
        }
    }
}
