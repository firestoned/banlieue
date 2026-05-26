// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::scheduler`].
//!
//! Table-driven where it pays; explicit per-filter where readability matters.
//! Every test constructs synthetic inputs via helpers so each filter step
//! can be exercised in isolation.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use banlieue_api::banlieue::{
        AffinityMode, AntiAffinityRule, Architecture, DiskSpec, FailureDomain,
        FailureDomainAttributes, GuestAgent, HardwareSpec, ImagePerProviderStatus, ImageSource,
        ImageSourceKind, MigrationPolicy, NetworkClassMapping, NetworkInterfaceSpec, NetworkSpec,
        OsFamily, PlacementSpec, Provider, ProviderCapabilities, ProviderConnection, ProviderSpec,
        ProviderStatus, ScheduledPlacement, StorageClassMapping, VMClass, VMClassSpec, VMImage,
        VMImageSpec, VMImageStatus, VirtualMachine, VirtualMachineSpec, VirtualMachineStatus,
    };
    use banlieue_api::common::{
        DiskProvisioning, Firmware, IpamSource, IpamSpec, LabelSelector, LocalObjectReference,
        PowerState,
    };
    use kube::core::ObjectMeta;

    use super::super::*;

    // --- Builders ------------------------------------------------------------

    fn vm(name: &str, ns: &str, labels: BTreeMap<String, String>) -> VirtualMachine {
        VirtualMachine {
            metadata: ObjectMeta {
                name: Some(name.into()),
                namespace: Some(ns.into()),
                labels: Some(labels),
                generation: Some(1),
                ..Default::default()
            },
            spec: VirtualMachineSpec {
                class_ref: LocalObjectReference { name: "c".into() },
                image_ref: LocalObjectReference { name: "i".into() },
                placement: PlacementSpec::default(),
                desired_power_state: PowerState::PoweredOn,
                user_data: None,
                migration_policy: MigrationPolicy::Automatic,
                paused: false,
            },
            status: None,
        }
    }

    fn vm_with_placement(
        name: &str,
        ns: &str,
        labels: BTreeMap<String, String>,
        placement: PlacementSpec,
    ) -> VirtualMachine {
        let mut v = vm(name, ns, labels);
        v.spec.placement = placement;
        v
    }

    fn scheduled_vm(
        name: &str,
        ns: &str,
        labels: BTreeMap<String, String>,
        provider: &str,
        failure_domain: &str,
    ) -> VirtualMachine {
        let mut v = vm(name, ns, labels);
        v.status = Some(VirtualMachineStatus {
            scheduled: Some(ScheduledPlacement {
                provider_name: provider.into(),
                provider_class: "vsphere".into(),
                failure_domain: failure_domain.into(),
                resolved_storage: vec![],
                resolved_networks: vec![],
                scheduled_at: None,
            }),
            ..Default::default()
        });
        v
    }

    fn class(
        disks: Vec<(&str, &str)>,
        nics: Vec<(&str, &str)>,
        features: Vec<&str>,
        firmware: Firmware,
    ) -> VMClass {
        VMClass {
            metadata: ObjectMeta {
                name: Some("c".into()),
                ..Default::default()
            },
            spec: VMClassSpec {
                hardware: HardwareSpec {
                    cpus: 2,
                    memory_mi_b: 1024,
                    disks: disks
                        .into_iter()
                        .map(|(n, sc)| DiskSpec {
                            name: n.into(),
                            size_gi_b: 10,
                            storage_class: sc.into(),
                            provisioning: DiskProvisioning::Thin,
                        })
                        .collect(),
                },
                network: NetworkSpec {
                    interfaces: nics
                        .into_iter()
                        .map(|(n, nc)| NetworkInterfaceSpec {
                            name: n.into(),
                            network_class: nc.into(),
                            ipam: IpamSpec {
                                source: IpamSource::Dhcp,
                                static_: None,
                                pool: None,
                            },
                            mtu: None,
                        })
                        .collect(),
                },
                firmware,
                features: features.into_iter().map(String::from).collect(),
            },
        }
    }

    fn image_ready_on(providers: &[&str]) -> VMImage {
        VMImage {
            metadata: ObjectMeta {
                name: Some("i".into()),
                ..Default::default()
            },
            spec: VMImageSpec {
                os_family: OsFamily::Linux,
                os_distribution: "ubuntu".into(),
                os_version: "22.04".into(),
                architecture: Architecture::Amd64,
                guest_agent: GuestAgent::CloudInit,
                sources: vec![ImageSource {
                    provider_class: "vsphere".into(),
                    kind: ImageSourceKind::Template,
                    reference: "ubuntu-22.04-cloudinit".into(),
                    import_from: None,
                    checksum: None,
                }],
            },
            status: Some(VMImageStatus {
                per_provider: providers
                    .iter()
                    .map(|p| ImagePerProviderStatus {
                        provider_name: (*p).into(),
                        provider_namespace: "banlieue-system".into(),
                        ready: true,
                        resolved_ref: Some(format!("/tpl/{p}")),
                        reason: None,
                        message: None,
                    })
                    .collect(),
                conditions: vec![],
                observed_generation: None,
            }),
        }
    }

    fn provider(
        name: &str,
        labels: BTreeMap<String, String>,
        fds: Vec<FailureDomain>,
        caps: ProviderCapabilities,
    ) -> Provider {
        Provider {
            metadata: ObjectMeta {
                name: Some(name.into()),
                namespace: Some("banlieue-system".into()),
                labels: Some(labels),
                ..Default::default()
            },
            spec: ProviderSpec {
                provider_class_ref: LocalObjectReference {
                    name: "vsphere".into(),
                },
                connection: ProviderConnection {
                    endpoint: "https://vcenter/sdk".into(),
                    credentials_ref: LocalObjectReference {
                        name: "creds".into(),
                    },
                    insecure_skip_tls_verify: false,
                    ca_bundle: None,
                },
                capabilities: caps,
                paused: false,
            },
            status: Some(ProviderStatus {
                failure_domains: fds,
                conditions: vec![],
                observed_generation: None,
            }),
        }
    }

    fn fd(
        name: &str,
        labels: BTreeMap<String, String>,
        storage: &[&str],
        network: &[&str],
        features: &[&str],
    ) -> FailureDomain {
        FailureDomain {
            name: name.into(),
            labels,
            attributes: FailureDomainAttributes {
                available_storage_classes: storage.iter().map(|s| s.to_string()).collect(),
                available_network_classes: network.iter().map(|s| s.to_string()).collect(),
                features: features.iter().map(|s| s.to_string()).collect(),
                raw: BTreeMap::new(),
            },
        }
    }

    fn lbls<const N: usize>(kv: [(&str, &str); N]) -> BTreeMap<String, String> {
        kv.into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn label_selector(kv: &[(&str, &str)]) -> LabelSelector {
        LabelSelector {
            match_labels: kv
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            match_expressions: vec![],
        }
    }

    fn caps_with(
        storage: &[(&str, &str, &str)],
        network: &[(&str, &str, &str)],
    ) -> ProviderCapabilities {
        // each tuple: (class_name, target_key, target_value)
        ProviderCapabilities {
            storage_classes: storage
                .iter()
                .map(|(name, k, v)| StorageClassMapping {
                    name: name.to_string(),
                    target: BTreeMap::from([(k.to_string(), v.to_string())]),
                })
                .collect(),
            network_classes: network
                .iter()
                .map(|(name, k, v)| NetworkClassMapping {
                    name: name.to_string(),
                    target: BTreeMap::from([(k.to_string(), v.to_string())]),
                })
                .collect(),
            features: vec![],
        }
    }

    // --- Happy path ----------------------------------------------------------

    #[test]
    fn happy_path_picks_single_candidate() {
        let v = vm("db-01", "banlieue-system", BTreeMap::new());
        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "prod")],
            vec![],
            Firmware::Efi,
        );
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            lbls([("dc", "dc1")]),
            vec![fd(
                "vc1-cluster-a",
                lbls([("cluster", "a")]),
                &["gold"],
                &["prod"],
                &[],
            )],
            caps_with(
                &[("gold", "datastore", "ds-fast-01")],
                &[("prod", "portGroup", "vmnet-prod")],
            ),
        );

        let d = schedule(&v, &cls, &img, &[p], &[]).expect("schedule");
        assert_eq!(d.provider_name, "vc1");
        assert_eq!(d.failure_domain_name, "vc1-cluster-a");
        assert_eq!(d.resolved_storage[0].class_name, "gold");
        assert_eq!(d.resolved_storage[0].backend_id, "ds-fast-01");
        assert_eq!(d.resolved_networks[0].class_name, "prod");
        assert_eq!(d.resolved_networks[0].backend_id, "vmnet-prod");
    }

    // --- Filter: providerSelector ------------------------------------------

    #[test]
    fn provider_selector_rejects_non_matching_providers() {
        let v = vm_with_placement(
            "db-01",
            "ns",
            BTreeMap::new(),
            PlacementSpec {
                provider_selector: Some(label_selector(&[("dc", "dc2")])),
                ..Default::default()
            },
        );
        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "prod")],
            vec![],
            Firmware::Efi,
        );
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            lbls([("dc", "dc1")]),
            vec![],
            ProviderCapabilities::default(),
        );

        let err = schedule(&v, &cls, &img, &[p], &[]).unwrap_err();
        assert_eq!(err, ScheduleError::NoProviderMatched);
        assert_eq!(err.reason(), reasons::NO_PROVIDER);
    }

    #[test]
    fn provider_selector_none_matches_every_provider() {
        let v = vm("db-01", "ns", BTreeMap::new()); // no selector
        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "prod")],
            vec![],
            Firmware::Efi,
        );
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            BTreeMap::new(),
            vec![fd("fd-a", BTreeMap::new(), &["gold"], &["prod"], &[])],
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );

        assert!(schedule(&v, &cls, &img, &[p], &[]).is_ok());
    }

    // --- Filter: failureDomainSelector --------------------------------------

    #[test]
    fn failure_domain_selector_drops_non_matching_fds() {
        let v = vm_with_placement(
            "db-01",
            "ns",
            BTreeMap::new(),
            PlacementSpec {
                failure_domain_selector: Some(label_selector(&[("cluster", "b")])),
                ..Default::default()
            },
        );
        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "prod")],
            vec![],
            Firmware::Efi,
        );
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            BTreeMap::new(),
            vec![
                fd("fd-a", lbls([("cluster", "a")]), &["gold"], &["prod"], &[]),
                fd("fd-b", lbls([("cluster", "b")]), &["gold"], &["prod"], &[]),
            ],
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );

        let d = schedule(&v, &cls, &img, &[p], &[]).unwrap();
        assert_eq!(d.failure_domain_name, "fd-b");
    }

    // --- Filter: image readiness --------------------------------------------

    #[test]
    fn image_not_ready_on_provider_yields_image_not_ready() {
        let v = vm("db-01", "ns", BTreeMap::new());
        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "prod")],
            vec![],
            Firmware::Efi,
        );
        let img = image_ready_on(&[]); // not ready anywhere
        let p = provider(
            "vc1",
            BTreeMap::new(),
            vec![fd("fd-a", BTreeMap::new(), &["gold"], &["prod"], &[])],
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );

        let err = schedule(&v, &cls, &img, &[p], &[]).unwrap_err();
        assert_eq!(err, ScheduleError::ImageNotReady);
    }

    // --- Filter: storage classes --------------------------------------------

    #[test]
    fn unsupported_storage_class_yields_no_failure_domain() {
        let v = vm("db-01", "ns", BTreeMap::new());
        let cls = class(
            vec![("os", "platinum")],
            vec![("eth0", "prod")],
            vec![],
            Firmware::Efi,
        );
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            BTreeMap::new(),
            vec![fd(
                "fd-a",
                BTreeMap::new(),
                &["gold", "silver"],
                &["prod"],
                &[],
            )],
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );

        let err = schedule(&v, &cls, &img, &[p], &[]).unwrap_err();
        match err {
            ScheduleError::NoFailureDomainMatched(msg) => {
                assert!(
                    msg.contains("platinum"),
                    "reject reasons mention class: {msg}"
                );
            }
            other => panic!("expected NoFailureDomainMatched, got {other:?}"),
        }
    }

    // --- Filter: network classes --------------------------------------------

    #[test]
    fn unsupported_network_class_yields_no_failure_domain() {
        let v = vm("db-01", "ns", BTreeMap::new());
        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "mgmt")],
            vec![],
            Firmware::Efi,
        );
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            BTreeMap::new(),
            vec![fd("fd-a", BTreeMap::new(), &["gold"], &["prod"], &[])],
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );

        let err = schedule(&v, &cls, &img, &[p], &[]).unwrap_err();
        assert!(matches!(err, ScheduleError::NoFailureDomainMatched(_)));
    }

    // --- Filter: features ---------------------------------------------------

    #[test]
    fn unsupported_feature_yields_no_failure_domain() {
        let v = vm("db-01", "ns", BTreeMap::new());
        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "prod")],
            vec!["gpuPassthrough"],
            Firmware::Efi,
        );
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            BTreeMap::new(),
            vec![fd("fd-a", BTreeMap::new(), &["gold"], &["prod"], &[])],
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );

        let err = schedule(&v, &cls, &img, &[p], &[]).unwrap_err();
        assert!(matches!(err, ScheduleError::NoFailureDomainMatched(_)));
    }

    // --- Filter: firmware (efi-secure) --------------------------------------

    #[test]
    fn efi_secure_without_feature_yields_firmware_unsupported() {
        let v = vm("db-01", "ns", BTreeMap::new());
        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "prod")],
            vec![],
            Firmware::EfiSecure,
        );
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            BTreeMap::new(),
            vec![fd("fd-a", BTreeMap::new(), &["gold"], &["prod"], &[])],
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );

        let err = schedule(&v, &cls, &img, &[p], &[]).unwrap_err();
        assert_eq!(err, ScheduleError::FirmwareUnsupported);
    }

    #[test]
    fn efi_secure_with_feature_is_accepted() {
        let v = vm("db-01", "ns", BTreeMap::new());
        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "prod")],
            vec![],
            Firmware::EfiSecure,
        );
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            BTreeMap::new(),
            vec![fd(
                "fd-a",
                BTreeMap::new(),
                &["gold"],
                &["prod"],
                &["efiSecureBoot"],
            )],
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );

        assert!(schedule(&v, &cls, &img, &[p], &[]).is_ok());
    }

    // --- Anti-affinity ------------------------------------------------------

    #[test]
    fn required_anti_affinity_drops_colliding_fd() {
        // Two FDs; one already hosts a sibling matching the rule's selector.
        let labels_db_prod = lbls([("app", "db-prod")]);
        let placement = PlacementSpec {
            anti_affinity: vec![AntiAffinityRule {
                topology_key: "failure_domain".into(),
                label_selector: label_selector(&[("app", "db-prod")]),
                mode: AffinityMode::Required,
            }],
            ..Default::default()
        };
        let v = vm_with_placement("db-02", "ns", labels_db_prod.clone(), placement);
        let sibling = scheduled_vm("db-01", "ns", labels_db_prod.clone(), "vc1", "fd-a");

        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "prod")],
            vec![],
            Firmware::Efi,
        );
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            BTreeMap::new(),
            vec![
                fd(
                    "fd-a",
                    lbls([("failure_domain", "fd-a")]),
                    &["gold"],
                    &["prod"],
                    &[],
                ),
                fd(
                    "fd-b",
                    lbls([("failure_domain", "fd-b")]),
                    &["gold"],
                    &["prod"],
                    &[],
                ),
            ],
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );

        let d = schedule(&v, &cls, &img, &[p], &[sibling]).unwrap();
        assert_eq!(d.failure_domain_name, "fd-b", "must avoid fd-a");
    }

    #[test]
    fn required_anti_affinity_can_block_all_candidates() {
        let labels_db_prod = lbls([("app", "db-prod")]);
        let placement = PlacementSpec {
            anti_affinity: vec![AntiAffinityRule {
                topology_key: "failure_domain".into(),
                label_selector: label_selector(&[("app", "db-prod")]),
                mode: AffinityMode::Required,
            }],
            ..Default::default()
        };
        let v = vm_with_placement("db-02", "ns", labels_db_prod.clone(), placement);
        let sibling = scheduled_vm("db-01", "ns", labels_db_prod, "vc1", "fd-a");

        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "prod")],
            vec![],
            Firmware::Efi,
        );
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            BTreeMap::new(),
            vec![fd(
                "fd-a",
                lbls([("failure_domain", "fd-a")]),
                &["gold"],
                &["prod"],
                &[],
            )],
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );

        let err = schedule(&v, &cls, &img, &[p], &[sibling]).unwrap_err();
        match err {
            ScheduleError::AntiAffinityUnsatisfied(key) => {
                assert_eq!(key, "failure_domain");
            }
            other => panic!("expected AntiAffinityUnsatisfied, got {other:?}"),
        }
    }

    // --- Tie-break ----------------------------------------------------------

    #[test]
    fn tiebreak_is_alphabetical_provider_then_fd() {
        let v = vm("db-01", "ns", BTreeMap::new());
        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "prod")],
            vec![],
            Firmware::Efi,
        );
        let img = image_ready_on(&["aaa", "zzz"]);
        let providers = vec![
            provider(
                "zzz",
                BTreeMap::new(),
                vec![fd("z-fd", BTreeMap::new(), &["gold"], &["prod"], &[])],
                caps_with(
                    &[("gold", "datastore", "z-ds")],
                    &[("prod", "portGroup", "z-pg")],
                ),
            ),
            provider(
                "aaa",
                BTreeMap::new(),
                vec![
                    fd("b-fd", BTreeMap::new(), &["gold"], &["prod"], &[]),
                    fd("a-fd", BTreeMap::new(), &["gold"], &["prod"], &[]),
                ],
                caps_with(
                    &[("gold", "datastore", "a-ds")],
                    &[("prod", "portGroup", "a-pg")],
                ),
            ),
        ];

        let d = schedule(&v, &cls, &img, &providers, &[]).unwrap();
        assert_eq!(d.provider_name, "aaa");
        assert_eq!(d.failure_domain_name, "a-fd");
    }

    // --- Backend ID resolution ---------------------------------------------

    #[test]
    fn backend_id_uses_first_value_by_btreemap_key_order() {
        // Target { datastoreCluster: "dsc-gold" } — single value, picked directly.
        let v = vm("db-01", "ns", BTreeMap::new());
        let cls = class(vec![("os", "gold")], vec![], vec![], Firmware::Efi);
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            BTreeMap::new(),
            vec![fd("fd-a", BTreeMap::new(), &["gold"], &[], &[])],
            caps_with(&[("gold", "datastoreCluster", "dsc-gold")], &[]),
        );
        let d = schedule(&v, &cls, &img, &[p], &[]).unwrap();
        assert_eq!(d.resolved_storage[0].backend_id, "dsc-gold");
    }

    #[test]
    fn backend_id_falls_back_to_class_name_when_no_mapping_present() {
        // The class IS available on the failure domain (so it passes the filter)
        // but the Provider's capabilities table has no entry — the backend_id
        // is set to the class name itself.
        let v = vm("db-01", "ns", BTreeMap::new());
        let cls = class(vec![("os", "gold")], vec![], vec![], Firmware::Efi);
        let img = image_ready_on(&["vc1"]);
        let mut p = provider(
            "vc1",
            BTreeMap::new(),
            vec![fd("fd-a", BTreeMap::new(), &["gold"], &[], &[])],
            ProviderCapabilities::default(),
        );
        // Make the class "available" on the failure domain without an explicit mapping.
        if let Some(status) = p.status.as_mut() {
            status.failure_domains[0]
                .attributes
                .available_storage_classes = vec!["gold".into()];
        }

        let d = schedule(&v, &cls, &img, &[p], &[]).unwrap();
        assert_eq!(d.resolved_storage[0].backend_id, "gold");
    }

    // --- to_scheduled_placement --------------------------------------------

    #[test]
    fn to_scheduled_placement_round_trips_resolution() {
        let v = vm("db-01", "ns", BTreeMap::new());
        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "prod")],
            vec![],
            Firmware::Efi,
        );
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            BTreeMap::new(),
            vec![fd("fd-a", BTreeMap::new(), &["gold"], &["prod"], &[])],
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );
        let d = schedule(&v, &cls, &img, &[p], &[]).unwrap();

        let now = k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
            k8s_openapi::jiff::Timestamp::now(),
        );
        let sp = d.to_scheduled_placement(now);
        assert_eq!(sp.provider_name, "vc1");
        assert_eq!(sp.failure_domain, "fd-a");
        assert_eq!(sp.resolved_storage[0].backend_id, "ds-1");
        assert_eq!(sp.resolved_networks[0].backend_id, "pg-1");
        assert!(sp.scheduled_at.is_some());
    }
}
