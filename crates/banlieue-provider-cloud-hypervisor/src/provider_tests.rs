// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `provider.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::test_fixtures::config;
    use banlieue_api::banlieue::{
        NetworkClassMapping, ProviderCapabilities, ProviderConnection, ProviderSpec,
        StorageClassMapping,
    };
    use banlieue_api::common::LocalObjectReference;
    use kube::api::ObjectMeta;

    fn target(host_class: &str) -> Option<BTreeMap<String, String>> {
        Some(BTreeMap::from([(
            TARGET_KEY_HOST_CLASS.to_string(),
            host_class.to_string(),
        )]))
    }

    fn provider(features: &[&str]) -> Provider {
        let mut p = Provider::new(
            "ch-a",
            ProviderSpec {
                provider_class_ref: LocalObjectReference {
                    name: "cloud-hypervisor".into(),
                },
                connection: ProviderConnection {
                    endpoint: "bar.foo.io".into(),
                    credentials_ref: None,
                    insecure_skip_tls_verify: false,
                    ca_bundle: None,
                },
                capabilities: ProviderCapabilities {
                    storage_classes: vec![
                        StorageClassMapping {
                            name: "gold".into(),
                            target: target("fast"),
                            ..Default::default()
                        },
                        StorageClassMapping {
                            name: "silver".into(),
                            target: target("slow"),
                            ..Default::default()
                        },
                    ],
                    network_classes: vec![NetworkClassMapping {
                        name: "prod".into(),
                        target: target("lan"),
                        ..Default::default()
                    }],
                    features: features.iter().map(|f| (*f).to_string()).collect(),
                },
                paused: false,
                failure_domain_name_overrides: Default::default(),
                use_content_library: Default::default(),
                attestation: None,
                scheduling: None,
                namespace_selector: None,
            },
        );
        p.metadata = ObjectMeta {
            name: Some("ch-a".into()),
            namespace: Some("banlieue-system".into()),
            ..Default::default()
        };
        p
    }

    fn healthy() -> HostFacts {
        HostFacts {
            kvm: true,
            vmm_binary: true,
            firmware: true,
            storage_present: ["fast".to_string()].into(),
            bridges_present: ["lan".to_string()].into(),
            vtpm: false,
            ek_ca_pem: None,
            ca_not_after: None,
            capacity: HostCapacity {
                cpus: Some(32),
                cpu_model: Some("Intel(R) Xeon(R) CPU E5-2630 v3 @ 2.40GHz".into()),
                memory_mib: Some(64_318),
                hugepages_mib: Some(0),
                memory_available_mib: Some(40_000),
                storage: [(
                    "fast".to_string(),
                    StoreSpace {
                        total_gib: 900,
                        free_gib: 812,
                    },
                )]
                .into(),
            },
            storage_without_reflink: BTreeSet::new(),
            guest_budget: SliceBudget::default(),
        }
    }

    // ------------------------------------------------------------------
    // Host capacity (roadmap 09's stop condition: real CPU, memory, storage)
    // ------------------------------------------------------------------

    const CPUINFO: &str = "processor\t: 0\nvendor_id\t: GenuineIntel\nmodel name\t: Intel(R) Xeon(R) CPU E5-2630 v3 @ 2.40GHz\nflags\t\t: fpu vme\n\nprocessor\t: 1\nmodel name\t: Intel(R) Xeon(R) CPU E5-2630 v3 @ 2.40GHz\n\n";
    const MEMINFO: &str = "MemTotal:       65861768 kB\nMemFree:         1024000 kB\nHugePages_Total:     512\nHugePages_Free:      512\nHugepagesize:       2048 kB\n";

    #[test]
    fn cpu_count_and_model_come_from_cpuinfo() {
        assert_eq!(
            parse_cpuinfo(CPUINFO),
            (
                Some(2),
                Some("Intel(R) Xeon(R) CPU E5-2630 v3 @ 2.40GHz".to_string())
            )
        );
        assert_eq!(parse_cpuinfo(""), (None, None));
    }

    /// Hugepages are reserved, not allocated on demand, so they are their
    /// own figure (roadmap 09, Gotchas).
    #[test]
    fn memory_and_reserved_hugepages_come_from_meminfo() {
        assert_eq!(parse_meminfo(MEMINFO), (Some(64_318), Some(1024)));
        assert_eq!(
            parse_meminfo("MemTotal: 2048 kB\nHugePages_Total: 0\nHugepagesize: 2048 kB\n"),
            (Some(2), Some(0))
        );
        assert_eq!(parse_meminfo("garbage"), (None, None));
    }

    /// `MemAvailable` already leaves out reserved hugepages and counts
    /// reclaimable cache (ADR-0128).
    #[test]
    fn available_memory_comes_from_meminfo() {
        assert_eq!(
            parse_mem_available("MemTotal: 8192 kB\nMemAvailable:    4194304 kB\n"),
            Some(4096)
        );
        assert_eq!(parse_mem_available(MEMINFO), None);
    }

    #[test]
    fn the_failure_domain_reports_the_hosts_capacity() {
        let st = compute_status(&provider(&[]), &config(), &healthy(), 1);
        let raw = &st.failure_domains[0].attributes.raw;
        let get = |k: &str| raw.get(k).map(String::as_str);
        assert_eq!(get("cpus"), Some("32"));
        assert_eq!(
            get("cpuModel"),
            Some("Intel(R) Xeon(R) CPU E5-2630 v3 @ 2.40GHz")
        );
        assert_eq!(get("memoryMiB"), Some("64318"));
        assert_eq!(get("hugepagesMiB"), Some("0"));
        assert!(
            get("storageFreeGiB").is_none(),
            "free disk is typed now (ADR-0128)"
        );
        assert_eq!(get("nestedVirtualization"), Some("false"));
    }

    /// A storage class whose filesystem cannot reflink makes every clone a
    /// full copy, and the guest's first disk flush waits for it to reach the
    /// disk (roadmap 18, 2026-11-07): named in the failure domain, so it is
    /// visible from the cluster.
    #[test]
    fn storage_that_cannot_reflink_is_named() {
        let facts = HostFacts {
            storage_without_reflink: ["fast".to_string()].into(),
            ..healthy()
        };
        let st = compute_status(&provider(&[]), &config(), &facts, 1);
        let raw = &st.failure_domains[0].attributes.raw;
        assert_eq!(
            raw.get("storageWithoutReflink").map(String::as_str),
            Some("fast")
        );
    }

    /// Slow clones are not a broken host: the Provider stays Ready and keeps
    /// its failure domain.
    #[test]
    fn storage_that_cannot_reflink_does_not_block_scheduling() {
        let facts = HostFacts {
            storage_without_reflink: ["fast".to_string()].into(),
            ..healthy()
        };
        let mut p = provider(&[]);
        p.spec.capabilities.storage_classes.pop();
        let st = compute_status(&p, &config(), &facts, 1);
        assert_eq!(st.failure_domains.len(), 1);
        assert_eq!(ready(&st).status, "True");
    }

    #[test]
    fn storage_that_reflinks_is_not_mentioned() {
        let st = compute_status(&provider(&[]), &config(), &healthy(), 1);
        let raw = &st.failure_domains[0].attributes.raw;
        assert!(!raw.contains_key("storageWithoutReflink"), "{raw:?}");
    }

    /// A figure the host would not give is left out, not reported as zero.
    #[test]
    fn unknown_capacity_is_omitted() {
        let facts = HostFacts {
            capacity: HostCapacity::default(),
            ..healthy()
        };
        let st = compute_status(&provider(&[]), &config(), &facts, 1);
        let raw = &st.failure_domains[0].attributes.raw;
        for k in [
            "cpus",
            "cpuModel",
            "memoryMiB",
            "hugepagesMiB",
            "storageFreeGiB",
        ] {
            assert!(!raw.contains_key(k), "{k}: {raw:?}");
        }
    }

    fn ready(st: &ProviderStatus) -> &k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition {
        st.conditions.iter().find(|c| c.type_ == "Ready").unwrap()
    }

    /// One failure domain per host, named after the Provider, never a path.
    #[test]
    fn one_failure_domain_named_after_the_provider() {
        let st = compute_status(&provider(&[]), &config(), &healthy(), 1);
        assert_eq!(st.failure_domains.len(), 1);
        let fd = &st.failure_domains[0];
        assert_eq!(fd.name, "ch-a");
        assert_eq!(fd.labels.get("name").map(String::as_str), Some("ch-a"));
        assert!(
            fd.attributes.raw.values().all(|v| !v.contains('/')),
            "{:?}",
            fd.attributes.raw
        );
    }

    /// Only classes the host really serves are published.
    #[test]
    fn a_declared_class_the_host_cannot_serve_is_withheld_and_reported() {
        let st = compute_status(&provider(&[]), &config(), &healthy(), 1);
        let fd = &st.failure_domains[0];
        assert_eq!(
            fd.attributes.available_storage_classes,
            vec!["gold".to_string()]
        );
        assert_eq!(
            fd.attributes.available_network_classes,
            vec!["prod".to_string()]
        );
        let r = ready(&st);
        assert_eq!(r.status, "False");
        assert_eq!(r.reason, reasons::CAPABILITIES_INCOMPLETE);
        assert!(r.message.contains("silver"), "{}", r.message);
    }

    #[test]
    fn a_missing_bridge_withholds_the_network_class() {
        let mut f = healthy();
        f.bridges_present.clear();
        let st = compute_status(&provider(&[]), &config(), &f, 1);
        assert!(
            st.failure_domains[0]
                .attributes
                .available_network_classes
                .is_empty()
        );
    }

    #[test]
    fn a_fully_served_provider_is_ready() {
        let mut p = provider(&[]);
        p.spec.capabilities.storage_classes.pop();
        let st = compute_status(&p, &config(), &healthy(), 7);
        let r = ready(&st);
        assert_eq!(r.status, "True");
        assert_eq!(st.observed_generation, Some(7));
        assert!(st.workload.is_none(), "workload belongs to the operator");
    }

    /// A host that cannot run guests offers no failure domain at all.
    #[test]
    fn a_host_without_kvm_or_vmm_offers_nothing() {
        for gap in [
            HostFacts {
                kvm: false,
                ..healthy()
            },
            HostFacts {
                vmm_binary: false,
                ..healthy()
            },
            HostFacts {
                firmware: false,
                ..healthy()
            },
        ] {
            let st = compute_status(&provider(&[]), &config(), &gap, 1);
            assert!(st.failure_domains.is_empty());
            assert_eq!(ready(&st).reason, reasons::HOST_NOT_READY);
        }
    }

    /// ADR-0065 Decision 2: `vtpm` is passed through only when this host
    /// can really manufacture and run one; declared on a host without swtpm
    /// it is withheld, so no `tpmEnabled` VM is scheduled here.
    #[test]
    fn vtpm_is_advertised_only_when_the_host_can_serve_it() {
        let without = compute_status(&provider(&["vtpm", "hugepages"]), &config(), &healthy(), 1);
        assert_eq!(
            without.failure_domains[0].attributes.features,
            vec!["hugepages".to_string()]
        );
        assert!(without.ek_ca_certificates.is_empty());

        let facts = HostFacts {
            vtpm: true,
            ek_ca_pem: Some("CA-PEM".into()),
            ..healthy()
        };
        let with = compute_status(&provider(&["vtpm", "hugepages"]), &config(), &facts, 1);
        assert_eq!(
            with.failure_domains[0].attributes.features,
            vec!["vtpm".to_string(), "hugepages".to_string()]
        );
        // Decision 6: the host's EK CA, for ekTrustBundle.
        assert_eq!(with.ek_ca_certificates, vec!["CA-PEM".to_string()]);

        // Usable but not declared: features are declared, never discovered.
        let undeclared = compute_status(&provider(&["hugepages"]), &config(), &facts, 1);
        assert_eq!(
            undeclared.failure_domains[0].attributes.features,
            vec!["hugepages".to_string()]
        );
    }

    /// A host config without `[tpm]` has no vTPM, whatever is installed.
    #[test]
    fn no_tpm_section_means_no_vtpm_fact() {
        let f = gather_facts(&config());
        assert!(!f.vtpm);
        assert!(f.ek_ca_pem.is_none());
    }

    #[test]
    fn gather_facts_sees_this_machine() {
        // Grounded in the real host this runs on: loopback exists, and a
        // storage path under an empty temp dir does not.
        let empty = tempfile::tempdir().unwrap();
        let mut c = config();
        c.network_classes.insert("lo".into(), "lo".into());
        c.storage_classes
            .insert("fast".into(), empty.path().join("absent"));
        let f = gather_facts(&c);
        assert!(f.bridges_present.contains("lo"));
        assert!(!f.storage_present.contains("fast"));
    }

    /// An unchanged condition keeps its lastTransitionTime.
    #[test]
    fn unchanged_conditions_keep_their_transition_time() {
        let mut p = provider(&[]);
        let first = compute_status(&p, &config(), &healthy(), 1);
        let t = ready(&first).last_transition_time.clone();
        p.status = Some(first);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = compute_status(&p, &config(), &healthy(), 2);
        assert_eq!(ready(&second).last_transition_time, t);
        assert_eq!(ready(&second).observed_generation, Some(2));
    }

    #[test]
    fn only_our_provider_by_name_and_namespace() {
        let c = config();
        let mut p = provider(&[]);
        assert!(is_this_host(&p, &c));
        p.metadata.namespace = Some("other".into());
        assert!(!is_this_host(&p, &c));
        p.metadata.namespace = Some("banlieue-system".into());
        p.metadata.name = Some("ch-b".into());
        assert!(!is_this_host(&p, &c));
    }

    /// ADR-0060 Decision 4: this provider reads no Secret. A Provider that
    /// names credentials is misconfigured; it is told so and offered no
    /// failure domain, and the reference is never read.
    #[test]
    fn a_provider_naming_credentials_is_refused_and_offers_nothing() {
        let mut p = provider(&[]);
        p.spec.connection.credentials_ref = Some(LocalObjectReference {
            name: "some-secret".into(),
        });
        let st = compute_status(&p, &config(), &healthy(), 1);
        assert!(st.failure_domains.is_empty());
        let r = ready(&st);
        assert_eq!(r.status, "False");
        assert_eq!(r.reason, reasons::CREDENTIALS_NOT_ALLOWED);
    }

    // ------------------------------------------------------------------
    // Typed capacity for the scheduler (ADR-0107)
    // ------------------------------------------------------------------

    fn host(cpus: Option<u32>, memory: Option<u64>, hugepages: Option<u64>) -> HostCapacity {
        HostCapacity {
            cpus,
            memory_mib: memory,
            hugepages_mib: hugepages,
            ..HostCapacity::default()
        }
    }

    /// No slice budget: guests may use the host, less reserved hugepages
    /// (memory a guest cannot be given on demand).
    #[test]
    fn without_a_budget_guests_get_the_host_less_hugepages() {
        let c = guest_capacity(
            &host(Some(96), Some(262_144), Some(1024)),
            &SliceBudget::default(),
        );
        assert_eq!(c.cpus, Some(96));
        assert_eq!(c.memory_mi_b, Some(261_120));
    }

    /// A smaller budget wins; a larger one changes nothing.
    #[test]
    fn the_slice_budget_caps_what_guests_get() {
        let h = host(Some(96), Some(262_144), Some(0));
        let small = SliceBudget {
            allowed_cpus: Some(80),
            memory_max_mib: Some(204_800),
            ..SliceBudget::default()
        };
        let c = guest_capacity(&h, &small);
        assert_eq!((c.cpus, c.memory_mi_b), (Some(80), Some(204_800)));
        let large = SliceBudget {
            allowed_cpus: Some(128),
            memory_max_mib: Some(1 << 30),
            ..SliceBudget::default()
        };
        let c = guest_capacity(&h, &large);
        assert_eq!((c.cpus, c.memory_mi_b), (Some(96), Some(262_144)));
    }

    /// A host figure that could not be read still yields the budget's.
    #[test]
    fn a_budget_stands_in_for_an_unreadable_host_figure() {
        let budget = SliceBudget {
            allowed_cpus: Some(8),
            memory_max_mib: Some(4096),
            ..SliceBudget::default()
        };
        let c = guest_capacity(&HostCapacity::default(), &budget);
        assert_eq!((c.cpus, c.memory_mi_b), (Some(8), Some(4096)));
    }

    /// Each guest's VMM runs with MemoryMax = guest + this; the scheduler
    /// must count it, or a full host OOM-kills a VMM.
    #[test]
    fn the_per_vm_overhead_is_the_vmms_memory_margin() {
        let c = guest_capacity(&HostCapacity::default(), &SliceBudget::default());
        assert_eq!(c.memory_overhead_mi_b_per_vm, Some(VMM_MEMORY_OVERHEAD_MIB));
        assert_eq!(VMM_MEMORY_OVERHEAD_MIB, 512);
    }

    // ------------------------------------------------------------------
    // Measured capacity (ADR-0128)
    // ------------------------------------------------------------------

    /// Free memory is the host's, or the slice's headroom when smaller, never
    /// more than what guests may use, and rounded down to a whole GiB so
    /// the status does not change on every pass.
    #[test]
    fn available_memory_is_capped_and_rounded_down_to_a_gib() {
        let mut h = host(None, Some(65_536), Some(0));
        h.memory_available_mib = Some(40_000);
        let c = guest_capacity(&h, &SliceBudget::default());
        assert_eq!(c.memory_available_mi_b, Some(39 * 1024));

        let tight = SliceBudget {
            memory_headroom_mib: Some(5000),
            ..SliceBudget::default()
        };
        assert_eq!(
            guest_capacity(&h, &tight).memory_available_mi_b,
            Some(4 * 1024)
        );

        let small = SliceBudget {
            memory_max_mib: Some(8192),
            ..SliceBudget::default()
        };
        assert_eq!(
            guest_capacity(&h, &small).memory_available_mi_b,
            Some(8192),
            "never more than memoryMiB"
        );

        assert_eq!(
            guest_capacity(&HostCapacity::default(), &SliceBudget::default()).memory_available_mi_b,
            None
        );
    }

    /// Each available storage class carries its store's size and free
    /// space, under the Provider's class name; an unserved class has none.
    #[test]
    fn the_failure_domain_publishes_typed_storage() {
        let status = compute_status(&provider(&[]), &config(), &healthy(), 1);
        let cap = status.failure_domains[0]
            .attributes
            .capacity
            .clone()
            .expect("typed capacity");
        assert_eq!(
            cap.storage,
            vec![banlieue_api::banlieue::StorageCapacity {
                storage_class: "gold".into(),
                total_gi_b: Some(900),
                free_gi_b: Some(812),
                shared: false,
            }]
        );
        assert_eq!(cap.memory_available_mi_b, Some(39 * 1024));
    }

    #[test]
    fn the_failure_domain_publishes_typed_capacity() {
        let mut facts = healthy();
        facts.guest_budget = SliceBudget {
            allowed_cpus: None,
            memory_max_mib: Some(32_768),
            ..SliceBudget::default()
        };
        let status = compute_status(&provider(&[]), &config(), &facts, 1);
        let cap = status.failure_domains[0]
            .attributes
            .capacity
            .clone()
            .expect("typed capacity");
        assert_eq!(cap.cpus, Some(32));
        assert_eq!(cap.memory_mi_b, Some(32_768));
        assert_eq!(cap.memory_overhead_mi_b_per_vm, Some(512));
    }

    // ------------------------------------------------------------------
    // ADR-0146 Decision 4: an opted-in host with no hostCa is not Ready
    // ------------------------------------------------------------------

    use crate::test_fixtures::with_attestation;
    use banlieue_api::banlieue::provider::ProviderAttestation;
    use banlieue_api::common::{CABundleSource, KeySelector};

    /// The shared Provider without the class `healthy()` cannot serve, so a
    /// Ready condition here is decided by the attestation hold alone.
    fn servable() -> Provider {
        let mut p = provider(&[]);
        p.spec
            .capabilities
            .storage_classes
            .retain(|c| c.name != "silver");
        p
    }

    fn asserting(source: CABundleSource) -> Provider {
        let mut p = servable();
        p.spec.attestation = Some(ProviderAttestation {
            ek_trust_bundle: None,
            host_ca: Some(source),
        });
        p
    }

    fn inline_ca() -> CABundleSource {
        CABundleSource {
            inline: Some("-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n".into()),
            ..Default::default()
        }
    }

    fn ready_triple(st: &ProviderStatus) -> (String, String, String) {
        let c = st
            .conditions
            .iter()
            .find(|c| c.type_ == condition_types::READY)
            .expect("Ready");
        (c.status.clone(), c.reason.clone(), c.message.clone())
    }

    #[test]
    fn an_opted_in_host_without_a_host_ca_is_not_ready_and_names_the_field() {
        let st = compute_status(&servable(), &with_attestation(config()), &healthy(), 1);
        let (status, reason, message) = ready_triple(&st);
        assert_eq!(
            (status.as_str(), reason.as_str()),
            ("False", "HostCaMissing")
        );
        assert!(message.contains("spec.attestation.hostCa"), "{message}");
        assert!(
            st.failure_domains.is_empty(),
            "nothing may be scheduled here"
        );
    }

    // ------------------------------------------- an expired CA (ADR-0146 Decision 7)

    use std::time::{Duration, SystemTime};

    const DAY: Duration = Duration::from_secs(86_400);

    fn facts_with_ca_expiring(at: SystemTime) -> HostFacts {
        HostFacts {
            ca_not_after: Some(at),
            ..healthy()
        }
    }

    #[test]
    fn a_ca_with_no_expiry_to_report_holds_nothing() {
        assert_eq!(host_ca_expiry_hold(None, SystemTime::now()), None);
    }

    #[test]
    fn a_ca_before_its_not_after_holds_nothing() {
        let now = SystemTime::now();
        assert_eq!(host_ca_expiry_hold(Some(now + DAY), now), None);
    }

    #[test]
    fn a_ca_at_or_past_its_not_after_holds_every_guest() {
        let now = SystemTime::now();
        for not_after in [now, now - DAY] {
            let hold = host_ca_expiry_hold(Some(not_after), now).expect("expired");
            assert_eq!(hold.reason(), "HostCaExpired");
            assert!(hold.message().contains("rotat"), "{}", hold.message());
        }
    }

    #[test]
    fn an_opted_in_host_whose_ca_has_expired_is_not_ready() {
        let facts = facts_with_ca_expiring(SystemTime::now() - DAY);
        let st = compute_status(
            &asserting(inline_ca()),
            &with_attestation(config()),
            &facts,
            1,
        );
        let (status, reason, _) = ready_triple(&st);
        assert_eq!(
            (status.as_str(), reason.as_str()),
            ("False", "HostCaExpired")
        );
        assert!(
            st.failure_domains.is_empty(),
            "nothing may be scheduled here"
        );
    }

    #[test]
    fn an_unexpired_ca_leaves_the_host_ready() {
        let facts = facts_with_ca_expiring(SystemTime::now() + DAY);
        let st = compute_status(
            &asserting(inline_ca()),
            &with_attestation(config()),
            &facts,
            1,
        );
        assert_eq!(ready_triple(&st).1, "Reconciled");
    }

    #[test]
    fn a_missing_host_ca_is_reported_before_an_expired_one() {
        let facts = facts_with_ca_expiring(SystemTime::now() - DAY);
        let st = compute_status(&servable(), &with_attestation(config()), &facts, 1);
        assert_eq!(ready_triple(&st).1, "HostCaMissing");
    }

    #[test]
    fn a_host_that_did_not_opt_in_never_reports_an_expired_ca() {
        let facts = facts_with_ca_expiring(SystemTime::now() - DAY);
        let st = compute_status(&servable(), &config(), &facts, 1);
        assert_ne!(ready_triple(&st).1, "HostCaExpired");
    }

    #[test]
    fn the_expiry_is_read_from_ca_crt_alone() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("ca");
        let ca = banlieue_attestation_ca::AttestationCa::create(&dir, "host-01", None).unwrap();
        let got = ca_not_after(&dir).expect("a readable certificate");
        let want = ca.not_after();
        let skew = got
            .duration_since(want)
            .or_else(|e| Ok::<_, ()>(e.duration()))
            .unwrap();
        assert!(skew < Duration::from_secs(2), "{skew:?}");
        // Without the key the certificate is still enough: the reconciler
        // never needs to read the private key to know when it expires.
        std::fs::remove_file(dir.join("ca.key")).unwrap();
        assert!(ca_not_after(&dir).is_some());
    }

    #[test]
    fn an_unreadable_or_absent_ca_certificate_has_no_expiry_to_report() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(ca_not_after(&t.path().join("nothing")), None);
        std::fs::write(t.path().join("ca.crt"), "not a certificate").unwrap();
        assert_eq!(ca_not_after(t.path()), None);
    }

    /// An `ekTrustBundle` alone anchors vTPM guests, not signed ones.
    #[test]
    fn an_ek_trust_bundle_alone_does_not_satisfy_the_opt_in() {
        let mut p = servable();
        p.spec.attestation = Some(ProviderAttestation {
            ek_trust_bundle: Some(inline_ca()),
            host_ca: None,
        });
        let st = compute_status(&p, &with_attestation(config()), &healthy(), 1);
        assert_eq!(ready_triple(&st).1, "HostCaMissing");
    }

    #[test]
    fn an_asserted_host_ca_makes_the_opted_in_host_ready() {
        let st = compute_status(
            &asserting(inline_ca()),
            &with_attestation(config()),
            &healthy(),
            1,
        );
        let (status, reason, _) = ready_triple(&st);
        assert_eq!((status.as_str(), reason.as_str()), ("True", "Reconciled"));
        assert_eq!(st.failure_domains.len(), 1);
    }

    /// Inline is the only source the provider can judge from the Provider
    /// alone; one that is not exactly one certificate is refused.
    #[test]
    fn an_invalid_inline_host_ca_is_unreadable() {
        let cert = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
        // Built from parts: a literal key header in a fixture trips the push scanner.
        let label = format!("{} {}", "PRIVATE", "KEY");
        let key = format!("-----BEGIN {label}-----\nAAAA\n-----END {label}-----\n");
        for inline in [
            format!("{cert}{cert}"),
            key.clone(),
            format!("{cert}{key}"),
            String::new(),
        ] {
            let source = CABundleSource {
                inline: Some(inline),
                ..Default::default()
            };
            let st = compute_status(
                &asserting(source),
                &with_attestation(config()),
                &healthy(),
                1,
            );
            let (status, reason, message) = ready_triple(&st);
            assert_eq!(
                (status.as_str(), reason.as_str()),
                ("False", "HostCaUnreadable")
            );
            assert!(message.contains("spec.attestation.hostCa"), "{message}");
            assert!(st.failure_domains.is_empty());
        }
    }

    /// ADR-0143 Decision 2: banlieue never verifies the anchor, and the
    /// shared role grants no ConfigMap or Secret read, so a reference counts
    /// as set and is not looked at.
    #[test]
    fn a_configmap_or_secret_source_counts_as_set_and_is_not_held() {
        let selector = KeySelector {
            name: "host-ca".into(),
            key: None,
        };
        for source in [
            CABundleSource {
                config_map_ref: Some(selector.clone()),
                ..Default::default()
            },
            CABundleSource {
                secret_ref: Some(selector.clone()),
                ..Default::default()
            },
        ] {
            let p = asserting(source);
            assert_eq!(host_ca_hold(&with_attestation(config()), &p), None);
            let st = compute_status(&p, &with_attestation(config()), &healthy(), 1);
            assert_eq!(ready_triple(&st).1, "Reconciled");
        }
    }

    /// The hold wins over host gaps and capability gaps: it is the first
    /// thing an operator must fix.
    #[test]
    fn the_hold_is_reported_before_host_gaps() {
        let mut facts = healthy();
        facts.kvm = false;
        let st = compute_status(&servable(), &with_attestation(config()), &facts, 1);
        assert_eq!(ready_triple(&st).1, "HostCaMissing");
    }

    /// The status with every transition time zeroed, so two passes compare.
    fn untimed(mut st: ProviderStatus) -> ProviderStatus {
        for c in &mut st.conditions {
            c.last_transition_time = k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                k8s_openapi::jiff::Timestamp::UNIX_EPOCH,
            );
        }
        st
    }

    /// Not opted in: nothing changes, whatever the Provider carries.
    #[test]
    fn a_host_that_did_not_opt_in_never_reports_a_host_ca_reason() {
        let plain = compute_status(&servable(), &config(), &healthy(), 1);
        assert_eq!(ready_triple(&plain).1, "Reconciled");
        let bad = CABundleSource {
            inline: Some("not a certificate".into()),
            ..Default::default()
        };
        for p in [servable(), asserting(inline_ca()), asserting(bad)] {
            let st = compute_status(&p, &config(), &healthy(), 1);
            assert!(!ready_triple(&st).1.starts_with("HostCa"));
            assert_eq!(untimed(st), untimed(plain.clone()), "byte for byte");
        }
    }

    #[test]
    fn the_hold_decision_is_pure_over_config_and_provider() {
        let opted = with_attestation(config());
        assert_eq!(host_ca_hold(&opted, &servable()), Some(HostCaHold::Missing));
        assert_eq!(host_ca_hold(&opted, &asserting(inline_ca())), None);
        let bad = CABundleSource {
            inline: Some("x".into()),
            ..Default::default()
        };
        assert!(matches!(
            host_ca_hold(&opted, &asserting(bad.clone())),
            Some(HostCaHold::Unreadable(_))
        ));
        assert_eq!(host_ca_hold(&config(), &servable()), None);
        assert_eq!(host_ca_hold(&config(), &asserting(bad)), None);
        assert_eq!(HostCaHold::Missing.reason(), "HostCaMissing");
        assert_eq!(
            HostCaHold::Unreadable(String::new()).reason(),
            "HostCaUnreadable"
        );
    }
}
