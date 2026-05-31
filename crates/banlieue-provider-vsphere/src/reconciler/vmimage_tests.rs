// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::vmimage`].
//!
//! These tests target the pure helpers and `compute_template_status` (which
//! takes `&dyn VSphereClient`, so `FakeClient` drives it without contacting
//! kube or vCenter).

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use banlieue_api::banlieue::{
        Architecture, FailureDomain, FailureDomainAttributes, GuestAgent, ImageSource,
        ImageSourceKind, OsFamily, Provider, ProviderConnection, ProviderSpec, ProviderStatus,
        VMImage, VMImageSpec,
    };
    use banlieue_api::common::LocalObjectReference;
    use banlieue_provider_sdk::status::condition_status;
    use kube::api::ObjectMeta;

    use crate::client::{Datacenter, FakeClient, Inventory, VSphereClient};

    use super::super::{
        AggregateReady, aggregate_ready, compute_template_status, find_vsphere_source, reasons,
    };

    fn dc(name: &str) -> Datacenter {
        Datacenter {
            name: name.to_string(),
            moref: format!("datacenter-{name}"),
        }
    }

    fn provider(name: &str, namespace: &str) -> Provider {
        let mut raw = BTreeMap::new();
        raw.insert("datacenter".to_string(), "dc-east".to_string());
        raw.insert("cluster".to_string(), "cluster-a".to_string());

        Provider {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                ..Default::default()
            },
            spec: ProviderSpec {
                provider_class_ref: LocalObjectReference {
                    name: "vsphere".to_string(),
                },
                connection: ProviderConnection {
                    endpoint: "https://vc".to_string(),
                    credentials_ref: LocalObjectReference {
                        name: "creds".to_string(),
                    },
                    insecure_skip_tls_verify: true,
                    ca_bundle: None,
                },
                capabilities: Default::default(),
                paused: false,
            },
            status: Some(ProviderStatus {
                failure_domains: vec![FailureDomain {
                    name: format!("{name}-dc-east-cluster-a"),
                    labels: Default::default(),
                    attributes: FailureDomainAttributes {
                        raw,
                        ..Default::default()
                    },
                }],
                conditions: vec![],
                observed_generation: Some(1),
            }),
        }
    }

    fn vsphere_image_source(template_name: &str) -> ImageSource {
        ImageSource {
            provider_class: "vsphere".to_string(),
            kind: ImageSourceKind::Template,
            reference: template_name.to_string(),
            import_from: None,
            checksum: None,
        }
    }

    fn fake_client_with(template: Option<(&str, &str)>) -> FakeClient {
        let mut builder = Inventory::builder().with_dc("dc-east");
        if let Some((dc_name, tname)) = template {
            builder = builder.with_template(dc_name, tname);
        }
        FakeClient::new(builder.build())
    }

    fn as_client(c: &FakeClient) -> &dyn VSphereClient {
        c
    }

    // ---------- find_vsphere_source --------------------------------------

    #[test]
    fn find_vsphere_source_picks_first_vsphere_template() {
        let sources = vec![
            ImageSource {
                provider_class: "proxmox".to_string(),
                kind: ImageSourceKind::Template,
                reference: "9000".to_string(),
                import_from: None,
                checksum: None,
            },
            vsphere_image_source("ubuntu-22.04"),
        ];
        let picked = find_vsphere_source(&sources).unwrap();
        assert_eq!(picked.reference, "ubuntu-22.04");
    }

    #[test]
    fn find_vsphere_source_returns_none_when_no_vsphere_template() {
        let sources = vec![ImageSource {
            provider_class: "vsphere".to_string(),
            kind: ImageSourceKind::Url, // not Template
            reference: String::new(),
            import_from: Some("https://example.com/ubuntu.ova".to_string()),
            checksum: None,
        }];
        assert!(find_vsphere_source(&sources).is_none());
    }

    #[test]
    fn find_vsphere_source_returns_none_for_other_provider_classes() {
        let sources = vec![ImageSource {
            provider_class: "libvirt".to_string(),
            kind: ImageSourceKind::Template,
            reference: "/var/lib/libvirt/ubuntu.qcow2".to_string(),
            import_from: None,
            checksum: None,
        }];
        assert!(find_vsphere_source(&sources).is_none());
    }

    // ---------- compute_template_status ----------------------------------

    #[tokio::test]
    async fn compute_template_status_returns_ready_when_template_present() {
        let client = fake_client_with(Some(("dc-east", "ubuntu-22.04")));
        let dcs = vec![dc("dc-east")];
        let row = compute_template_status(
            as_client(&client),
            &dcs,
            "ubuntu-22.04",
            &provider("prov-east", "banlieue"),
        )
        .await;
        assert!(row.ready);
        assert_eq!(row.reason.as_deref(), Some(reasons::RECONCILED));
        assert_eq!(row.provider_name, "prov-east");
        assert_eq!(row.provider_namespace, "banlieue");
        assert_eq!(
            row.resolved_ref.as_deref(),
            Some("[dc-east] ubuntu-22.04"),
            "resolved_ref should follow vSphere [datacenter] template-name convention"
        );
    }

    #[tokio::test]
    async fn compute_template_status_returns_not_found_when_template_absent() {
        let client = fake_client_with(None); // DC seeded but no template
        let dcs = vec![dc("dc-east")];
        let row = compute_template_status(
            as_client(&client),
            &dcs,
            "ubuntu-22.04",
            &provider("p", "ns"),
        )
        .await;
        assert!(!row.ready);
        assert_eq!(row.reason.as_deref(), Some(reasons::TEMPLATE_NOT_FOUND));
        assert!(row.message.as_deref().unwrap().contains("ubuntu-22.04"));
    }

    #[tokio::test]
    async fn compute_template_status_returns_not_found_with_no_datacenters() {
        // Defensive: if for some reason no DCs are passed in (e.g. Provider
        // status went stale and live walk is empty too), don't claim ready.
        let client = fake_client_with(Some(("dc-east", "ubuntu-22.04")));
        let row = compute_template_status(
            as_client(&client),
            &[],
            "ubuntu-22.04",
            &provider("p", "ns"),
        )
        .await;
        assert!(!row.ready);
        assert_eq!(row.reason.as_deref(), Some(reasons::TEMPLATE_NOT_FOUND));
        assert!(
            row.message.as_deref().unwrap().contains("no datacenters"),
            "message should explain why: {:?}",
            row.message
        );
    }

    // ---------- aggregate_ready ------------------------------------------

    #[test]
    fn aggregate_ready_true_when_all_rows_ready() {
        let rows = vec![ready_row("a"), ready_row("b")];
        let agg = aggregate_ready(&rows);
        assert_eq!(agg.status, condition_status::TRUE);
        assert_eq!(agg.reason, reasons::RECONCILED);
    }

    #[test]
    fn aggregate_ready_false_when_any_row_unready() {
        let rows = vec![
            ready_row("a"),
            unready_row("b", reasons::TEMPLATE_NOT_FOUND, "missing"),
        ];
        let agg = aggregate_ready(&rows);
        assert_eq!(agg.status, condition_status::FALSE);
        assert_eq!(
            agg.reason,
            reasons::TEMPLATE_NOT_FOUND,
            "aggregate reason inherits the first failing row's reason"
        );
    }

    #[test]
    fn aggregate_ready_unknown_when_no_rows() {
        let agg = aggregate_ready(&[]);
        assert_eq!(agg.status, condition_status::UNKNOWN);
        assert_eq!(agg.reason, reasons::NO_VSPHERE_SOURCE);
    }

    #[test]
    fn aggregate_ready_buckets_unknown_reason_strings() {
        // If a row has a reason string we don't know about, the aggregate must
        // still pick a stable enum value rather than leaking arbitrary text.
        let rows = vec![unready_row("a", "SomeFutureReason", "future tense")];
        let agg = aggregate_ready(&rows);
        assert_eq!(agg.status, condition_status::FALSE);
        assert!(
            matches!(
                agg.reason,
                reasons::LOOKUP_FAILED
                    | reasons::TEMPLATE_NOT_FOUND
                    | reasons::SECRET_UNAVAILABLE
                    | reasons::CONNECT_FAILED
            ),
            "unknown reason should be bucketed; got {:?}",
            agg.reason
        );
    }

    fn ready_row(name: &str) -> banlieue_api::banlieue::ImagePerProviderStatus {
        banlieue_api::banlieue::ImagePerProviderStatus {
            provider_name: name.to_string(),
            provider_namespace: "ns".to_string(),
            ready: true,
            resolved_ref: Some("[dc] t".to_string()),
            reason: Some(reasons::RECONCILED.to_string()),
            message: None,
        }
    }

    fn unready_row(
        name: &str,
        reason: &str,
        message: &str,
    ) -> banlieue_api::banlieue::ImagePerProviderStatus {
        banlieue_api::banlieue::ImagePerProviderStatus {
            provider_name: name.to_string(),
            provider_namespace: "ns".to_string(),
            ready: false,
            resolved_ref: None,
            reason: Some(reason.to_string()),
            message: Some(message.to_string()),
        }
    }

    // ---------- Hooks into the rest of the type system -------------------

    #[test]
    fn aggregate_ready_struct_is_clone_eq() {
        // Smoke that the surface struct stays Clone/Eq — useful for tests
        // that snapshot the aggregate value across reconcile passes.
        let a = AggregateReady {
            status: condition_status::TRUE,
            reason: reasons::RECONCILED,
            message: "ok".into(),
        };
        assert_eq!(a, a.clone());
    }

    // Smoke: VMImage construction (rules out future field-rename drift breaking
    // these tests silently).
    #[test]
    fn vmimage_minimal_construct() {
        let _ = VMImage {
            metadata: ObjectMeta {
                name: Some("ubuntu-22-04".to_string()),
                ..Default::default()
            },
            spec: VMImageSpec {
                os_family: OsFamily::Linux,
                os_distribution: "ubuntu".to_string(),
                os_version: "22.04".to_string(),
                architecture: Architecture::Amd64,
                guest_agent: GuestAgent::CloudInit,
                sources: vec![vsphere_image_source("ubuntu-22.04")],
            },
            status: None,
        };
    }
}
