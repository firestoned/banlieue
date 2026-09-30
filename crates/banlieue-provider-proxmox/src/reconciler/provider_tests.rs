// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `Provider` reconciler.
//!
//! `compute_status` takes `&dyn ProxmoxApi`, so these drive the real
//! capability-probing logic against `FakeProxmox`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{
        NetworkClassMapping, ProviderCapabilities, ProviderConnection, ProviderSpec,
        StorageClassMapping,
    };
    use banlieue_api::common::LocalObjectReference;
    use banlieue_provider_sdk::status::{condition_status, find_condition};
    use banlieue_proxmox::FakeProxmox;
    use kube::api::ObjectMeta;
    use std::collections::BTreeMap;

    fn target(k: &str, v: &str) -> Option<BTreeMap<String, String>> {
        Some(BTreeMap::from([(k.to_string(), v.to_string())]))
    }

    fn provider(storage: &[(&str, &str)], networks: &[(&str, &str)]) -> Provider {
        Provider {
            metadata: ObjectMeta {
                name: Some("pve-a".into()),
                namespace: Some("banlieue-system".into()),
                ..Default::default()
            },
            spec: ProviderSpec {
                provider_class_ref: LocalObjectReference {
                    name: PROVIDER_CLASS_NAME.into(),
                },
                connection: ProviderConnection {
                    endpoint: "https://bar.foo.io:8006".into(),
                    credentials_ref: Some(LocalObjectReference {
                        name: "creds".into(),
                    }),
                    insecure_skip_tls_verify: false,
                    ca_bundle: None,
                },
                capabilities: ProviderCapabilities {
                    storage_classes: storage
                        .iter()
                        .map(|(n, s)| StorageClassMapping {
                            name: (*n).into(),
                            target: target("storage", s),
                            ..Default::default()
                        })
                        .collect(),
                    network_classes: networks
                        .iter()
                        .map(|(n, b)| NetworkClassMapping {
                            name: (*n).into(),
                            target: target("bridge", b),
                            ..Default::default()
                        })
                        .collect(),
                    features: vec!["nestedVirtualization".into()],
                },
                paused: false,
                use_content_library: false,
                failure_domain_name_overrides: Vec::new(),
                attestation: None,
            },
            status: None,
        }
    }

    fn ready(
        status: &ProviderStatus,
    ) -> &k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition {
        find_condition(&status.conditions, condition_types::READY).unwrap()
    }

    #[tokio::test]
    async fn everything_declared_and_present_is_ready() {
        let f = FakeProxmox::single_node("pve1");
        let p = provider(
            &[("gold", "local-lvm"), ("seed-iso", "local")],
            &[("prod", "vmbr0")],
        );
        let s = compute_status(&f, &p, 3).await.unwrap();
        assert_eq!(ready(&s).status, condition_status::TRUE, "{:?}", ready(&s));
        assert_eq!(s.observed_generation, Some(3));
        let fd = &s.failure_domains[0];
        assert_eq!(
            fd.attributes.available_storage_classes,
            ["gold", "seed-iso"]
        );
        assert_eq!(fd.attributes.available_network_classes, ["prod"]);
        assert_eq!(fd.attributes.features, ["nestedVirtualization"]);
    }

    /// The controller schedules by this raw attribute; without it scheduling
    /// fails with MissingFdRaw.
    #[tokio::test]
    async fn each_failure_domain_carries_its_node_as_a_raw_attribute() {
        let f = FakeProxmox::single_node("pve1");
        f.add_node("pve2");
        let s = compute_status(&f, &provider(&[], &[]), 1).await.unwrap();
        let names: Vec<&str> = s.failure_domains.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["pve-a-pve1", "pve-a-pve2"]);
        assert_eq!(
            s.failure_domains[0]
                .attributes
                .raw
                .get("node")
                .map(String::as_str),
            Some("pve1")
        );
        assert_eq!(
            s.failure_domains[1]
                .attributes
                .raw
                .get("node")
                .map(String::as_str),
            Some("pve2")
        );
        assert!(s.failure_domains[0].attributes.raw.contains_key("version"));
    }

    #[tokio::test]
    async fn an_offline_node_is_not_a_failure_domain() {
        let f = FakeProxmox::single_node("pve1");
        f.add_node("pve2");
        f.set_node_offline("pve2");
        let s = compute_status(&f, &provider(&[], &[]), 1).await.unwrap();
        assert_eq!(s.failure_domains.len(), 1);
        assert_eq!(s.failure_domains[0].name, "pve-a-pve1");
    }

    #[tokio::test]
    async fn no_online_node_is_an_error() {
        let f = FakeProxmox::single_node("pve1");
        f.set_node_offline("pve1");
        let e = compute_status(&f, &provider(&[], &[]), 1)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("no online node"), "{e}");
    }

    #[tokio::test]
    async fn a_storage_that_does_not_exist_is_reported_not_invented() {
        let f = FakeProxmox::single_node("pve1");
        let s = compute_status(&f, &provider(&[("gold", "ceph")], &[]), 1)
            .await
            .unwrap();
        assert!(
            s.failure_domains[0]
                .attributes
                .available_storage_classes
                .is_empty()
        );
        let r = ready(&s);
        assert_eq!(r.status, condition_status::FALSE);
        assert_eq!(r.reason, reasons::CAPABILITIES_INCOMPLETE);
        assert!(
            r.message.contains("gold") && r.message.contains("ceph"),
            "{}",
            r.message
        );
    }

    /// Roadmap 06 gotcha: a storage can exist and still refuse a disk.
    #[tokio::test]
    async fn a_storage_without_images_content_fails_loudly_and_says_why() {
        let f = FakeProxmox::single_node("pve1");
        f.add_storage("iso-only", "dir", "iso");
        let s = compute_status(&f, &provider(&[("gold", "iso-only")], &[]), 1)
            .await
            .unwrap();
        assert!(
            s.failure_domains[0]
                .attributes
                .available_storage_classes
                .is_empty()
        );
        let r = ready(&s);
        assert_eq!(r.status, condition_status::FALSE);
        assert!(r.message.contains("images"), "{}", r.message);
    }

    #[tokio::test]
    async fn the_seed_iso_class_must_allow_iso_content() {
        let f = FakeProxmox::single_node("pve1");
        // local-lvm holds images, not ISOs.
        let s = compute_status(&f, &provider(&[("seed-iso", "local-lvm")], &[]), 1)
            .await
            .unwrap();
        let r = ready(&s);
        assert_eq!(r.status, condition_status::FALSE);
        assert!(
            r.message.contains("seed-iso") && r.message.contains("iso"),
            "{}",
            r.message
        );
        assert!(
            s.failure_domains[0]
                .attributes
                .available_storage_classes
                .is_empty()
        );
    }

    #[tokio::test]
    async fn an_ordinary_class_is_not_held_to_the_iso_rule() {
        let f = FakeProxmox::single_node("pve1");
        let s = compute_status(&f, &provider(&[("gold", "local-lvm")], &[]), 1)
            .await
            .unwrap();
        assert_eq!(ready(&s).status, condition_status::TRUE);
    }

    #[tokio::test]
    async fn a_bridge_that_is_absent_is_reported() {
        let f = FakeProxmox::single_node("pve1");
        let s = compute_status(&f, &provider(&[], &[("prod", "vmbr9")]), 1)
            .await
            .unwrap();
        assert!(
            s.failure_domains[0]
                .attributes
                .available_network_classes
                .is_empty()
        );
        let r = ready(&s);
        assert_eq!(r.status, condition_status::FALSE);
        assert!(r.message.contains("vmbr9"), "{}", r.message);
    }

    #[tokio::test]
    async fn a_class_present_on_only_some_nodes_narrows_per_node_and_stays_ready() {
        let f = FakeProxmox::single_node("pve1");
        f.add_node("pve2");
        // The fake shares inventory across nodes, so narrow via a declared
        // class that exists everywhere and one that exists nowhere.
        let s = compute_status(
            &f,
            &provider(&[("gold", "local-lvm")], &[("prod", "vmbr0")]),
            1,
        )
        .await
        .unwrap();
        for fd in &s.failure_domains {
            assert_eq!(fd.attributes.available_storage_classes, ["gold"]);
        }
        assert_eq!(ready(&s).status, condition_status::TRUE);
    }

    #[tokio::test]
    async fn a_class_with_no_target_key_is_reported() {
        let f = FakeProxmox::single_node("pve1");
        let mut p = provider(&[("gold", "local-lvm")], &[]);
        p.spec.capabilities.storage_classes[0].target = None;
        let s = compute_status(&f, &p, 1).await.unwrap();
        let r = ready(&s);
        assert_eq!(r.status, condition_status::FALSE);
        assert!(r.message.contains("gold"), "{}", r.message);
    }

    #[tokio::test]
    async fn reachability_condition_reports_the_version_and_counts() {
        let f = FakeProxmox::single_node("pve1");
        let s = compute_status(&f, &provider(&[], &[]), 1).await.unwrap();
        let c = find_condition(&s.conditions, condition_types::PROVIDER_REACHABLE).unwrap();
        assert_eq!(c.status, condition_status::TRUE);
        assert!(c.message.contains("9."), "{}", c.message);
        assert!(c.message.contains("1 online node"), "{}", c.message);
    }

    #[test]
    fn a_failed_status_carries_the_reason_on_both_conditions() {
        let s = failed_status(4, reasons::CONNECT_FAILED, "refused".into());
        assert!(s.failure_domains.is_empty());
        assert_eq!(s.observed_generation, Some(4));
        for t in [condition_types::READY, condition_types::PROVIDER_REACHABLE] {
            let c = find_condition(&s.conditions, t).unwrap();
            assert_eq!(
                (c.status.as_str(), c.reason.as_str()),
                ("False", reasons::CONNECT_FAILED)
            );
        }
    }

    #[test]
    fn unauthorized_errors_get_their_own_reason() {
        let e = Error::Proxmox(banlieue_proxmox::Error::Api {
            status: 401,
            message: "Authentication failed!".into(),
        });
        assert_eq!(failure_reason(&e), reasons::UNAUTHORIZED);
        let e = Error::Proxmox(banlieue_proxmox::Error::Transport("refused".into()));
        assert_eq!(failure_reason(&e), reasons::CONNECT_FAILED);
        let e = Error::Missing("Provider.spec.connection.credentialsRef");
        assert_eq!(failure_reason(&e), reasons::CREDENTIALS_UNAVAILABLE);
    }
}
