// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::Context`]: which namespace a provider reads its own
//! `Provider` and credentials from, and which machines it serves (ADR-0133).

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use banlieue_api::common::ProviderReference;
    use banlieue_provider_sdk::scope::{NamespaceGate, ProviderScope};
    use banlieue_proxmox::FakeProxmox;
    use k8s_openapi::api::core::v1::Namespace;
    use kube::runtime::{reflector, watcher::Event};
    use kube::{Client, Config};

    use super::super::*;
    use crate::client::StaticClientFactory;

    const HOME: &str = "banlieue-system";
    const TENANT: &str = "team-a";
    const OTHER: &str = "team-b";
    const SELECTOR: &str = "banlieue.io/tenant=true";
    const PROVIDER: &str = "pve-a";

    /// A client that is never used to make a request: building one does not
    /// connect, so no cluster is needed.
    fn offline_client() -> Client {
        let url = "http://127.0.0.1:1".parse().expect("valid URL");
        Client::try_from(Config::new(url)).expect("client from static config")
    }

    fn ctx(namespace: Option<&str>) -> Context {
        Context::new(
            offline_client(),
            namespace.map(str::to_string),
            Arc::new(StaticClientFactory::new(Arc::new(
                FakeProxmox::single_node("pve1"),
            ))),
            None,
        )
    }

    fn pref(name: &str, namespace: Option<&str>) -> ProviderReference {
        ProviderReference {
            name: name.to_string(),
            namespace: namespace.map(str::to_string),
        }
    }

    fn selected(names: &[&str]) -> NamespaceGate {
        let (reader, mut writer) = reflector::store::<Namespace>();
        for n in names {
            let mut ns = Namespace::default();
            ns.metadata.name = Some((*n).to_string());
            writer.apply_watcher_event(&Event::Apply(ns));
        }
        NamespaceGate::selected(reader)
    }

    fn shared_ctx(served: &[&str]) -> Context {
        let scope = ProviderScope::resolve(Some(HOME), "x", Some(SELECTOR)).unwrap();
        let mut c = ctx(Some(HOME));
        c.provider_name = Some(PROVIDER.to_string());
        c.with_scope(scope, selected(served))
    }

    /// The provider reads its `Provider`, credentials and CA bundle from its
    /// home namespace, whatever namespace the machine is in.
    #[tokio::test]
    async fn the_home_namespace_is_where_the_provider_reads_credentials() {
        let c = shared_ctx(&[TENANT]);
        assert_eq!(c.home_namespace(TENANT), HOME);
        assert_eq!(c.home_namespace(HOME), HOME);
    }

    #[tokio::test]
    async fn without_a_scope_the_home_is_the_serving_namespace() {
        assert_eq!(ctx(Some(HOME)).home_namespace(TENANT), HOME);
        assert_eq!(ctx(None).home_namespace(TENANT), TENANT);
    }

    #[tokio::test]
    async fn the_shared_provider_serves_a_tenant_machine_that_names_it() {
        let c = shared_ctx(&[TENANT]);
        assert!(c.serves_machine(TENANT, &pref(PROVIDER, Some(HOME))));
    }

    #[tokio::test]
    async fn a_tenant_machine_that_names_only_the_providers_name_is_not_served() {
        let c = shared_ctx(&[TENANT]);
        assert!(!c.serves_machine(TENANT, &pref(PROVIDER, None)));
    }

    #[tokio::test]
    async fn another_providers_machine_is_not_served() {
        let c = shared_ctx(&[TENANT]);
        assert!(!c.serves_machine(TENANT, &pref("dr-pve", Some(HOME))));
    }

    #[tokio::test]
    async fn a_machine_in_the_home_namespace_with_a_bare_ref_is_served() {
        let c = shared_ctx(&[]);
        assert!(c.serves_machine(HOME, &pref(PROVIDER, None)));
    }

    #[tokio::test]
    async fn a_selected_namespace_is_served_and_others_are_skipped() {
        let c = shared_ctx(&[TENANT]);
        assert!(c.skip_unserved(TENANT, false).is_none());
        assert!(c.skip_unserved(OTHER, false).is_some());
    }

    #[tokio::test]
    async fn the_home_namespace_is_always_served() {
        let c = shared_ctx(&[]);
        assert!(c.skip_unserved(HOME, false).is_none());
    }

    /// A finalizer must still run when a namespace is un-labelled.
    #[tokio::test]
    async fn a_deleting_machine_is_served_even_in_an_unselected_namespace() {
        let c = shared_ctx(&[]);
        assert!(c.skip_unserved(OTHER, true).is_none());
    }

    #[tokio::test]
    async fn without_a_selector_only_the_home_namespace_is_served() {
        let scope = ProviderScope::resolve(Some(HOME), "x", None).unwrap();
        let c = ctx(Some(HOME)).with_scope(scope, NamespaceGate::none());
        assert!(c.skip_unserved(HOME, false).is_none());
        assert!(c.skip_unserved(TENANT, false).is_some());
    }

    #[tokio::test]
    async fn no_scope_installed_skips_nothing() {
        assert!(ctx(Some(HOME)).skip_unserved(TENANT, false).is_none());
    }
}
