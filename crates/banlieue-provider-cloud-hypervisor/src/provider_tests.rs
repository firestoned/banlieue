// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `provider.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{
        NetworkClassMapping, ProviderCapabilities, ProviderConnection, ProviderSpec,
        StorageClassMapping,
    };
    use banlieue_api::common::LocalObjectReference;
    use kube::api::ObjectMeta;

    fn config() -> HostConfig {
        HostConfig::parse(
            r#"
[provider]
name = "ch-a"
namespace = "banlieue-system"
kubeconfig = "/etc/banlieue/kubeconfig"
[vmm]
binary = "/usr/local/bin/cloud-hypervisor"
version = "v53.0"
firmware = "/opt/banlieue/firmware/f/CLOUDHV.fd"
[paths]
run_root = "/run/banlieue/ch"
state_root = "/var/lib/banlieue"
[guests]
uid_base = 2000000
uid_count = 10
[storage_classes]
fast = "/srv/banlieue/ch"
[network_classes]
lan = "br0"
"#,
        )
        .unwrap()
    }

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
            capacity: HostCapacity {
                cpus: Some(32),
                cpu_model: Some("Intel(R) Xeon(R) CPU E5-2630 v3 @ 2.40GHz".into()),
                memory_mib: Some(64_318),
                hugepages_mib: Some(0),
                storage_free_gib: [("fast".to_string(), 812)].into(),
            },
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
        assert_eq!(get("storageFreeGiB"), Some("fast=812"));
        assert_eq!(get("nestedVirtualization"), Some("false"));
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
}
