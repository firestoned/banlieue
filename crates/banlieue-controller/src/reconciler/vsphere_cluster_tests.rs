// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::vsphere_cluster`].

#[cfg(test)]
mod tests {
    use super::super::{aggregate_failure_domains, build_status, select_providers};

    use banlieue_api::banlieue::{
        FailureDomain, FailureDomainAttributes, Provider, ProviderCapabilities, ProviderConnection,
        ProviderSpec, ProviderStatus,
    };
    use banlieue_api::common::{ApiEndpoint, LabelSelector, LocalObjectReference};
    use banlieue_api::infrastructure::VSphereClusterSpec;
    use std::collections::BTreeMap;

    // ----------------------------------------------------------------------
    // Fixtures
    // ----------------------------------------------------------------------

    fn pairs(kvs: &[(&str, &str)]) -> BTreeMap<String, String> {
        kvs.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn make_fd(name: &str, labels: &[(&str, &str)], raw: &[(&str, &str)]) -> FailureDomain {
        FailureDomain {
            name: name.to_string(),
            labels: pairs(labels),
            attributes: FailureDomainAttributes {
                available_storage_classes: Vec::new(),
                available_network_classes: Vec::new(),
                features: Vec::new(),
                raw: pairs(raw),
            },
        }
    }

    fn make_provider(name: &str, labels: &[(&str, &str)], fds: Vec<FailureDomain>) -> Provider {
        let spec = ProviderSpec {
            provider_class_ref: LocalObjectReference {
                name: "vsphere".to_string(),
            },
            connection: ProviderConnection {
                endpoint: "https://vc.example.com/sdk".to_string(),
                credentials_ref: LocalObjectReference {
                    name: "creds".to_string(),
                },
                insecure_skip_tls_verify: false,
                ca_bundle: None,
            },
            capabilities: ProviderCapabilities::default(),
            paused: false,
        };
        let mut p = Provider::new(name, spec);
        if !labels.is_empty() {
            p.metadata.labels = Some(pairs(labels));
        }
        p.status = Some(ProviderStatus {
            failure_domains: fds,
            conditions: Vec::new(),
            observed_generation: None,
        });
        p
    }

    fn spec_with_refs(refs: &[&str]) -> VSphereClusterSpec {
        VSphereClusterSpec {
            control_plane_endpoint: None,
            provider_selector: LabelSelector::default(),
            provider_refs: refs
                .iter()
                .map(|n| LocalObjectReference {
                    name: n.to_string(),
                })
                .collect(),
            control_plane_failure_domain_selector: None,
            paused: false,
        }
    }

    // ----------------------------------------------------------------------
    // select_providers
    // ----------------------------------------------------------------------

    #[test]
    fn select_providers_explicit_refs_take_precedence() {
        let all = vec![
            make_provider("prod-vsphere", &[("env", "prod")], vec![]),
            make_provider("dr-vsphere", &[("env", "dr")], vec![]),
            make_provider("lab-vsphere", &[("env", "lab")], vec![]),
        ];
        // Refs select exactly two, ignoring the selector entirely.
        let mut spec = spec_with_refs(&["prod-vsphere", "lab-vsphere"]);
        spec.provider_selector = LabelSelector {
            match_labels: pairs(&[("env", "dr")]),
            match_expressions: Vec::new(),
        };
        let selected = select_providers(&all, &spec);
        let names: Vec<String> = selected
            .iter()
            .map(|p| p.metadata.name.clone().unwrap())
            .collect();
        assert_eq!(names, vec!["prod-vsphere", "lab-vsphere"]);
    }

    #[test]
    fn select_providers_by_label_selector() {
        let all = vec![
            make_provider("prod-vsphere", &[("env", "prod")], vec![]),
            make_provider("dr-vsphere", &[("env", "dr")], vec![]),
        ];
        let spec = VSphereClusterSpec {
            control_plane_endpoint: None,
            provider_selector: LabelSelector {
                match_labels: pairs(&[("env", "prod")]),
                match_expressions: Vec::new(),
            },
            provider_refs: Vec::new(),
            control_plane_failure_domain_selector: None,
            paused: false,
        };
        let selected = select_providers(&all, &spec);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].metadata.name.as_deref(), Some("prod-vsphere"));
    }

    #[test]
    fn select_providers_empty_selector_matches_all() {
        let all = vec![
            make_provider("a", &[], vec![]),
            make_provider("b", &[("env", "x")], vec![]),
        ];
        let spec = VSphereClusterSpec {
            control_plane_endpoint: None,
            provider_selector: LabelSelector::default(),
            provider_refs: Vec::new(),
            control_plane_failure_domain_selector: None,
            paused: false,
        };
        assert_eq!(select_providers(&all, &spec).len(), 2);
    }

    // ----------------------------------------------------------------------
    // aggregate_failure_domains
    // ----------------------------------------------------------------------

    #[test]
    fn aggregate_translates_fds_and_merges_attributes() {
        let p = make_provider(
            "prod-vsphere",
            &[],
            vec![make_fd(
                "prod-vsphere-dc0-c0",
                &[("dc", "DC0"), ("cluster", "C0")],
                &[("datacenter", "DC0"), ("resourcePool", "rp-1")],
            )],
        );
        let refs = vec![&p];
        let fds = aggregate_failure_domains(&refs, None);
        assert_eq!(fds.len(), 1);
        assert_eq!(fds[0].name, "prod-vsphere-dc0-c0");
        // labels + raw merged; raw `datacenter`/`resourcePool` and label `dc`/`cluster` all present.
        assert_eq!(fds[0].attributes.get("dc").map(String::as_str), Some("DC0"));
        assert_eq!(
            fds[0].attributes.get("datacenter").map(String::as_str),
            Some("DC0")
        );
        assert_eq!(
            fds[0].attributes.get("resourcePool").map(String::as_str),
            Some("rp-1")
        );
    }

    #[test]
    fn aggregate_no_selector_marks_all_control_plane_eligible() {
        let p = make_provider(
            "p",
            &[],
            vec![
                make_fd("fd-a", &[("dc", "DC0")], &[]),
                make_fd("fd-b", &[("dc", "DC1")], &[]),
            ],
        );
        let refs = vec![&p];
        let fds = aggregate_failure_domains(&refs, None);
        assert!(fds.iter().all(|fd| fd.control_plane == Some(true)));
    }

    #[test]
    fn aggregate_control_plane_selector_marks_only_matching_subset() {
        let p = make_provider(
            "p",
            &[],
            vec![
                make_fd("fd-a", &[("role", "cp")], &[]),
                make_fd("fd-b", &[("role", "worker")], &[]),
            ],
        );
        let refs = vec![&p];
        let selector = LabelSelector {
            match_labels: pairs(&[("role", "cp")]),
            match_expressions: Vec::new(),
        };
        let fds = aggregate_failure_domains(&refs, Some(&selector));
        let by_name: BTreeMap<&str, Option<bool>> = fds
            .iter()
            .map(|fd| (fd.name.as_str(), fd.control_plane))
            .collect();
        assert_eq!(by_name["fd-a"], Some(true));
        assert_eq!(by_name["fd-b"], Some(false));
    }

    #[test]
    fn aggregate_spans_multiple_providers_and_skips_status_less_ones() {
        let p1 = make_provider("prod", &[], vec![make_fd("prod-c0", &[("dc", "D0")], &[])]);
        let p2 = make_provider(
            "dr",
            &[],
            vec![
                make_fd("dr-c0", &[("dc", "D1")], &[]),
                make_fd("dr-c1", &[("dc", "D1")], &[]),
            ],
        );
        // A provider with no status contributes nothing.
        let p3 = Provider::new(
            "pending",
            ProviderSpec {
                provider_class_ref: LocalObjectReference {
                    name: "vsphere".to_string(),
                },
                connection: ProviderConnection {
                    endpoint: "https://x/sdk".to_string(),
                    credentials_ref: LocalObjectReference {
                        name: "c".to_string(),
                    },
                    insecure_skip_tls_verify: false,
                    ca_bundle: None,
                },
                capabilities: ProviderCapabilities::default(),
                paused: false,
            },
        );
        let refs = vec![&p1, &p2, &p3];
        let fds = aggregate_failure_domains(&refs, None);
        // 1 (prod) + 2 (dr) + 0 (pending) = 3 — one cluster spanning two vCenters.
        assert_eq!(fds.len(), 3);
    }

    // ----------------------------------------------------------------------
    // build_status
    // ----------------------------------------------------------------------

    #[test]
    fn build_status_provisioned_when_failure_domains_present() {
        let all = vec![make_provider(
            "prod",
            &[],
            vec![make_fd("prod-c0", &[("dc", "D0")], &[])],
        )];
        let spec = VSphereClusterSpec {
            control_plane_endpoint: Some(ApiEndpoint {
                host: "10.0.0.10".to_string(),
                port: 6443,
            }),
            provider_selector: LabelSelector::default(),
            provider_refs: Vec::new(),
            control_plane_failure_domain_selector: None,
            paused: false,
        };
        let status = build_status(&spec, &all, 7);
        assert_eq!(status.initialization.provisioned, Some(true));
        assert_eq!(status.failure_domains.len(), 1);
        assert_eq!(status.observed_generation, Some(7));
        // controlPlaneEndpoint echoed from spec.
        assert_eq!(status.control_plane_endpoint.as_ref().unwrap().port, 6443);
        let ready = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .unwrap();
        assert_eq!(ready.status, "True");
        assert_eq!(ready.reason, "Reconciled");
    }

    #[test]
    fn build_status_not_provisioned_when_no_failure_domains() {
        // Selector matches no provider → zero FDs.
        let all = vec![make_provider("prod", &[("env", "prod")], vec![])];
        let spec = VSphereClusterSpec {
            control_plane_endpoint: None,
            provider_selector: LabelSelector {
                match_labels: pairs(&[("env", "nope")]),
                match_expressions: Vec::new(),
            },
            provider_refs: Vec::new(),
            control_plane_failure_domain_selector: None,
            paused: false,
        };
        let status = build_status(&spec, &all, 1);
        assert_eq!(status.initialization.provisioned, Some(false));
        assert!(status.failure_domains.is_empty());
        let ready = status
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "NoFailureDomains");
    }
}
