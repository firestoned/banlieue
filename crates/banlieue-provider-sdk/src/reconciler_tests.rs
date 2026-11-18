// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::reconciler`].

#[cfg(test)]
mod tests {
    use super::super::*;

    // The Action type is opaque (Debug only, no Eq), so we assert that the
    // constructors are callable and produce different values. This guards
    // against accidental swapping of the constants without much ceremony.
    #[test]
    fn helpers_are_callable_and_distinct_debug() {
        let default = format!("{:?}", requeue_default());
        let error = format!("{:?}", requeue_on_error());
        let long = format!("{:?}", requeue_long());
        let none = format!("{:?}", no_requeue());

        assert_ne!(default, error);
        assert_ne!(default, long);
        assert_ne!(default, none);
        assert_ne!(error, none);
    }

    #[test]
    fn requeue_intervals_are_monotonic() {
        const _: () = assert!(REQUEUE_ON_ERROR_SECS < REQUEUE_DEFAULT_SECS);
        const _: () = assert!(REQUEUE_DEFAULT_SECS < REQUEUE_LONG_SECS);
    }

    // --- serves --------------------------------------------------------------

    #[test]
    fn a_provider_serves_only_its_own_machines() {
        assert!(serves(Some("kvm-b"), "kvm-b"));
        assert!(!serves(Some("kvm-a"), "kvm-b"));
    }

    /// A statically installed provider has no `--provider-name` and serves
    /// every Provider of its class.
    #[test]
    fn a_provider_without_a_name_serves_every_machine() {
        assert!(serves(None, "kvm-b"));
        assert!(serves(None, "kvm-a"));
    }

    // --- serves_machine (ADR-0133 Decision 5) ---------------------------------

    use banlieue_api::common::ProviderReference;

    const HOME: &str = "banlieue-system";
    const TENANT: &str = "team-a";

    fn pref(name: &str, namespace: Option<&str>) -> ProviderReference {
        ProviderReference {
            name: name.to_string(),
            namespace: namespace.map(str::to_string),
        }
    }

    /// The case the whole ADR is for: a tenant's machine names the shared
    /// provider in its home namespace, and that provider takes it.
    #[test]
    fn a_shared_provider_serves_a_tenant_machine_that_names_it() {
        assert!(serves_machine(
            Some("vcenter"),
            HOME,
            TENANT,
            &pref("vcenter", Some(HOME))
        ));
    }

    /// The namespace half of the match: the same name in another namespace is
    /// another provider's machine, and a tenant must not be able to steer a
    /// machine onto a provider by name alone.
    #[test]
    fn a_machine_naming_a_same_named_provider_elsewhere_is_ignored() {
        assert!(!serves_machine(
            Some("vcenter"),
            HOME,
            TENANT,
            &pref("vcenter", Some("other-ns"))
        ));
    }

    /// A bare name means the machine's own namespace, which is not the home
    /// namespace for a tenant machine: the shared provider does not take it.
    #[test]
    fn a_bare_provider_ref_in_a_tenant_namespace_is_not_the_shared_providers() {
        assert!(!serves_machine(
            Some("vcenter"),
            HOME,
            TENANT,
            &pref("vcenter", None)
        ));
    }

    /// The pre-ADR-0133 case: machine, Provider and provider in one namespace.
    #[test]
    fn a_bare_provider_ref_in_the_home_namespace_is_served() {
        assert!(serves_machine(
            Some("vcenter"),
            HOME,
            HOME,
            &pref("vcenter", None)
        ));
    }

    #[test]
    fn another_providers_machine_is_never_served() {
        assert!(!serves_machine(
            Some("vcenter"),
            HOME,
            TENANT,
            &pref("dr-vcenter", Some(HOME))
        ));
    }

    /// A static install has no `--provider-name` and serves every Provider in
    /// its home namespace, but still only that namespace's.
    #[test]
    fn a_provider_without_a_name_serves_every_provider_in_its_home_namespace_only() {
        assert!(serves_machine(None, HOME, TENANT, &pref("any", Some(HOME))));
        assert!(serves_machine(None, HOME, HOME, &pref("any", None)));
        assert!(!serves_machine(None, HOME, TENANT, &pref("any", None)));
        assert!(!serves_machine(
            None,
            HOME,
            TENANT,
            &pref("any", Some("other-ns"))
        ));
    }

    fn provider(name: &str) -> banlieue_api::banlieue::Provider {
        let mut p: banlieue_api::banlieue::Provider = serde_json::from_value(serde_json::json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "Provider",
            "metadata": { "name": name, "namespace": "banlieue-system" },
            "spec": {
                "providerClassRef": { "name": "libvirt" },
                "connection": { "endpoint": "qemu+tls://bar.foo.io/system" }
            }
        }))
        .unwrap();
        p.metadata.name = Some(name.to_string());
        p
    }

    /// ADR-0087, Amendment 2026-11-01: a workload started for one Provider
    /// reconciles that Provider's rows and no other's: it holds that
    /// Provider's credentials only.
    #[test]
    fn a_workload_for_one_provider_has_only_that_provider_in_scope() {
        let all = vec![provider("kvm-a"), provider("kvm-b")];
        let mine = providers_in_scope(all.clone(), Some("kvm-b"));
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].metadata.name.as_deref(), Some("kvm-b"));
        assert!(providers_in_scope(all.clone(), Some("kvm-c")).is_empty());
    }

    #[test]
    fn a_static_install_has_every_provider_in_scope() {
        let all = vec![provider("kvm-a"), provider("kvm-b")];
        assert_eq!(providers_in_scope(all, None).len(), 2);
    }

    // --- provider_watch_config -----------------------------------------------

    #[test]
    fn a_named_provider_narrows_the_watch_to_that_object() {
        let config = provider_watch_config(Some("kvm-a"));
        assert_eq!(
            config.field_selector.as_deref(),
            Some("metadata.name=kvm-a")
        );
    }

    #[test]
    fn a_static_install_watches_every_provider() {
        assert_eq!(provider_watch_config(None).field_selector, None);
    }

    // --- providers_of_class ---------------------------------------------------

    #[test]
    fn only_providers_of_the_class_are_kept() {
        let mut other = provider("vc-a");
        other.spec.provider_class_ref.name = "vsphere".to_string();
        let kept = providers_of_class(vec![provider("kvm-a"), other], "libvirt");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].metadata.name.as_deref(), Some("kvm-a"));
        assert!(providers_of_class(vec![provider("kvm-a")], "proxmox").is_empty());
    }

    // --- requeue_error_policy -------------------------------------------------

    /// Whatever the kind, error and context types, a failed reconcile is
    /// retried on the error interval.
    #[test]
    fn the_shared_error_policy_requeues_on_the_error_interval() {
        let action = requeue_error_policy(
            Arc::new(provider("kvm-a")),
            &std::io::Error::other("backend unreachable"),
            Arc::new(()),
        );
        assert_eq!(format!("{action:?}"), format!("{:?}", requeue_on_error()));
    }
}
