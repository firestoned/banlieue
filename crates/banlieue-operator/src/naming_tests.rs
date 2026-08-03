// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `naming.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    // ----------------------------------------------------------------------
    // workload_name
    // ----------------------------------------------------------------------

    #[test]
    fn workload_name_follows_the_documented_convention() {
        assert_eq!(
            workload_name("vsphere", "prod-vc"),
            "banlieue-provider-vsphere-prod-vc"
        );
    }

    /// Derived names must be a pure function of (class, provider): the operator
    /// recomputes them on every reconcile and must land on the same object
    /// rather than orphaning the previous one.
    #[test]
    fn workload_name_is_deterministic() {
        assert_eq!(
            workload_name("vsphere", "prod-vc"),
            workload_name("vsphere", "prod-vc")
        );
    }

    #[test]
    fn workload_name_distinguishes_providers_of_the_same_class() {
        assert_ne!(
            workload_name("vsphere", "prod-vc"),
            workload_name("vsphere", "dr-vc")
        );
    }

    #[test]
    fn workload_name_distinguishes_classes_with_the_same_provider_name() {
        assert_ne!(
            workload_name("vsphere", "prod"),
            workload_name("vsphere-canary", "prod")
        );
    }

    /// Object names longer than 63 characters break label values and, for
    /// Deployments, the generated ReplicaSet/pod names. Long inputs are
    /// truncated with a hash suffix rather than emitting an invalid name.
    #[test]
    fn workload_name_is_capped_at_the_kubernetes_limit() {
        let long_provider = "a".repeat(200);
        let name = workload_name("vsphere", &long_provider);
        assert!(
            name.len() <= MAX_NAME_LEN,
            "expected <= {MAX_NAME_LEN}, got {}",
            name.len()
        );
    }

    #[test]
    fn truncated_workload_names_stay_distinct_and_deterministic() {
        let a = workload_name("vsphere", &format!("{}-one", "x".repeat(200)));
        let b = workload_name("vsphere", &format!("{}-two", "x".repeat(200)));
        assert_ne!(a, b, "hash suffix must disambiguate truncated names");
        assert_eq!(
            a,
            workload_name("vsphere", &format!("{}-one", "x".repeat(200)))
        );
    }

    /// A truncated name must still be a valid DNS-1123 label: lowercase
    /// alphanumerics and '-', starting and ending with an alphanumeric.
    #[test]
    fn truncated_workload_name_is_a_valid_dns_label() {
        let name = workload_name("vsphere", &"a".repeat(200));
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "unexpected characters in {name}"
        );
        assert!(!name.starts_with('-') && !name.ends_with('-'), "{name}");
    }

    // ----------------------------------------------------------------------
    // component
    // ----------------------------------------------------------------------

    #[test]
    fn component_names_the_backend_role() {
        assert_eq!(component("vsphere"), "provider-vsphere");
        assert_eq!(component("libvirt"), "provider-libvirt");
    }

    // ----------------------------------------------------------------------
    // labels
    // ----------------------------------------------------------------------

    #[test]
    fn workload_labels_carry_provider_class_and_management_provenance() {
        let labels = workload_labels("vsphere", "prod-vc", "vsphere");
        assert_eq!(labels.get(LABEL_NAME).unwrap(), APP_NAME);
        assert_eq!(labels.get(LABEL_COMPONENT).unwrap(), "provider-vsphere");
        assert_eq!(labels.get(LABEL_MANAGED_BY).unwrap(), MANAGED_BY);
        assert_eq!(labels.get(LABEL_PROVIDER).unwrap(), "prod-vc");
        assert_eq!(labels.get(LABEL_PROVIDER_CLASS).unwrap(), "vsphere");
        assert_eq!(
            labels.get(LABEL_INSTANCE).unwrap(),
            "banlieue-provider-vsphere-prod-vc"
        );
    }

    /// `Deployment.spec.selector` is immutable after creation, so it must be
    /// built from values that cannot change for a given Provider. The provider
    /// name is such a value; the class and backend are not necessarily.
    #[test]
    fn selector_labels_are_minimal_and_immutable() {
        let selector = selector_labels("prod-vc");
        assert_eq!(selector.len(), 2);
        assert_eq!(selector.get(LABEL_NAME).unwrap(), APP_NAME);
        assert_eq!(selector.get(LABEL_PROVIDER).unwrap(), "prod-vc");
    }

    /// Every selector entry must also be present on the pod template, or the
    /// Deployment selects nothing and the apiserver rejects it.
    #[test]
    fn selector_labels_are_a_subset_of_workload_labels() {
        let labels = workload_labels("vsphere", "prod-vc", "vsphere");
        for (key, value) in selector_labels("prod-vc") {
            assert_eq!(
                labels.get(&key),
                Some(&value),
                "selector key {key} missing from workload labels"
            );
        }
    }

    // ----------------------------------------------------------------------
    // provider_selector
    // ----------------------------------------------------------------------

    #[test]
    fn provider_selector_renders_a_label_selector_expression() {
        assert_eq!(provider_selector("prod-vc"), "banlieue.io/provider=prod-vc");
    }

    // ----------------------------------------------------------------------
    // cluster_scoped_name — cluster-scoped objects need namespace disambiguation
    // ----------------------------------------------------------------------

    /// A ClusterRoleBinding is cluster-scoped, so two Providers with the SAME
    /// name and class in DIFFERENT namespaces would otherwise collide on one
    /// object and fight over its subject — last writer wins, and the loser
    /// silently loses its permissions. The operator watches all namespaces by
    /// default, so this is reachable in any multi-tenant install.
    #[test]
    fn cluster_scoped_names_differ_across_namespaces() {
        assert_ne!(
            cluster_scoped_name("vsphere", "tenant-a", "prod-vc"),
            cluster_scoped_name("vsphere", "tenant-b", "prod-vc"),
            "same provider name in two namespaces must not share a cluster-scoped object"
        );
    }

    #[test]
    fn cluster_scoped_name_includes_class_namespace_and_provider() {
        let name = cluster_scoped_name("vsphere", "tenant-a", "prod-vc");
        assert!(name.starts_with(WORKLOAD_NAME_PREFIX), "{name}");
        for part in ["vsphere", "tenant-a", "prod-vc"] {
            assert!(name.contains(part), "{name} should mention {part}");
        }
    }

    #[test]
    fn cluster_scoped_name_is_deterministic_and_capped() {
        let a = cluster_scoped_name("vsphere", "tenant-a", "prod-vc");
        assert_eq!(a, cluster_scoped_name("vsphere", "tenant-a", "prod-vc"));

        let long = cluster_scoped_name("vsphere", &"n".repeat(120), &"p".repeat(120));
        assert!(long.len() <= MAX_NAME_LEN, "got {}", long.len());
        assert!(!long.ends_with('-'), "{long}");
    }

    /// Namespaced objects keep the shorter name — they are already scoped by
    /// their namespace, so adding it would be redundant noise.
    #[test]
    fn namespaced_and_cluster_scoped_names_are_distinct() {
        assert_ne!(
            workload_name("vsphere", "prod-vc"),
            cluster_scoped_name("vsphere", "tenant-a", "prod-vc")
        );
    }

    // ----------------------------------------------------------------------
    // Provider-namespace label — lets cluster-scoped objects be selected exactly
    // ----------------------------------------------------------------------

    #[test]
    fn workload_labels_record_the_provider_namespace() {
        let labels = workload_labels_for("vsphere", "tenant-a", "prod-vc", "vsphere");
        assert_eq!(labels.get(LABEL_PROVIDER).unwrap(), "prod-vc");
        assert_eq!(labels.get(LABEL_PROVIDER_NAMESPACE).unwrap(), "tenant-a");
    }

    /// Pruning orphans after a class change selects by provider identity, which
    /// must be exact cluster-wide or one tenant's prune deletes another's
    /// workload.
    #[test]
    fn owned_by_selector_pins_both_name_and_namespace() {
        let selector = owned_by_selector("tenant-a", "prod-vc");
        assert!(
            selector.contains("banlieue.io/provider=prod-vc"),
            "{selector}"
        );
        assert!(
            selector.contains("banlieue.io/provider-namespace=tenant-a"),
            "{selector}"
        );
    }
}
