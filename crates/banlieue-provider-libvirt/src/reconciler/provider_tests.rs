// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `Provider` reconciler.
//!
//! `compute_status` takes `&dyn LibvirtClient`, so these drive the real
//! verification logic with a `FakeClient` — no kube, no TLS, no libvirtd.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{
        NetworkClassMapping, ProviderCapabilities, ProviderConnection, ProviderSpec,
        StorageClassMapping,
    };
    use banlieue_api::common::LocalObjectReference;
    use banlieue_provider_sdk::status::{condition_status, find_condition};
    use kube::api::ObjectMeta;
    use std::collections::BTreeMap;

    use crate::client::FakeClient;

    fn target(k: &str, v: &str) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert(k.to_string(), v.to_string());
        m
    }

    /// A Provider declaring `gold`->pool `default` and `prod`->network `default`.
    fn provider_with(
        storage: Vec<(&str, &str)>,
        networks: Vec<(&str, &str)>,
    ) -> banlieue_api::banlieue::Provider {
        banlieue_api::banlieue::Provider {
            metadata: ObjectMeta {
                name: Some("libvirt-1".to_string()),
                namespace: Some("banlieue-system".to_string()),
                ..Default::default()
            },
            spec: ProviderSpec {
                provider_class_ref: LocalObjectReference {
                    name: PROVIDER_CLASS_NAME.to_string(),
                },
                connection: ProviderConnection {
                    endpoint: "qemu+tls://libvirt-host.example/system".to_string(),
                    credentials_ref: LocalObjectReference {
                        name: "creds".to_string(),
                    },
                    insecure_skip_tls_verify: false,
                    ca_bundle: None,
                },
                capabilities: ProviderCapabilities {
                    storage_classes: storage
                        .into_iter()
                        .map(|(name, pool)| StorageClassMapping {
                            name: name.to_string(),
                            target: target("pool", pool),
                        })
                        .collect(),
                    network_classes: networks
                        .into_iter()
                        .map(|(name, net)| NetworkClassMapping {
                            name: name.to_string(),
                            target: target("network", net),
                        })
                        .collect(),
                    features: vec!["nestedVirtualization".to_string()],
                },
                paused: false,
            },
            status: None,
        }
    }

    #[tokio::test]
    async fn all_declared_capabilities_present_yields_ready() {
        let client = FakeClient::with(&["default", "boot"], &["default"]);
        let p = provider_with(vec![("gold", "default")], vec![("prod", "default")]);
        let status = compute_status(&client, &p, 1).await.unwrap();

        assert_eq!(
            status.failure_domains.len(),
            1,
            "one host, one failure domain"
        );
        let fd = &status.failure_domains[0];
        assert_eq!(fd.name, "libvirt-1");
        assert_eq!(fd.attributes.available_storage_classes, vec!["gold"]);
        assert_eq!(fd.attributes.available_network_classes, vec!["prod"]);

        let ready = find_condition(&status.conditions, condition_types::READY).unwrap();
        assert_eq!(ready.status, condition_status::TRUE);
        assert_eq!(ready.reason, reasons::RECONCILED);
    }

    #[tokio::test]
    async fn a_declared_pool_missing_on_the_host_is_not_advertised() {
        // The whole reason for probing: advertising a capability the host does
        // not have would let the scheduler place a VM that can never start.
        let client = FakeClient::with(&["default"], &["default"]);
        let p = provider_with(
            vec![("gold", "default"), ("nvme", "does-not-exist")],
            vec![("prod", "default")],
        );
        let status = compute_status(&client, &p, 3).await.unwrap();

        let fd = &status.failure_domains[0];
        assert_eq!(
            fd.attributes.available_storage_classes,
            vec!["gold"],
            "the missing class must be dropped, not reported"
        );

        let ready = find_condition(&status.conditions, condition_types::READY).unwrap();
        assert_eq!(ready.status, condition_status::FALSE);
        assert_eq!(ready.reason, reasons::CAPABILITIES_INCOMPLETE);
        assert!(ready.message.contains("nvme"), "message names the culprit");

        // Reachability is a separate axis: we DID reach the host.
        let reachable =
            find_condition(&status.conditions, condition_types::PROVIDER_REACHABLE).unwrap();
        assert_eq!(reachable.status, condition_status::TRUE);
    }

    #[tokio::test]
    async fn a_declared_network_missing_on_the_host_is_reported() {
        let client = FakeClient::with(&["default"], &["default"]);
        let p = provider_with(vec![("gold", "default")], vec![("dmz", "br-dmz")]);
        let status = compute_status(&client, &p, 1).await.unwrap();
        assert!(
            status.failure_domains[0]
                .attributes
                .available_network_classes
                .is_empty()
        );
        let ready = find_condition(&status.conditions, condition_types::READY).unwrap();
        assert_eq!(ready.reason, reasons::CAPABILITIES_INCOMPLETE);
        assert!(ready.message.contains("dmz"));
    }

    #[tokio::test]
    async fn a_class_whose_target_lacks_a_pool_key_counts_as_missing() {
        // libvirt storage classes map via `pool`; a mapping using vSphere's
        // `datastore` key names nothing here and must not be advertised.
        let client = FakeClient::with(&["default"], &["default"]);
        let mut p = provider_with(vec![], vec![]);
        p.spec.capabilities.storage_classes = vec![StorageClassMapping {
            name: "gold".to_string(),
            target: target("datastore", "ds-fast-01"),
        }];
        let status = compute_status(&client, &p, 1).await.unwrap();
        assert!(
            status.failure_domains[0]
                .attributes
                .available_storage_classes
                .is_empty()
        );
    }

    #[tokio::test]
    async fn failure_domain_carries_the_host_inventory_and_labels() {
        let client = FakeClient::with(&["default", "boot"], &["default"]);
        let mut p = provider_with(vec![("gold", "default")], vec![("prod", "default")]);
        let mut labels = BTreeMap::new();
        labels.insert("dc".to_string(), "dc1".to_string());
        p.metadata.labels = Some(labels);

        let status = compute_status(&client, &p, 1).await.unwrap();
        let fd = &status.failure_domains[0];
        assert_eq!(fd.labels.get("dc").map(String::as_str), Some("dc1"));
        assert_eq!(fd.attributes.raw.get("pools").unwrap(), "default,boot");
        assert_eq!(fd.attributes.raw.get("networks").unwrap(), "default");
        assert_eq!(fd.attributes.features, vec!["nestedVirtualization"]);
    }

    #[tokio::test]
    async fn a_transport_failure_propagates() {
        let client = FakeClient::failing("connection refused");
        let p = provider_with(vec![("gold", "default")], vec![]);
        assert!(compute_status(&client, &p, 1).await.is_err());
    }

    #[test]
    fn failed_status_publishes_no_failure_domains() {
        // Advertising domains we could not verify is worse than advertising
        // none: the scheduler reads this list to place VMs.
        let status = failed_status(7, reasons::CONNECT_FAILED, "no route to host".into());
        assert!(status.failure_domains.is_empty());
        assert_eq!(status.observed_generation, Some(7));
        for t in [condition_types::READY, condition_types::PROVIDER_REACHABLE] {
            let c = find_condition(&status.conditions, t).unwrap();
            assert_eq!(c.status, condition_status::FALSE);
            assert_eq!(c.reason, reasons::CONNECT_FAILED);
        }
    }
}
