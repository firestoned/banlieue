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
        RawDiskArtifactPhase, RawDiskArtifactStatus, VMImage, VMImageSpec,
    };
    use banlieue_api::common::LocalObjectReference;
    use kube::api::ObjectMeta;

    use crate::client::{Datacenter, FakeClient, Inventory, VSphereClient};

    use super::super::{
        compute_template_status, compute_url_source_status, find_vsphere_source, reasons,
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
                workload: None,
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
    fn find_vsphere_source_also_picks_url_sources() {
        // Url sources (banlieue-imagebuilder pipeline, ADR-0010) are now a
        // supported vsphere source kind, not skipped.
        let sources = vec![ImageSource {
            provider_class: "vsphere".to_string(),
            kind: ImageSourceKind::Url,
            reference: String::new(),
            import_from: Some("quay.io/kairos/ubuntu:24.04".to_string()),
            checksum: None,
        }];
        let picked = find_vsphere_source(&sources).unwrap();
        assert_eq!(picked.kind, ImageSourceKind::Url);
    }

    #[test]
    fn find_vsphere_source_returns_none_for_backing_file_only() {
        // BackingFile is a libvirt-shaped concept; vsphere never declares one.
        let sources = vec![ImageSource {
            provider_class: "vsphere".to_string(),
            kind: ImageSourceKind::BackingFile,
            reference: "/var/lib/libvirt/ubuntu.qcow2".to_string(),
            import_from: None,
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

    // ---------- compute_url_source_status ---------------------------------

    #[test]
    fn url_source_no_raw_disk_artifact_yet_is_build_pending() {
        let row = compute_url_source_status(&provider("p", "ns"), None);
        assert!(!row.ready);
        assert_eq!(row.reason.as_deref(), Some(reasons::BUILD_PENDING));
        assert!(row.zones.is_empty());
    }

    #[test]
    fn url_source_pending_or_building_artifact_is_build_pending() {
        for phase in [
            RawDiskArtifactPhase::Pending,
            RawDiskArtifactPhase::Building,
        ] {
            let artifact = RawDiskArtifactStatus {
                phase,
                os_artifact_ref: "img-build".to_string(),
                pvc_ref: None,
                disk_file: None,
                reason: None,
                message: None,
                checksum: None,
            };
            let row = compute_url_source_status(&provider("p", "ns"), Some(&artifact));
            assert!(!row.ready);
            assert_eq!(row.reason.as_deref(), Some(reasons::BUILD_PENDING));
        }
    }

    #[test]
    fn url_source_failed_artifact_surfaces_build_failed_with_message() {
        let artifact = RawDiskArtifactStatus {
            phase: RawDiskArtifactPhase::Failed,
            os_artifact_ref: "img-build".to_string(),
            pvc_ref: None,
            disk_file: None,
            reason: None,
            message: Some("pull failed: manifest unknown".to_string()),
            checksum: None,
        };
        let row = compute_url_source_status(&provider("p", "ns"), Some(&artifact));
        assert!(!row.ready);
        assert_eq!(row.reason.as_deref(), Some(reasons::BUILD_FAILED));
        assert_eq!(
            row.message.as_deref(),
            Some("pull failed: manifest unknown")
        );
    }

    #[test]
    fn url_source_ready_artifact_with_no_failure_domains() {
        let mut p = provider("p", "ns");
        p.status.as_mut().unwrap().failure_domains = vec![];
        let artifact = RawDiskArtifactStatus {
            phase: RawDiskArtifactPhase::Ready,
            os_artifact_ref: "img-build".to_string(),
            pvc_ref: Some(LocalObjectReference {
                name: "img-build-artifacts".to_string(),
            }),
            disk_file: Some("img-build.raw".to_string()),
            reason: None,
            message: None,
            checksum: None,
        };
        let row = compute_url_source_status(&p, Some(&artifact));
        assert!(!row.ready);
        assert_eq!(row.reason.as_deref(), Some(reasons::NO_FAILURE_DOMAINS));
        assert!(row.zones.is_empty());
    }

    #[test]
    fn url_source_ready_artifact_reports_one_pending_zone_per_failure_domain() {
        // provider("p", "ns") seeds exactly one failure domain.
        let artifact = RawDiskArtifactStatus {
            phase: RawDiskArtifactPhase::Ready,
            os_artifact_ref: "img-build".to_string(),
            pvc_ref: Some(LocalObjectReference {
                name: "img-build-artifacts".to_string(),
            }),
            disk_file: Some("img-build.raw".to_string()),
            reason: None,
            message: None,
            checksum: None,
        };
        let p = provider("p", "ns");
        let expected_zone = p.status.as_ref().unwrap().failure_domains[0].name.clone();
        let row = compute_url_source_status(&p, Some(&artifact));
        // Raw disk is built, but per-zone import isn't implemented yet — the
        // provider-level row must stay not-ready until every zone is.
        assert!(!row.ready);
        assert_eq!(row.zones.len(), 1);
        assert_eq!(row.zones[0].name, expected_zone);
        assert!(!row.zones[0].ready);
        assert_eq!(
            row.zones[0].reason.as_deref(),
            Some(reasons::PER_ZONE_IMPORT_NOT_IMPLEMENTED)
        );
    }

    // ---------- Hooks into the rest of the type system -------------------

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
