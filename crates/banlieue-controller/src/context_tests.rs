// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::Context`].

#[cfg(test)]
mod tests {
    use banlieue_api::banlieue::VirtualMachine;
    use kube::runtime::reflector;
    use kube::{Client, Config};

    use super::super::*;

    /// A client that is never used to make a request: building one does not
    /// connect, so no cluster is needed.
    fn offline_client() -> Client {
        let url = "http://127.0.0.1:1".parse().expect("valid URL");
        Client::try_from(Config::new(url)).expect("client from static config")
    }

    #[tokio::test]
    async fn new_context_has_no_vm_store() {
        let ctx = Context::new(offline_client(), None);
        assert!(
            ctx.vm_store.is_none(),
            "outside the running controller the duplicate-address check must fall back to a LIST"
        );
    }

    #[tokio::test]
    async fn with_vm_store_attaches_the_store_and_keeps_the_scope() {
        let (reader, _writer) = reflector::store::<VirtualMachine>();
        let ctx = Context::new(offline_client(), Some("ns".into())).with_vm_store(reader);
        assert!(ctx.vm_store.is_some());
        assert_eq!(ctx.namespace.as_deref(), Some("ns"));
    }

    /// ADR-0088 is Accepted (2026-10-26): a context built the ordinary way
    /// honours `firstBootSealing`, matching the CLI default.
    #[tokio::test]
    async fn first_boot_sealing_is_on_by_default() {
        assert!(Context::new(offline_client(), None).first_boot_sealing);
    }

    #[tokio::test]
    async fn with_first_boot_sealing_sets_the_flag_and_keeps_the_rest() {
        let (reader, _writer) = reflector::store::<VirtualMachine>();
        let ctx = Context::new(offline_client(), Some("ns".into()))
            .with_vm_store(reader)
            .with_first_boot_sealing(false);
        assert!(!ctx.first_boot_sealing);
        assert!(ctx.vm_store.is_some());
        assert_eq!(ctx.namespace.as_deref(), Some("ns"));
        assert!(ctx.with_first_boot_sealing(true).first_boot_sealing);
    }

    fn namespace(name: &str) -> k8s_openapi::api::core::v1::Namespace {
        let mut ns = k8s_openapi::api::core::v1::Namespace::default();
        ns.metadata.name = Some(name.to_string());
        ns
    }

    /// Single-namespace modes narrow the watch itself, so the default gate
    /// passes whatever the watch delivers.
    #[tokio::test]
    async fn default_context_serves_every_namespace_it_is_handed() {
        assert!(Context::new(offline_client(), Some("ns".into())).serves("ns"));
    }

    #[tokio::test]
    async fn gated_context_serves_only_selected_namespaces() {
        let (reader, mut writer) = reflector::store::<k8s_openapi::api::core::v1::Namespace>();
        writer.apply_watcher_event(&kube::runtime::watcher::Event::Apply(namespace("team-a")));
        let ctx = Context::new(offline_client(), None)
            .with_namespace_gate(NamespaceGate::selected(reader));
        assert!(ctx.serves("team-a"));
        assert!(!ctx.serves("team-b"));
    }

    /// A reconcile for an unserved namespace must do nothing and ask to be
    /// woken only by a change: the namespace being labelled in triggers a
    /// reconcile-all, so no timer is needed.
    #[tokio::test]
    async fn unserved_namespace_yields_await_change() {
        let (reader, _writer) = reflector::store::<k8s_openapi::api::core::v1::Namespace>();
        let ctx = Context::new(offline_client(), None)
            .with_namespace_gate(NamespaceGate::selected(reader));
        assert!(ctx.skip_unserved("team-b", false).is_some());
        let served = Context::new(offline_client(), None);
        assert!(served.skip_unserved("team-b", false).is_none());
    }

    /// Un-labelling a namespace must not strand a deletion: the finalizer
    /// has to run even though the namespace is no longer served.
    #[tokio::test]
    async fn deletion_is_never_skipped_in_an_unserved_namespace() {
        let (reader, _writer) = reflector::store::<k8s_openapi::api::core::v1::Namespace>();
        let ctx = Context::new(offline_client(), None)
            .with_namespace_gate(NamespaceGate::selected(reader));
        assert!(ctx.skip_unserved("team-b", true).is_none());
    }

    // ---- provider namespace (ADR-0133 Decision 2) --------------------------

    /// Unset is the default and means no fallback: the controller never
    /// searches "every namespace" for a Provider.
    #[tokio::test]
    async fn the_provider_namespace_is_unset_by_default() {
        let ctx = Context::new(offline_client(), None);
        assert_eq!(ctx.provider_namespace(), None);
    }

    #[tokio::test]
    async fn with_provider_namespace_sets_it() {
        let ctx = Context::new(offline_client(), None)
            .with_provider_namespace(Some("banlieue-system".into()));
        assert_eq!(ctx.provider_namespace(), Some("banlieue-system"));
    }

    /// A blank value (an empty `BANLIEUE_PROVIDER_NAMESPACE=""` in a manifest)
    /// is unset, not the empty namespace.
    #[tokio::test]
    async fn a_blank_provider_namespace_is_unset() {
        let ctx = Context::new(offline_client(), None).with_provider_namespace(Some("  ".into()));
        assert_eq!(ctx.provider_namespace(), None);
    }
}
