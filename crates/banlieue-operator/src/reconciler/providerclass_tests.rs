// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `reconciler/providerclass.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{
        ImagePullPolicy, LoggingSpec, Provider, ProviderClassSpec, ProviderConnection,
        ProviderImage, ProviderSpec,
    };
    use banlieue_api::common::LocalObjectReference;

    fn class(backend: &str, tag: &str) -> ProviderClassSpec {
        ProviderClassSpec {
            backend: backend.to_string(),
            image: ProviderImage {
                repository: "ghcr.io/firestoned/banlieue".to_string(),
                tag: tag.to_string(),
                digest: None,
                pull_policy: Some(ImagePullPolicy::IfNotPresent),
                pull_secrets: Vec::new(),
            },
            workload_namespace: None,
            replicas: None,
            resources: None,
            node_selector: Default::default(),
            tolerations: Vec::new(),
            logging: LoggingSpec::default(),
            additional_rules: Vec::new(),
            paused: false,
        }
    }

    fn provider_named(name: &str, class_name: &str) -> Provider {
        let mut p = Provider::new(
            name,
            ProviderSpec {
                provider_class_ref: LocalObjectReference {
                    name: class_name.to_string(),
                },
                connection: ProviderConnection {
                    endpoint: "https://vcenter.invalid/sdk".to_string(),
                    credentials_ref: LocalObjectReference {
                        name: "creds".to_string(),
                    },
                    insecure_skip_tls_verify: false,
                    ca_bundle: None,
                },
                capabilities: Default::default(),
                paused: false,
            },
        );
        p.metadata.namespace = Some("banlieue-system".to_string());
        p
    }

    // ----------------------------------------------------------------------
    // count_providers
    // ----------------------------------------------------------------------

    #[test]
    fn counts_only_providers_referencing_this_class() {
        let providers = vec![
            provider_named("a", "vsphere"),
            provider_named("b", "vsphere"),
            provider_named("c", "libvirt"),
        ];
        assert_eq!(count_providers(&providers, "vsphere"), 2);
        assert_eq!(count_providers(&providers, "libvirt"), 1);
    }

    #[test]
    fn an_unreferenced_class_counts_zero_not_absent() {
        // Zero must be reported, not omitted: a blank column is indistinguishable
        // from "not reconciled yet", which is the bug this field exists to avoid.
        assert_eq!(count_providers(&[], "vsphere"), 0);
    }

    #[test]
    fn counts_providers_across_namespaces() {
        let mut a = provider_named("same-name", "vsphere");
        a.metadata.namespace = Some("tenant-a".to_string());
        let mut b = provider_named("same-name", "vsphere");
        b.metadata.namespace = Some("tenant-b".to_string());
        assert_eq!(count_providers(&[a, b], "vsphere"), 2);
    }

    // ----------------------------------------------------------------------
    // assess — is this class actually usable?
    // ----------------------------------------------------------------------

    /// The shared per-backend ClusterRole is a hard prerequisite: the operator
    /// binds it but cannot create it, so without it every workload of this class
    /// runs with no permissions. That was bug-110, and a class-level condition
    /// makes it visible in `kubectl get providerclasses` instead of only in a
    /// provider pod's 403s.
    #[test]
    fn a_class_whose_cluster_role_is_missing_is_not_ready() {
        let assessment = assess(&class("vsphere", "v0.1.0"), false);
        assert_eq!(assessment, ClassReadiness::MissingClusterRole);
        assert!(!assessment.is_ready());
        assert!(
            assessment
                .message("vsphere")
                .contains("banlieue-provider-vsphere"),
            "message should name the missing ClusterRole outright, not a placeholder: {}",
            assessment.message("vsphere")
        );
    }

    #[test]
    fn a_class_with_an_empty_image_tag_is_not_ready() {
        let assessment = assess(&class("vsphere", ""), true);
        assert_eq!(assessment, ClassReadiness::InvalidImage);
        assert!(!assessment.is_ready());
    }

    #[test]
    fn a_complete_class_is_ready() {
        let assessment = assess(&class("vsphere", "v0.1.0"), true);
        assert_eq!(assessment, ClassReadiness::Ready);
        assert!(assessment.is_ready());
    }

    /// A missing ClusterRole is the more actionable failure, so it is reported
    /// even when the image is also wrong — fixing the image alone would still
    /// leave the class unusable.
    #[test]
    fn a_missing_cluster_role_is_reported_ahead_of_a_bad_image() {
        assert_eq!(
            assess(&class("vsphere", ""), false),
            ClassReadiness::MissingClusterRole
        );
    }

    /// Condition reasons are matched by dashboards and alert rules, so they must
    /// be a closed set of stable identifiers rather than free-form prose.
    #[test]
    fn reasons_are_stable_identifiers() {
        for (assessment, expected) in [
            (ClassReadiness::Ready, "Ready"),
            (ClassReadiness::MissingClusterRole, "MissingClusterRole"),
            (ClassReadiness::InvalidImage, "InvalidImage"),
        ] {
            assert_eq!(assessment.reason(), expected);
            assert!(
                !assessment.reason().contains(' '),
                "a reason must be a single identifier"
            );
        }
    }

    #[test]
    fn the_shared_cluster_role_name_matches_what_the_workload_binds() {
        assert_eq!(
            crate::workload::shared_cluster_role_name("vsphere"),
            "banlieue-provider-vsphere",
            "assess() checks for this exact name, so the two must not drift"
        );
    }
}
