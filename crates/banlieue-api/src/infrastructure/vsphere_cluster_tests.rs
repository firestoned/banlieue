// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `vsphere_cluster.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::collections::BTreeMap;

    // ----------------------------------------------------------------------
    // Spec defaults / serialization
    // ----------------------------------------------------------------------

    #[test]
    fn minimal_spec_omits_all_optional_fields() {
        // A VSphereCluster that selects everything (empty selector) and sets no
        // endpoint should serialize to an empty object — every field is either
        // skipped-when-empty or defaulted-on-read.
        let spec = VSphereClusterSpec {
            control_plane_endpoint: None,
            provider_selector: LabelSelector::default(),
            provider_refs: Vec::new(),
            control_plane_failure_domain_selector: None,
            paused: false,
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json, serde_json::json!({}));
        let back: VSphereClusterSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn empty_object_deserializes_to_default_spec() {
        let spec: VSphereClusterSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(spec.control_plane_endpoint, None);
        assert!(spec.provider_refs.is_empty());
        assert!(!spec.paused);
    }

    #[test]
    fn explicit_provider_refs_round_trip() {
        let spec = VSphereClusterSpec {
            control_plane_endpoint: Some(ApiEndpoint {
                host: "10.0.0.10".to_string(),
                port: 6443,
            }),
            provider_selector: LabelSelector::default(),
            provider_refs: vec![
                LocalObjectReference {
                    name: "prod-vsphere".to_string(),
                },
                LocalObjectReference {
                    name: "dr-vsphere".to_string(),
                },
            ],
            control_plane_failure_domain_selector: None,
            paused: false,
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "controlPlaneEndpoint": { "host": "10.0.0.10", "port": 6443 },
                "providerRefs": [ { "name": "prod-vsphere" }, { "name": "dr-vsphere" } ]
            })
        );
        let back: VSphereClusterSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn provider_selector_serializes_when_set() {
        let mut match_labels = BTreeMap::new();
        match_labels.insert("env".to_string(), "prod".to_string());
        let spec = VSphereClusterSpec {
            control_plane_endpoint: None,
            provider_selector: LabelSelector {
                match_labels,
                match_expressions: Vec::new(),
            },
            provider_refs: Vec::new(),
            control_plane_failure_domain_selector: None,
            paused: false,
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "providerSelector": { "matchLabels": { "env": "prod" } } })
        );
    }

    #[test]
    fn paused_true_serializes_and_false_is_skipped() {
        let paused = VSphereClusterSpec {
            control_plane_endpoint: None,
            provider_selector: LabelSelector::default(),
            provider_refs: Vec::new(),
            control_plane_failure_domain_selector: None,
            paused: true,
        };
        assert_eq!(
            serde_json::to_value(&paused).unwrap(),
            serde_json::json!({ "paused": true })
        );
    }

    #[test]
    fn control_plane_endpoint_uses_camel_case() {
        let spec = VSphereClusterSpec {
            control_plane_endpoint: Some(ApiEndpoint {
                host: "h".to_string(),
                port: 6443,
            }),
            provider_selector: LabelSelector::default(),
            provider_refs: Vec::new(),
            control_plane_failure_domain_selector: None,
            paused: false,
        };
        let s = serde_json::to_string(&spec).unwrap();
        assert!(s.contains("\"controlPlaneEndpoint\""), "got: {s}");
        assert!(!s.contains("control_plane_endpoint"), "got: {s}");
    }

    // ----------------------------------------------------------------------
    // Status — CAPI v1beta2 contract shape
    // ----------------------------------------------------------------------

    #[test]
    fn default_status_serializes_empty() {
        let status = VSphereClusterStatus::default();
        let json = serde_json::to_value(&status).unwrap();
        // `initialization` is not skipped (matches VSphereMachineStatus), so an
        // empty initialization object is present.
        assert_eq!(json, serde_json::json!({ "initialization": {} }));
    }

    #[test]
    fn status_failure_domains_are_a_list() {
        let mut attributes = BTreeMap::new();
        attributes.insert("datacenter".to_string(), "DC0".to_string());
        attributes.insert("cluster".to_string(), "C0".to_string());
        let status = VSphereClusterStatus {
            initialization: InitializationStatus {
                provisioned: Some(true),
            },
            control_plane_endpoint: Some(ApiEndpoint {
                host: "10.0.0.10".to_string(),
                port: 6443,
            }),
            failure_domains: vec![
                ClusterFailureDomain {
                    name: "prod-vsphere-dc0-c0".to_string(),
                    control_plane: Some(true),
                    attributes: attributes.clone(),
                },
                ClusterFailureDomain {
                    name: "dr-vsphere-dc1-c2".to_string(),
                    control_plane: Some(false),
                    attributes: BTreeMap::new(),
                },
            ],
            conditions: Vec::new(),
            observed_generation: Some(3),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert!(json["failureDomains"].is_array(), "got: {json}");
        assert_eq!(json["failureDomains"].as_array().unwrap().len(), 2);
        assert_eq!(json["failureDomains"][0]["controlPlane"], true);
        assert_eq!(json["controlPlaneEndpoint"]["port"], 6443);
        let back: VSphereClusterStatus = serde_json::from_value(json).unwrap();
        assert_eq!(back, status);
    }

    // ----------------------------------------------------------------------
    // CRD generation
    // ----------------------------------------------------------------------

    #[test]
    fn vsphere_cluster_crd_metadata_matches_kube_attributes() {
        use kube::CustomResourceExt;
        let crd = VSphereCluster::crd();
        assert_eq!(crd.spec.group, "infrastructure.banlieue.io");
        assert_eq!(crd.spec.names.kind, "VSphereCluster");
        assert_eq!(crd.spec.names.plural, "vsphereclusters");
        assert_eq!(crd.spec.scope, "Namespaced");
        assert!(
            crd.spec
                .versions
                .iter()
                .any(|v| v.name == "v1alpha1" && v.served && v.storage)
        );
    }

    #[test]
    fn vsphere_cluster_crd_advertises_shortname() {
        use kube::CustomResourceExt;
        let crd = VSphereCluster::crd();
        let short_names = crd.spec.names.short_names.unwrap_or_default();
        assert!(
            short_names.contains(&"vsc".to_string()),
            "got: {short_names:?}"
        );
    }
}
