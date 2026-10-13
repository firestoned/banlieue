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
        DiskProvisioning, Firmware, IpamShape, LabelSelector, LocalObjectReference, PowerState,
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
                network_overrides: Vec::new(),
                hardware_override: None,
                folder: None,
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
                            ipam: IpamShape::default(),
                            mtu: None,
                        })
                        .collect(),
                },
                firmware,
                features: features.into_iter().map(String::from).collect(),
                tpm_enabled: false,
            },
        }
    }

    /// Wraps [`class`] with `spec.tpmEnabled` overridden (ADR-0039) —
    /// mirrors [`vm_with_placement`]'s pattern of layering one field on top
    /// of the base builder rather than threading a new positional
    /// parameter through every one of `class`'s existing call sites.
    fn class_with_tpm_enabled(mut cls: VMClass, enabled: bool) -> VMClass {
        cls.spec.tpm_enabled = enabled;
        cls
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
                cloud_configs: vec![],
                template: None,
                iso_overlay: None,
                trusted_boot: None,
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
                        zones: vec![],
                    })
                    .collect(),
                build_artifact: None,
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
                    credentials_ref: Some(LocalObjectReference {
                        name: "creds".into(),
                    }),
                    insecure_skip_tls_verify: false,
                    ca_bundle: None,
                },
                capabilities: caps,
                paused: false,
                use_content_library: false,
                failure_domain_name_overrides: Vec::new(),
                attestation: None,
            },
            status: Some(ProviderStatus {
                failure_domains: fds,
                conditions: vec![],
                workload: None,
                observed_generation: None,
                ek_ca_certificates: vec![],
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
                    target: Some(BTreeMap::from([(k.to_string(), v.to_string())])),
                    ..Default::default()
                })
                .collect(),
            network_classes: network
                .iter()
                .map(|(name, k, v)| NetworkClassMapping {
                    name: name.to_string(),
                    target: Some(BTreeMap::from([(k.to_string(), v.to_string())])),
                    ..Default::default()
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
            lbls([("datacenter", "dc1")]),
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
                provider_selector: Some(label_selector(&[("datacenter", "dc2")])),
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
            lbls([("datacenter", "dc1")]),
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

    // --- Filter: vTPM (ADR-0039) --------------------------------------------

    #[test]
    fn tpm_enabled_without_feature_yields_tpm_unsupported() {
        let v = vm("db-01", "ns", BTreeMap::new());
        let cls = class_with_tpm_enabled(
            class(
                vec![("os", "gold")],
                vec![("eth0", "prod")],
                vec![],
                Firmware::Efi,
            ),
            true,
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
        assert_eq!(err, ScheduleError::TpmUnsupported);
    }

    #[test]
    fn tpm_enabled_with_feature_is_accepted() {
        let v = vm("db-01", "ns", BTreeMap::new());
        let cls = class_with_tpm_enabled(
            class(
                vec![("os", "gold")],
                vec![("eth0", "prod")],
                vec![],
                Firmware::Efi,
            ),
            true,
        );
        let img = image_ready_on(&["vc1"]);
        let p = provider(
            "vc1",
            BTreeMap::new(),
            vec![fd("fd-a", BTreeMap::new(), &["gold"], &["prod"], &["vtpm"])],
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );

        let decision = schedule(&v, &cls, &img, &[p], &[]).unwrap();
        assert_eq!(decision.failure_domain_name, "fd-a");
    }

    #[test]
    fn tpm_disabled_does_not_require_feature() {
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

        let decision = schedule(&v, &cls, &img, &[p], &[]).unwrap();
        assert_eq!(decision.failure_domain_name, "fd-a");
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

    // --- Reject-reason capping (SEC-015) -------------------------------------

    #[test]
    fn reject_reasons_under_cap_render_in_full() {
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
            vec![
                fd("fd-a", BTreeMap::new(), &["gold"], &["prod"], &[]),
                fd("fd-b", BTreeMap::new(), &["gold"], &["prod"], &[]),
            ],
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );

        let err = schedule(&v, &cls, &img, &[p], &[]).unwrap_err();
        match err {
            ScheduleError::NoFailureDomainMatched(msg) => {
                assert!(msg.contains("vc1/fd-a:"), "first reason kept: {msg}");
                assert!(msg.contains("vc1/fd-b:"), "second reason kept: {msg}");
                assert!(!msg.contains("more"), "no summary under the cap: {msg}");
            }
            other => panic!("expected NoFailureDomainMatched, got {other:?}"),
        }
    }

    #[test]
    fn reject_reasons_over_cap_are_summarized() {
        let extra = 5;
        let fds: Vec<FailureDomain> = (0..MAX_REJECT_REASONS + extra)
            .map(|i| {
                fd(
                    &format!("fd-{i:02}"),
                    BTreeMap::new(),
                    &["gold"],
                    &["prod"],
                    &[],
                )
            })
            .collect();
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
            fds,
            caps_with(
                &[("gold", "datastore", "ds-1")],
                &[("prod", "portGroup", "pg-1")],
            ),
        );

        let err = schedule(&v, &cls, &img, &[p], &[]).unwrap_err();
        match err {
            ScheduleError::NoFailureDomainMatched(msg) => {
                let first_omitted = format!("vc1/fd-{MAX_REJECT_REASONS:02}:");
                assert!(msg.contains("vc1/fd-00:"), "first reason kept: {msg}");
                assert!(
                    !msg.contains(&first_omitted),
                    "reason past the cap is omitted: {msg}"
                );
                assert!(
                    msg.ends_with(&format!("; … and {extra} more")),
                    "summary appended: {msg}"
                );
            }
            other => panic!("expected NoFailureDomainMatched, got {other:?}"),
        }
    }

    #[test]
    fn reject_reasons_collector_keeps_exactly_max() {
        let mut reasons = RejectReasons::default();
        for i in 0..MAX_REJECT_REASONS * 2 {
            reasons.push(format!("reason-{i}"));
        }
        let msg = reasons.render();
        assert_eq!(
            msg.matches("reason-").count(),
            MAX_REJECT_REASONS,
            "exactly MAX_REJECT_REASONS kept: {msg}"
        );
        assert!(msg.ends_with(&format!("; … and {MAX_REJECT_REASONS} more")));
    }

    // --- Spread (ADR-0098) --------------------------------------------------

    const POOL_LABEL: &str = "banlieue.io/pool";
    const HOSTS: [&str; 3] = ["h1", "h2", "h3"];

    /// Three single-domain providers, all able to run the test class.
    fn three_hosts() -> (Vec<Provider>, VMImage, VMClass) {
        let providers = HOSTS
            .iter()
            .map(|h| {
                provider(
                    h,
                    BTreeMap::new(),
                    vec![fd(h, BTreeMap::new(), &["gold"], &["prod"], &[])],
                    caps_with(
                        &[("gold", "datastore", "ds")],
                        &[("prod", "portGroup", "pg")],
                    ),
                )
            })
            .collect();
        let cls = class(
            vec![("os", "gold")],
            vec![("eth0", "prod")],
            vec![],
            Firmware::Efi,
        );
        (providers, image_ready_on(&HOSTS), cls)
    }

    fn member(name: &str, pool: &str) -> VirtualMachine {
        vm(name, "ns", lbls([(POOL_LABEL, pool)]))
    }

    fn placed_member(name: &str, pool: &str, host: &str) -> VirtualMachine {
        scheduled_vm(name, "ns", lbls([(POOL_LABEL, pool)]), host, host)
    }

    fn host_of(v: &VirtualMachine, existing: &[VirtualMachine]) -> String {
        let (providers, img, cls) = three_hosts();
        schedule(v, &cls, &img, &providers, existing)
            .unwrap()
            .failure_domain_name
    }

    #[test]
    fn a_pool_spreads_evenly_as_members_are_placed() {
        let mut existing = Vec::new();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for i in 0..6 {
            let name = format!("pool-a-{i}");
            let host = host_of(&member(&name, "a"), &existing);
            *counts.entry(host.clone()).or_default() += 1;
            existing.push(placed_member(&name, "a", &host));
        }
        assert_eq!(counts.len(), HOSTS.len(), "every host used: {counts:?}");
        assert!(counts.values().all(|&n| n == 2), "two per host: {counts:?}");
    }

    #[test]
    fn the_least_loaded_host_wins_whatever_the_name() {
        let existing = vec![
            placed_member("m1", "a", "h1"),
            placed_member("m2", "a", "h2"),
        ];
        for i in 0..20 {
            let v = member(&format!("new-{i}"), "a");
            assert_eq!(host_of(&v, &existing), "h3");
        }
    }

    /// A pool member spreads against its own pool; other VMs only break ties.
    #[test]
    fn the_spread_group_of_a_member_is_its_pool() {
        let existing = vec![
            placed_member("a1", "a", "h1"),
            placed_member("a2", "a", "h2"),
            placed_member("b1", "b", "h3"),
            placed_member("b2", "b", "h3"),
            placed_member("b3", "b", "h3"),
        ];
        assert_eq!(host_of(&member("a3", "a"), &existing), "h3");
    }

    /// Among equally loaded hosts (for its pool), the one with fewer VMs
    /// overall wins.
    #[test]
    fn total_load_breaks_a_tie_in_pool_load() {
        let existing = vec![
            placed_member("b1", "b", "h1"),
            placed_member("b2", "b", "h2"),
        ];
        assert_eq!(host_of(&member("a1", "a"), &existing), "h3");
    }

    #[test]
    fn a_vm_outside_any_pool_spreads_against_every_vm() {
        let existing = vec![
            scheduled_vm("x1", "ns", BTreeMap::new(), "h1", "h1"),
            scheduled_vm("x2", "ns", BTreeMap::new(), "h1", "h1"),
            placed_member("a1", "a", "h2"),
            placed_member("a2", "a", "h3"),
        ];
        for i in 0..20 {
            let v = vm(&format!("solo-{i}"), "ns", BTreeMap::new());
            assert_ne!(host_of(&v, &existing), "h1");
        }
    }

    /// With nothing placed, the choice depends on the VM (rendezvous
    /// hashing), so concurrent members that cannot see each other spread;
    /// and it is deterministic.
    #[test]
    fn ties_are_broken_per_vm_not_by_a_fixed_order() {
        let mut chosen = std::collections::BTreeSet::new();
        for i in 0..30 {
            let v = member(&format!("race-{i}"), "a");
            let first = host_of(&v, &[]);
            assert_eq!(first, host_of(&v, &[]), "same inputs, same answer");
            chosen.insert(first);
        }
        assert_eq!(
            chosen.len(),
            HOSTS.len(),
            "ties reach every host: {chosen:?}"
        );
    }

    #[test]
    fn a_vm_does_not_count_against_itself() {
        let existing = vec![
            placed_member("self", "a", "h1"),
            placed_member("m2", "a", "h2"),
            placed_member("m3", "a", "h3"),
        ];
        assert_eq!(host_of(&member("self", "a"), &existing), "h1");
    }

    /// Spread orders what the filters allow; it never overrides them.
    #[test]
    fn a_selector_beats_spread() {
        let existing = vec![
            placed_member("m1", "a", "h1"),
            placed_member("m2", "a", "h1"),
        ];
        let v = vm_with_placement(
            "pinned",
            "ns",
            lbls([(POOL_LABEL, "a")]),
            PlacementSpec {
                provider_selector: Some(label_selector(&[("host", "h1")])),
                ..PlacementSpec::default()
            },
        );
        let (mut providers, img, cls) = three_hosts();
        for p in &mut providers {
            let name = p.metadata.name.clone().unwrap_or_default();
            p.metadata.labels = Some(lbls([("host", name.as_str())]));
        }
        let d = schedule(&v, &cls, &img, &providers, &existing).unwrap();
        assert_eq!(d.failure_domain_name, "h1");
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
