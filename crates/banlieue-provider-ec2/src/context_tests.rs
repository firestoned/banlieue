// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::Context`]: which machines the provider serves
//! (ADR-0133) and the orphan sweep's cadence.

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use banlieue_api::common::ProviderReference;
    use banlieue_aws::FakeEc2;
    use banlieue_provider_sdk::scope::{NamespaceGate, ProviderScope};
    use k8s_openapi::api::core::v1::Namespace;
    use kube::runtime::{reflector, watcher::Event};
    use kube::{Client, Config};

    use super::super::*;
    use crate::client::StaticClientFactory;

    const HOME: &str = "banlieue-system";
    const TENANT: &str = "team-a";
    const OTHER: &str = "team-b";
    const SELECTOR: &str = "banlieue.io/tenant=true";
    const PROVIDER: &str = "ec2-cc";
    const INTERVAL: Duration = Duration::from_secs(600);

    /// A client that is never used to make a request.
    fn offline_client() -> Client {
        let url = "http://127.0.0.1:1".parse().expect("valid URL");
        Client::try_from(Config::new(url)).expect("client from static config")
    }

    fn ctx(namespace: Option<&str>) -> Context {
        Context::new(
            offline_client(),
            namespace.map(str::to_string),
            Arc::new(StaticClientFactory::new(Arc::new(FakeEc2::ca_central_1()))),
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
        let scope = ProviderScope::resolve(Some(HOME), "x", Some(SELECTOR)).expect("scope");
        let mut c = ctx(Some(HOME));
        c.provider_name = Some(PROVIDER.to_string());
        c.with_scope(scope, selected(served))
    }

    #[tokio::test]
    async fn the_home_namespace_is_where_the_provider_reads_its_provider() {
        let c = shared_ctx(&[TENANT]);
        assert_eq!(c.home_namespace(TENANT), HOME);
        assert!(c.scope().is_some());
        assert!(ctx(Some(HOME)).scope().is_none());
    }

    #[tokio::test]
    async fn the_shared_provider_serves_a_tenant_machine_that_names_it() {
        let c = shared_ctx(&[TENANT]);
        assert!(c.serves_machine(TENANT, &pref(PROVIDER, Some(HOME))));
        assert!(!c.serves_machine(TENANT, &pref(PROVIDER, None)));
        assert!(!c.serves_machine(TENANT, &pref("ec2-other", Some(HOME))));
    }

    #[tokio::test]
    async fn an_unselected_namespace_is_skipped_unless_deleting() {
        let c = shared_ctx(&[TENANT]);
        assert!(c.skip_unserved(TENANT, false).is_none());
        assert!(c.skip_unserved(OTHER, false).is_some());
        assert!(c.skip_unserved(OTHER, true).is_none());
    }

    #[test]
    fn the_first_sweep_is_due() {
        let clock = SweepClock::default();
        assert!(clock.due(PROVIDER, Instant::now(), INTERVAL));
    }

    #[test]
    fn a_sweep_is_not_due_again_within_the_interval() {
        let clock = SweepClock::default();
        let start = Instant::now();
        assert!(clock.due(PROVIDER, start, INTERVAL));
        assert!(!clock.due(PROVIDER, start + INTERVAL / 2, INTERVAL));
        assert!(clock.due(PROVIDER, start + INTERVAL, INTERVAL));
    }

    #[test]
    fn each_provider_has_its_own_cadence() {
        let clock = SweepClock::default();
        let now = Instant::now();
        assert!(clock.due(PROVIDER, now, INTERVAL));
        assert!(clock.due("ec2-other", now, INTERVAL));
    }
}
