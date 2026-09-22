// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `libvirt_machine.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    fn sample_disk(name: &str, size: u32) -> LibvirtDiskSpec {
        LibvirtDiskSpec {
            name: name.to_string(),
            size_gi_b: size,
            bus: LibvirtDiskBus::Virtio,
        }
    }

    fn sample_nic(name: &str) -> LibvirtNicSpec {
        LibvirtNicSpec {
            name: name.to_string(),
            source: LibvirtNicSource {
                kind: LibvirtNicSourceKind::Network,
                name: "default".to_string(),
            },
            model: None,
            mac_address: None,
            ipam: IpamSpec::default(),
        }
    }

    fn minimal_spec() -> LibvirtMachineSpec {
        LibvirtMachineSpec {
            provider_id: None,
            failure_domain: None,
            provider_ref: LocalObjectReference {
                name: "libvirt-host-a".to_string(),
            },
            pool: "default".to_string(),
            domain_name: "db-prod-01".to_string(),
            boot_source: LibvirtBootSource {
                kind: LibvirtBootSourceKind::BackingVolume,
                volume: "ubuntu-22.04.qcow2".to_string(),
            },
            vcpus: 4,
            memory_mi_b: 8192,
            firmware: Firmware::Efi,
            machine_type: None,
            tpm_enabled: false,
            disks: vec![sample_disk("os", 40)],
            network: vec![sample_nic("eth0")],
            user_data: None,
            desired_power_state: PowerState::PoweredOn,
        }
    }

    // ------------------------------------------------------------------
    // Round-tripping
    // ------------------------------------------------------------------

    #[test]
    fn spec_round_trips_through_json() {
        let spec = minimal_spec();
        let json = serde_json::to_value(&spec).unwrap();
        let back: LibvirtMachineSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec, back);
    }

    /// Kubernetes field names are camelCase. A snake_case field in a CRD
    /// schema is a field users cannot set.
    #[test]
    fn spec_serializes_camel_case() {
        let json = serde_json::to_value(minimal_spec()).unwrap();
        let obj = json.as_object().unwrap();
        for key in [
            "providerRef",
            "domainName",
            "bootSource",
            "memoryMiB",
            "desiredPowerState",
        ] {
            assert!(obj.contains_key(key), "missing camelCase key {key}: {json}");
        }
        assert!(
            !obj.contains_key("domain_name"),
            "snake_case leaked: {json}"
        );
    }

    /// `providerID` keeps its Kubernetes spelling, not serde's `providerId`.
    /// This is the CAPI contract field name, so a rename here silently breaks
    /// InfraMachine compliance.
    #[test]
    fn provider_id_uses_the_capi_spelling() {
        let mut spec = minimal_spec();
        spec.provider_id = Some("libvirt://libvirt-host-a/0f3c-...".to_string());
        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("providerID").is_some(), "{json}");
        assert!(json.get("providerId").is_none(), "{json}");
    }

    // ------------------------------------------------------------------
    // Boot source — the Immediate / Deferred split (ADR-0050 Decision 6)
    // ------------------------------------------------------------------

    #[test]
    fn boot_source_serializes_kind_and_volume() {
        let json = serde_json::to_value(LibvirtBootSource {
            kind: LibvirtBootSourceKind::BackingVolume,
            volume: "base.qcow2".to_string(),
        })
        .unwrap();
        assert_eq!(json["kind"], "backingVolume");
        assert_eq!(json["volume"], "base.qcow2");
    }

    #[test]
    fn boot_source_install_media_round_trips() {
        let src = LibvirtBootSource {
            kind: LibvirtBootSourceKind::InstallMedia,
            volume: "kairos-installer.iso".to_string(),
        };
        let json = serde_json::to_value(&src).unwrap();
        assert_eq!(json["kind"], "installMedia");
        let back: LibvirtBootSource = serde_json::from_value(json).unwrap();
        assert_eq!(src, back);
    }

    /// A `Deferred` image installs itself onto an empty disk and needs the
    /// installer CD-ROM; an `Immediate` one is overlaid on an
    /// already-installed volume and needs neither. The provider branches on
    /// exactly this, so both predicates are pinned.
    #[test]
    fn install_media_needs_an_empty_disk_and_a_cdrom() {
        let deferred = LibvirtBootSource {
            kind: LibvirtBootSourceKind::InstallMedia,
            volume: "i.iso".into(),
        };
        let immediate = LibvirtBootSource {
            kind: LibvirtBootSourceKind::BackingVolume,
            volume: "b.qcow2".into(),
        };
        assert!(deferred.needs_empty_os_disk());
        assert!(deferred.needs_install_cdrom());
        assert!(!immediate.needs_empty_os_disk());
        assert!(!immediate.needs_install_cdrom());
    }

    // ------------------------------------------------------------------
    // NIC source
    // ------------------------------------------------------------------

    #[test]
    fn nic_source_bridge_round_trips() {
        let src = LibvirtNicSource {
            kind: LibvirtNicSourceKind::Bridge,
            name: "br0".to_string(),
        };
        let json = serde_json::to_value(&src).unwrap();
        assert_eq!(json["kind"], "bridge");
        assert_eq!(json["name"], "br0");
        let back: LibvirtNicSource = serde_json::from_value(json).unwrap();
        assert_eq!(src, back);
    }

    /// Only a libvirt-managed network has a lease file. Reading leases for a
    /// raw bridge is not merely fruitless — it would make the provider
    /// report "no address yet" forever instead of falling through to the
    /// agent or ARP.
    #[test]
    fn only_a_managed_network_has_dhcp_leases() {
        assert!(
            LibvirtNicSource {
                kind: LibvirtNicSourceKind::Network,
                name: "default".into(),
            }
            .has_dhcp_leases()
        );
        assert!(
            !LibvirtNicSource {
                kind: LibvirtNicSourceKind::Bridge,
                name: "br0".into(),
            }
            .has_dhcp_leases()
        );
    }

    // ------------------------------------------------------------------
    // Defaults
    // ------------------------------------------------------------------

    #[test]
    fn disk_bus_defaults_to_virtio() {
        let json = serde_json::json!({ "name": "os", "sizeGiB": 40 });
        let d: LibvirtDiskSpec = serde_json::from_value(json).unwrap();
        assert_eq!(d.bus, LibvirtDiskBus::Virtio);
    }

    #[test]
    fn tpm_enabled_defaults_to_false() {
        let json = serde_json::json!({
            "providerRef": { "name": "libvirt-host-a" },
            "pool": "default",
            "domainName": "d",
            "bootSource": { "kind": "backingVolume", "volume": "b.qcow2" },
            "vcpus": 2,
            "memoryMiB": 2048,
            "firmware": "efi",
            "disks": [{ "name": "os", "sizeGiB": 20 }],
            "network": [],
        });
        let s: LibvirtMachineSpec = serde_json::from_value(json).unwrap();
        assert!(!s.tpm_enabled);
        assert_eq!(s.desired_power_state, PowerState::PoweredOn);
    }

    // ------------------------------------------------------------------
    // Enum wire forms — the YAML 1.1 implicit-boolean trap
    // ------------------------------------------------------------------

    /// Go's YAML 1.1 parser (the apiserver's) reads bare `on`/`off`/`yes`/
    /// `no`/`y`/`n`/`true`/`false` in ANY case as booleans, and rejects the
    /// CRD with `Invalid value: "boolean"`. Every variant that reaches a CRD
    /// enum schema must therefore serialize to something outside that set.
    #[test]
    fn no_enum_variant_collides_with_a_yaml_boolean() {
        const YAML_BOOLEANS: &[&str] = &[
            "y", "n", "yes", "no", "on", "off", "true", "false", "t", "f",
        ];
        let mut tokens = vec![
            serde_json::to_value(LibvirtDiskBus::Virtio).unwrap(),
            serde_json::to_value(LibvirtDiskBus::Scsi).unwrap(),
            serde_json::to_value(LibvirtDiskBus::Sata).unwrap(),
            serde_json::to_value(LibvirtAddressSource::GuestAgent).unwrap(),
            serde_json::to_value(LibvirtAddressSource::DhcpLease).unwrap(),
            serde_json::to_value(LibvirtAddressSource::ArpTable).unwrap(),
        ];
        // The discriminator enums contribute their own tokens.
        tokens.push(serde_json::to_value(LibvirtBootSourceKind::BackingVolume).unwrap());
        tokens.push(serde_json::to_value(LibvirtBootSourceKind::InstallMedia).unwrap());
        tokens.push(serde_json::to_value(LibvirtNicSourceKind::Bridge).unwrap());
        tokens.push(serde_json::to_value(LibvirtNicSourceKind::Network).unwrap());

        for t in tokens {
            let s = t.as_str().expect("enum variants serialize as strings");
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
    fn status_defaults_are_empty_and_not_provisioned() {
        let st = LibvirtMachineStatus::default();
        assert_ne!(st.initialization.provisioned, Some(true));
        assert!(st.addresses.is_empty());
        assert!(st.conditions.is_empty());
        assert!(st.domain_uuid.is_none());
        assert!(st.address_source.is_none());
    }

    /// Empty collections are skipped so an untouched status does not write
    /// `addresses: []` into every object.
    #[test]
    fn status_omits_empty_collections() {
        let json = serde_json::to_value(LibvirtMachineStatus::default()).unwrap();
        assert!(json.get("addresses").is_none(), "{json}");
        assert!(json.get("conditions").is_none(), "{json}");
    }

    #[test]
    fn status_round_trips_with_addresses() {
        let st = LibvirtMachineStatus {
            initialization: InitializationStatus {
                provisioned: Some(true),
            },
            addresses: vec![MachineAddress {
                address_type: MachineAddressType::InternalIP,
                address: "192.0.2.24".to_string(),
            }],
            domain_uuid: Some("0f3c9a1e-0000-4000-8000-000000000001".to_string()),
            address_source: Some(LibvirtAddressSource::GuestAgent),
            ..Default::default()
        };
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(json["addressSource"], "guestAgent");
        let back: LibvirtMachineStatus = serde_json::from_value(json).unwrap();
        assert_eq!(st, back);
    }

    // ------------------------------------------------------------------
    // CRD generation
    // ------------------------------------------------------------------

    #[test]
    fn crd_has_the_expected_identity() {
        use kube::CustomResourceExt;
        let crd = LibvirtMachine::crd();
        assert_eq!(crd.spec.group, "infrastructure.banlieue.io");
        assert_eq!(crd.spec.names.kind, "LibvirtMachine");
        assert_eq!(crd.spec.names.plural, "libvirtmachines");
        assert_eq!(crd.spec.scope, "Namespaced");
    }

    /// The InfraMachine contract requires a status subresource: the provider
    /// owns status and must be able to patch it without touching spec.
    #[test]
    fn crd_has_a_status_subresource() {
        use kube::CustomResourceExt;
        let crd = LibvirtMachine::crd();
        let v = &crd.spec.versions[0];
        assert!(
            v.subresources.as_ref().is_some_and(|s| s.status.is_some()),
            "LibvirtMachine needs a status subresource for the CAPI contract"
        );
    }

    #[test]
    fn template_crd_has_the_expected_identity() {
        use kube::CustomResourceExt;
        let crd = LibvirtMachineTemplate::crd();
        assert_eq!(crd.spec.names.kind, "LibvirtMachineTemplate");
        assert_eq!(crd.spec.names.plural, "libvirtmachinetemplates");
    }

    /// The template wraps a full machine spec, per CAPI's
    /// `template: { spec: {...} }` InfraMachineTemplate shape.
    #[test]
    fn template_wraps_a_machine_spec() {
        let t = LibvirtMachineTemplateSpec {
            template: LibvirtMachineTemplateResource {
                spec: minimal_spec(),
            },
        };
        let json = serde_json::to_value(&t).unwrap();
        assert!(json["template"]["spec"]["domainName"].is_string(), "{json}");
    }

    // ------------------------------------------------------------------
    // guestInstalled (ADR-0043)
    // ------------------------------------------------------------------

    /// Three states, not two. `None` is "not observed yet" — the expected
    /// state for the whole of a Deferred image's install — and collapsing
    /// it into `false` would lose the distinction between "still
    /// installing" and "checked, and it is not installed".
    #[test]
    fn guest_installed_is_tri_state_and_absent_by_default() {
        let st = LibvirtMachineStatus::default();
        assert_eq!(st.guest_installed, None);
        let json = serde_json::to_value(&st).unwrap();
        assert!(
            json.get("guestInstalled").is_none(),
            "an unobserved guest must not serialize a value: {json}"
        );
    }

    #[test]
    fn guest_installed_round_trips_as_camel_case() {
        let json = serde_json::json!({ "guestInstalled": true });
        let st: LibvirtMachineStatus = serde_json::from_value(json).unwrap();
        assert_eq!(st.guest_installed, Some(true));
        assert_eq!(
            serde_json::to_value(&st).unwrap()["guestInstalled"],
            serde_json::json!(true)
        );
    }
}
