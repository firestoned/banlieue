// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::provider_lookup`] (ADR-0133 Decisions 1, 2, 11).

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use banlieue_api::banlieue::{
        Provider, ProviderCapabilities, ProviderConnection, ProviderSpec, VirtualMachine,
    };
    use banlieue_api::common::{LabelSelector, LocalObjectReference};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    use super::super::*;

    const TENANT: &str = "team-a";
    const PLATFORM: &str = "banlieue-system";

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn selector(pairs: &[(&str, &str)]) -> LabelSelector {
        LabelSelector {
            match_labels: labels(pairs),
            ..Default::default()
        }
    }

    fn provider(
        name: &str,
        namespace: &str,
        class: &str,
        namespace_selector: Option<LabelSelector>,
    ) -> Provider {
        Provider {
            metadata: ObjectMeta {
                name: Some(name.into()),
                namespace: Some(namespace.into()),
                ..Default::default()
            },
            spec: ProviderSpec {
                provider_class_ref: LocalObjectReference { name: class.into() },
                connection: ProviderConnection {
                    endpoint: "https://vcenter.example.com/sdk".into(),
                    credentials_ref: Some(LocalObjectReference {
                        name: "creds".into(),
                    }),
                    insecure_skip_tls_verify: false,
                    ca_bundle: None,
                },
                capabilities: ProviderCapabilities::default(),
                paused: false,
                use_content_library: false,
                failure_domain_name_overrides: Vec::new(),
                attestation: None,
                scheduling: None,
                namespace_selector,
            },
            status: None,
        }
    }

    fn names(c: &Candidates) -> Vec<String> {
        c.providers
            .iter()
            .map(|p| p.metadata.name.clone().unwrap_or_default())
            .collect()
    }

    fn tenant_labels() -> BTreeMap<String, String> {
        labels(&[("banlieue.io/tenant", "true")])
    }

    #[test]
    fn a_namespace_with_its_own_provider_uses_only_those() {
        let local = vec![provider("own", TENANT, "vsphere", None)];
        let platform = vec![provider(
            "shared",
            PLATFORM,
            "vsphere",
            Some(selector(&[("banlieue.io/tenant", "true")])),
        )];
        let c = select_candidates(local, platform, &tenant_labels());
        assert_eq!(c.source, CandidateSource::VmNamespace);
        assert_eq!(names(&c), vec!["own"]);
    }

    /// A namespace that runs its own Provider has chosen its own backend; a
    /// selector that matches nothing there must not spill onto the platform.
    #[test]
    fn a_namespace_with_its_own_provider_never_falls_back() {
        let local = vec![provider("own", TENANT, "libvirt", None)];
        let platform = vec![provider(
            "shared",
            PLATFORM,
            "vsphere",
            Some(selector(&[("banlieue.io/tenant", "true")])),
        )];
        let c = select_candidates(local, platform, &tenant_labels());
        assert_eq!(names(&c), vec!["own"]);
        assert!(!names(&c).contains(&"shared".to_string()));
    }

    #[test]
    fn a_namespace_with_no_provider_falls_back_to_a_declaring_one() {
        let platform = vec![provider(
            "shared",
            PLATFORM,
            "vsphere",
            Some(selector(&[("banlieue.io/tenant", "true")])),
        )];
        let c = select_candidates(Vec::new(), platform, &tenant_labels());
        assert_eq!(c.source, CandidateSource::ProviderNamespace);
        assert_eq!(names(&c), vec!["shared"]);
    }

    /// No `namespaceSelector` means "my own namespace only", so such a
    /// Provider is never anyone's fallback.
    #[test]
    fn a_provider_without_a_namespace_selector_is_never_a_fallback() {
        let platform = vec![provider("shared", PLATFORM, "vsphere", None)];
        let c = select_candidates(Vec::new(), platform, &tenant_labels());
        assert!(c.providers.is_empty());
        assert_eq!(c.source, CandidateSource::None);
    }

    #[test]
    fn a_selector_that_does_not_match_the_namespace_excludes_the_provider() {
        let platform = vec![provider(
            "shared",
            PLATFORM,
            "vsphere",
            Some(selector(&[("team", "other")])),
        )];
        let c = select_candidates(Vec::new(), platform, &tenant_labels());
        assert!(c.providers.is_empty());
    }

    #[test]
    fn only_the_declaring_providers_among_several_are_kept() {
        let platform = vec![
            provider(
                "yes",
                PLATFORM,
                "vsphere",
                Some(selector(&[("banlieue.io/tenant", "true")])),
            ),
            provider("no", PLATFORM, "vsphere", Some(selector(&[("x", "y")]))),
            provider("unset", PLATFORM, "vsphere", None),
        ];
        let c = select_candidates(Vec::new(), platform, &tenant_labels());
        assert_eq!(names(&c), vec!["yes"]);
    }

    /// A class whose binary does not yet accept a foreign reference must not be
    /// offered one (ADR-0133 Decision 10): nothing would reconcile the machine.
    #[test]
    fn a_class_that_cannot_serve_a_foreign_reference_is_excluded() {
        let platform = vec![
            provider(
                "fc",
                PLATFORM,
                "firecracker",
                Some(selector(&[("banlieue.io/tenant", "true")])),
            ),
            provider(
                "vc",
                PLATFORM,
                "vsphere",
                Some(selector(&[("banlieue.io/tenant", "true")])),
            ),
        ];
        let c = select_candidates(Vec::new(), platform, &tenant_labels());
        assert_eq!(names(&c), vec!["vc"]);
    }

    #[test]
    fn every_implemented_class_is_offered_a_foreign_reference() {
        for class in ["vsphere", "libvirt", "proxmox", "cloud-hypervisor"] {
            let platform = vec![provider(
                "shared",
                PLATFORM,
                class,
                Some(selector(&[("banlieue.io/tenant", "true")])),
            )];
            let c = select_candidates(Vec::new(), platform, &tenant_labels());
            assert_eq!(names(&c), vec!["shared"], "{class}");
        }
    }

    #[test]
    fn supports_shared_provider_matches_the_sdk_list() {
        for class in ["vsphere", "libvirt", "proxmox", "cloud-hypervisor"] {
            assert!(supports_shared_provider(class), "{class}");
        }
        assert!(!supports_shared_provider("firecracker"));
        assert!(!supports_shared_provider(""));
    }

    /// `{}` is Kubernetes' "every namespace", but banlieue never defaults to
    /// every namespace (ADR-0130/0131): an empty selector serves nothing, the
    /// same as on the provider, which cannot render it as a flag.
    #[test]
    fn an_empty_selector_serves_no_namespace() {
        let platform = vec![provider(
            "shared",
            PLATFORM,
            "vsphere",
            Some(LabelSelector::default()),
        )];
        let c = select_candidates(Vec::new(), platform, &tenant_labels());
        assert!(c.providers.is_empty());
        assert_eq!(c.source, CandidateSource::None);
    }

    #[test]
    fn nothing_anywhere_yields_no_candidates() {
        let c = select_candidates(Vec::new(), Vec::new(), &tenant_labels());
        assert!(c.providers.is_empty());
        assert_eq!(c.source, CandidateSource::None);
    }

    // ---- when to look in the provider namespace at all -------------------

    #[test]
    fn the_fallback_is_looked_up_only_when_the_vm_namespace_has_no_provider() {
        assert!(should_try_provider_namespace(0, Some(PLATFORM), TENANT));
        assert!(!should_try_provider_namespace(1, Some(PLATFORM), TENANT));
    }

    /// Unset means no fallback — never "search every namespace" (Decision 2).
    #[test]
    fn an_unset_provider_namespace_means_no_fallback() {
        assert!(!should_try_provider_namespace(0, None, TENANT));
        assert!(!should_try_provider_namespace(0, Some(""), TENANT));
    }

    /// A VM that already lives in the provider namespace has been looked at.
    #[test]
    fn a_vm_in_the_provider_namespace_has_nothing_to_fall_back_to() {
        assert!(!should_try_provider_namespace(0, Some(PLATFORM), PLATFORM));
    }

    // ---- placement peers (capacity accounting) ---------------------------

    fn vm_in(namespace: &str, name: &str) -> VirtualMachine {
        let mut vm: VirtualMachine = serde_json::from_value(serde_json::json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "VirtualMachine",
            "metadata": { "name": name },
            "spec": { "classRef": { "name": "c" }, "imageRef": { "name": "i" } }
        }))
        .expect("VirtualMachine fixture");
        vm.metadata.namespace = Some(namespace.into());
        vm
    }

    /// A shared provider's load is every tenant's VMs on it, so the peers are
    /// listed cluster-wide; only the namespaces the controller serves count.
    #[test]
    fn peers_are_limited_to_served_namespaces() {
        let vms = vec![
            vm_in("team-a", "a"),
            vm_in("team-b", "b"),
            vm_in("unserved", "c"),
        ];
        let kept = retain_served(vms, |ns| ns != "unserved");
        let names: Vec<_> = kept
            .iter()
            .map(|v| v.metadata.name.clone().unwrap())
            .collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn peers_in_no_namespace_are_dropped() {
        let mut orphan = vm_in("x", "x");
        orphan.metadata.namespace = None;
        assert!(retain_served(vec![orphan], |_| true).is_empty());
    }
}
