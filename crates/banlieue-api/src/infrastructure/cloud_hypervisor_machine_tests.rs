// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `cloud_hypervisor_machine.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    fn sample_nic(name: &str) -> ChNicSpec {
        ChNicSpec {
            name: name.to_string(),
            network_class: "default".to_string(),
            mac_address: None,
            ipam: IpamSpec::default(),
        }
    }

    fn minimal_spec() -> CloudHypervisorMachineSpec {
        CloudHypervisorMachineSpec {
            provider_id: None,
            failure_domain: None,
            provider_ref: LocalObjectReference {
                name: "ch-host-a".to_string(),
            },
            cpus: ChCpuSpec { boot: 2, max: None },
            memory: ChMemorySpec {
                size_mi_b: 4096,
                hugepages: false,
            },
            storage_class: "default".to_string(),
            boot_source: ChBootSource {
                kind: ChBootSourceKind::Image,
                image: "kairos-ubuntu-2404".to_string(),
            },
            os_disk_size_gi_b: 20,
            nics: vec![sample_nic("eth0")],
            tpm_enabled: false,
            user_data: None,
            desired_power_state: PowerState::PoweredOn,
        }
    }

    // ------------------------------------------------------------------
    // Round-tripping and wire names
    // ------------------------------------------------------------------

    #[test]
    fn spec_round_trips_through_json() {
        let spec = minimal_spec();
        let json = serde_json::to_value(&spec).unwrap();
        let back: CloudHypervisorMachineSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn spec_serializes_camel_case() {
        let json = serde_json::to_value(minimal_spec()).unwrap();
        for key in [
            "providerRef",
            "cpus",
            "memory",
            "storageClass",
            "bootSource",
            "osDiskSizeGiB",
            "nics",
            "desiredPowerState",
        ] {
            assert!(json.get(key).is_some(), "missing {key}: {json}");
        }
        assert_eq!(json["memory"]["sizeMiB"], 4096);
        assert_eq!(json["nics"][0]["networkClass"], "default");
    }

    #[test]
    fn provider_id_uses_the_capi_spelling() {
        let mut spec = minimal_spec();
        spec.provider_id = Some("cloudhypervisor://ch-host-a/0f3c9a1e".to_string());
        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("providerID").is_some(), "{json}");
        assert!(json.get("providerId").is_none(), "{json}");
    }

    /// The providerID names the Provider and the machine UID (ADR-0062
    /// Decision 2): nothing on the host outlives the VMM process, so the
    /// cluster supplies the identity.
    #[test]
    fn provider_id_is_scheme_provider_uid() {
        assert_eq!(
            cloud_hypervisor_provider_id("ch-host-a", "0f3c9a1e-0000-4000-8000-000000000001"),
            "cloudhypervisor://ch-host-a/0f3c9a1e-0000-4000-8000-000000000001"
        );
    }

    #[test]
    fn optional_fields_default_when_absent() {
        let json = serde_json::json!({
            "providerRef": { "name": "ch-host-a" },
            "cpus": { "boot": 2 },
            "memory": { "sizeMiB": 2048 },
            "storageClass": "default",
            "bootSource": { "kind": "image", "image": "kairos" },
            "osDiskSizeGiB": 20,
            "nics": [],
        });
        let s: CloudHypervisorMachineSpec = serde_json::from_value(json).unwrap();
        assert!(!s.tpm_enabled);
        assert!(!s.memory.hugepages);
        assert_eq!(s.cpus.max, None);
        assert_eq!(s.desired_power_state, PowerState::PoweredOn);
    }

    // ------------------------------------------------------------------
    // CPUs
    // ------------------------------------------------------------------

    /// With no hotplug headroom declared, the maximum is the boot count.
    #[test]
    fn max_vcpus_defaults_to_boot() {
        let c = ChCpuSpec { boot: 4, max: None };
        assert_eq!(c.max_vcpus(), 4);
    }

    #[test]
    fn max_vcpus_uses_declared_headroom() {
        let c = ChCpuSpec {
            boot: 2,
            max: Some(8),
        };
        assert_eq!(c.max_vcpus(), 8);
    }

    /// A maximum below the boot count is not headroom, it is a mistake. The
    /// VMM rejects it at create time; clamping here means the machine still
    /// boots with what it asked for.
    #[test]
    fn max_vcpus_is_never_below_boot() {
        let c = ChCpuSpec {
            boot: 4,
            max: Some(2),
        };
        assert_eq!(c.max_vcpus(), 4);
    }

    // ------------------------------------------------------------------
    // Boot source
    // ------------------------------------------------------------------

    #[test]
    fn boot_source_serializes_kind_and_image() {
        let json = serde_json::to_value(minimal_spec().boot_source).unwrap();
        assert_eq!(json["kind"], "image");
        assert_eq!(json["image"], "kairos-ubuntu-2404");
    }

    /// `InstallMedia` is ADR-0065's Deferred layout: an empty OS disk first,
    /// the installer second. `Image` clones an installed image instead.
    #[test]
    fn only_install_media_needs_an_installer_and_an_empty_disk() {
        let image = ChBootSource {
            kind: ChBootSourceKind::Image,
            image: "a".to_string(),
        };
        let media = ChBootSource {
            kind: ChBootSourceKind::InstallMedia,
            image: "a.iso".to_string(),
        };
        assert!(!image.needs_install_media());
        assert!(!image.needs_empty_os_disk());
        assert!(media.needs_install_media());
        assert!(media.needs_empty_os_disk());
    }

    #[test]
    fn install_media_round_trips() {
        let json = serde_json::json!({ "kind": "installMedia", "image": "installer.iso" });
        let b: ChBootSource = serde_json::from_value(json).unwrap();
        assert_eq!(b.kind, ChBootSourceKind::InstallMedia);
    }

    // ------------------------------------------------------------------
    // Enum wire forms — the YAML 1.1 implicit-boolean trap
    // ------------------------------------------------------------------

    /// Same rule as every other CRD here: no variant may serialize to a word
    /// Go's YAML 1.1 parser reads as a boolean.
    #[test]
    fn no_enum_variant_collides_with_a_yaml_boolean() {
        const YAML_BOOLEANS: &[&str] = &[
            "y", "n", "yes", "no", "on", "off", "true", "false", "t", "f",
        ];
        let tokens = [
            serde_json::to_value(ChBootSourceKind::Image).unwrap(),
            serde_json::to_value(ChBootSourceKind::InstallMedia).unwrap(),
            serde_json::to_value(ChAddressSource::Static).unwrap(),
            serde_json::to_value(ChAddressSource::Neighbour).unwrap(),
        ];
        for t in tokens {
            let s = t.as_str().expect("enum variants serialize as strings");
            assert!(
                !YAML_BOOLEANS.contains(&s.to_ascii_lowercase().as_str()),
                "variant {s:?} is a YAML 1.1 boolean"
            );
        }
    }

    // ------------------------------------------------------------------
    // Status
    // ------------------------------------------------------------------

    #[test]
    fn status_defaults_are_empty_and_not_provisioned() {
        let st = CloudHypervisorMachineStatus::default();
        assert_ne!(st.initialization.provisioned, Some(true));
        assert!(st.addresses.is_empty());
        assert!(st.conditions.is_empty());
        assert!(st.address_source.is_none());
        assert!(st.host_uid.is_none());
    }

    #[test]
    fn status_omits_empty_collections_and_unobserved_fields() {
        let json = serde_json::to_value(CloudHypervisorMachineStatus::default()).unwrap();
        for key in [
            "addresses",
            "conditions",
            "guestInstalled",
            "installMediaDetached",
            "tpmEndorsementCertificates",
            "hostUid",
        ] {
            assert!(json.get(key).is_none(), "{key} present: {json}");
        }
    }

    #[test]
    fn status_round_trips_with_addresses() {
        let st = CloudHypervisorMachineStatus {
            initialization: InitializationStatus {
                provisioned: Some(true),
            },
            addresses: vec![MachineAddress {
                address_type: MachineAddressType::InternalIP,
                address: "192.0.2.24".to_string(),
            }],
            address_source: Some(ChAddressSource::Neighbour),
            host_uid: Some(2_000_001),
            ..Default::default()
        };
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(json["addressSource"], "neighbour");
        assert_eq!(json["hostUid"], 2_000_001);
        let back: CloudHypervisorMachineStatus = serde_json::from_value(json).unwrap();
        assert_eq!(st, back);
    }

    // ------------------------------------------------------------------
    // CRD generation
    // ------------------------------------------------------------------

    #[test]
    fn crd_has_the_expected_identity() {
        use kube::CustomResourceExt;
        let crd = CloudHypervisorMachine::crd();
        assert_eq!(crd.spec.group, "infrastructure.banlieue.io");
        assert_eq!(crd.spec.names.kind, "CloudHypervisorMachine");
        assert_eq!(crd.spec.names.plural, "cloudhypervisormachines");
        assert_eq!(crd.spec.scope, "Namespaced");
    }

    /// The InfraMachine contract requires a status subresource.
    #[test]
    fn crd_has_a_status_subresource() {
        use kube::CustomResourceExt;
        let crd = CloudHypervisorMachine::crd();
        let v = &crd.spec.versions[0];
        assert!(v.subresources.as_ref().is_some_and(|s| s.status.is_some()));
    }

    #[test]
    fn template_crd_has_the_expected_identity() {
        use kube::CustomResourceExt;
        let crd = CloudHypervisorMachineTemplate::crd();
        assert_eq!(crd.spec.names.kind, "CloudHypervisorMachineTemplate");
        assert_eq!(crd.spec.names.plural, "cloudhypervisormachinetemplates");
    }

    #[test]
    fn template_wraps_a_machine_spec() {
        let t = CloudHypervisorMachineTemplateSpec {
            template: CloudHypervisorMachineTemplateResource {
                spec: minimal_spec(),
            },
        };
        let json = serde_json::to_value(&t).unwrap();
        assert!(json["template"]["spec"]["bootSource"].is_object(), "{json}");
    }

    /// One list feeds crdgen, the API reference and `banlieue bootstrap`. A
    /// CRD missing from it installs under GitOps and fails under bootstrap.
    #[test]
    fn both_crds_are_registered_and_carry_the_capi_label() {
        let crds = crate::crdgen_support::all_crds();
        for kind in ["CloudHypervisorMachine", "CloudHypervisorMachineTemplate"] {
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
