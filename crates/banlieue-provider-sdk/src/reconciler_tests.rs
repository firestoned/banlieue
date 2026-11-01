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
        assert!(serves(Some("slate-kvm"), "slate-kvm"));
        assert!(!serves(Some("grill-kvm"), "slate-kvm"));
    }

    /// A statically installed provider has no `--provider-name` and serves
    /// every Provider of its class.
    #[test]
    fn a_provider_without_a_name_serves_every_machine() {
        assert!(serves(None, "slate-kvm"));
        assert!(serves(None, "grill-kvm"));
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
}
