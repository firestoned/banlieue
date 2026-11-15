// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::Context`]'s namespace gate (ADR-0131).

#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::Namespace;
    use kube::runtime::{reflector, watcher::Event};
    use kube::{Client, Config};

    use super::super::*;

    /// A client that is never used to make a request: building one does not
    /// connect, so no cluster is needed.
    fn offline_client() -> Client {
        let url = "http://127.0.0.1:1".parse().expect("valid URL");
        Client::try_from(Config::new(url)).expect("client from static config")
    }

    fn namespace(name: &str) -> Namespace {
        let mut ns = Namespace::default();
        ns.metadata.name = Some(name.to_string());
        ns
    }

    /// Single-namespace modes narrow the watch itself, so the default gate
    /// passes whatever the watch delivers.
    #[tokio::test]
    async fn the_default_context_serves_what_the_watch_delivers() {
        let ctx = Context::new(offline_client(), "banlieue-system".into());
        assert!(ctx.serves("anything"));
        assert!(ctx.skip_unserved("anything", false).is_none());
    }

    #[tokio::test]
    async fn a_gated_context_serves_only_selected_namespaces() {
        let (reader, mut writer) = reflector::store::<Namespace>();
        writer.apply_watcher_event(&Event::Apply(namespace("team-a")));
        let ctx = Context::new(offline_client(), "banlieue-system".into())
            .with_namespace_gate(NamespaceGate::selected(reader));
        assert!(ctx.serves("team-a"));
        assert!(!ctx.serves("team-b"));
        assert!(ctx.skip_unserved("team-b", false).is_some());
    }

    /// Un-labelling must not strand a Provider's workload teardown.
    #[tokio::test]
    async fn a_deleting_provider_is_never_skipped() {
        let (reader, _writer) = reflector::store::<Namespace>();
        let ctx = Context::new(offline_client(), "banlieue-system".into())
            .with_namespace_gate(NamespaceGate::selected(reader));
        assert!(ctx.skip_unserved("team-b", true).is_none());
    }
}
