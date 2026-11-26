// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `reconciler/provider.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{
        LoggingSpec, ProviderClassSpec, ProviderConnection, ProviderImage, ProviderSpec,
    };
    use banlieue_api::common::{CABundleSource, KeySelector, LocalObjectReference};
    use k8s_openapi::api::apps::v1::{Deployment, DeploymentStatus};

    // ---- watch_namespace_selector (ADR-0133 Decision 11) ------------------

    use banlieue_api::common::LabelSelector;

    fn with_selector(labels: &[(&str, &str)]) -> ProviderSpec {
        let mut spec = provider_spec();
        spec.namespace_selector = Some(LabelSelector {
            match_labels: labels
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            ..Default::default()
        });
        spec
    }

    #[test]
    fn a_provider_without_a_namespace_selector_passes_no_flag() {
        assert_eq!(watch_namespace_selector(&provider_spec(), "vsphere"), None);
    }

    #[test]
    fn the_selector_is_rendered_for_the_flag() {
        assert_eq!(
            watch_namespace_selector(&with_selector(&[("banlieue.io/tenant", "true")]), "vsphere")
                .as_deref(),
            Some("banlieue.io/tenant=true")
        );
    }

    /// A backend whose binary does not parse the flag must not be handed it:
    /// clap rejects an unknown argument at startup and the provider pod would
    /// crash-loop. Only the backends in `SHARED_PROVIDER_BACKENDS` get it.
    #[test]
    fn a_backend_that_cannot_serve_shared_namespaces_gets_no_flag() {
        let spec = with_selector(&[("banlieue.io/tenant", "true")]);
        assert_eq!(watch_namespace_selector(&spec, "firecracker"), None);
        assert_eq!(watch_namespace_selector(&spec, "some-custom-backend"), None);
    }

    #[test]
    fn every_implemented_backend_gets_the_flag() {
        let spec = with_selector(&[("banlieue.io/tenant", "true")]);
        for backend in ["vsphere", "libvirt", "proxmox", "cloud-hypervisor"] {
            assert_eq!(
                watch_namespace_selector(&spec, backend).as_deref(),
                Some("banlieue.io/tenant=true"),
                "{backend}"
            );
        }
    }

    /// `{}` serves nothing (ADR-0133): it renders no flag, so the provider
    /// serves home only, and the controller does not offer it as a fallback.
    #[test]
    fn an_empty_selector_passes_no_flag() {
        assert_eq!(
            watch_namespace_selector(&with_selector(&[]), "vsphere"),
            None
        );
    }

    fn connection() -> ProviderConnection {
        ProviderConnection {
            endpoint: "https://vcenter.example.com/sdk".to_string(),
            credentials_ref: Some(LocalObjectReference {
                name: "prod-vc-creds".to_string(),
            }),
            insecure_skip_tls_verify: false,
            ca_bundle: None,
            azure: None,
        }
    }

    fn provider_spec() -> ProviderSpec {
        ProviderSpec {
            provider_class_ref: LocalObjectReference {
                name: "vsphere".to_string(),
            },
            connection: connection(),
            capabilities: Default::default(),
            paused: false,
            use_content_library: false,
            failure_domain_name_overrides: Vec::new(),
            attestation: None,
            scheduling: None,
            namespace_selector: None,
        }
    }

    fn class_spec() -> ProviderClassSpec {
        ProviderClassSpec {
            backend: "vsphere".to_string(),
            image: Some(ProviderImage {
                repository: "ghcr.io/firestoned/banlieue".to_string(),
                tag: "v0.1.0".to_string(),
                digest: None,
                pull_policy: None,
                pull_secrets: Vec::new(),
            }),
            deployment: None,
            workload_namespace: None,
            replicas: None,
            resources: None,
            node_selector: Default::default(),
            tolerations: Vec::new(),
            logging: LoggingSpec::default(),
            additional_rules: Vec::new(),
            paused: false,
        }
    }

    // ----------------------------------------------------------------------
    // skip_reason
    // ----------------------------------------------------------------------

    #[test]
    fn an_active_provider_and_class_are_reconciled() {
        assert_eq!(skip_reason(&provider_spec(), &class_spec()), None);
    }

    #[test]
    fn a_paused_provider_is_skipped() {
        let provider = ProviderSpec {
            paused: true,
            ..provider_spec()
        };
        assert_eq!(
            skip_reason(&provider, &class_spec()),
            Some(SkipReason::ProviderPaused)
        );
    }

    #[test]
    fn a_paused_class_skips_every_provider_of_that_class() {
        let class = ProviderClassSpec {
            paused: true,
            ..class_spec()
        };
        assert_eq!(
            skip_reason(&provider_spec(), &class),
            Some(SkipReason::ClassPaused)
        );
    }

    /// A paused Provider is the more specific signal, so it wins when both are
    /// set — the reported reason should name the object the author edited.
    #[test]
    fn provider_pause_takes_precedence_over_class_pause() {
        let provider = ProviderSpec {
            paused: true,
            ..provider_spec()
        };
        let class = ProviderClassSpec {
            paused: true,
            ..class_spec()
        };
        assert_eq!(
            skip_reason(&provider, &class),
            Some(SkipReason::ProviderPaused)
        );
    }

    // ----------------------------------------------------------------------
    // ca_bundle_refs — drives the resourceNames on the generated Role
    // ----------------------------------------------------------------------

    #[test]
    fn no_ca_bundle_yields_no_extra_grants() {
        assert_eq!(ca_bundle_refs(&connection()), (None, None));
    }

    #[test]
    fn a_config_map_ca_bundle_is_reported_as_a_config_map() {
        let conn = ProviderConnection {
            ca_bundle: Some(CABundleSource {
                config_map_ref: Some(KeySelector {
                    name: "vcenter-ca".to_string(),
                    key: None,
                }),
                ..Default::default()
            }),
            ..connection()
        };
        assert_eq!(
            ca_bundle_refs(&conn),
            (Some("vcenter-ca".to_string()), None)
        );
    }

    #[test]
    fn a_secret_ca_bundle_is_reported_as_a_secret() {
        let conn = ProviderConnection {
            ca_bundle: Some(CABundleSource {
                secret_ref: Some(KeySelector {
                    name: "vcenter-ca-secret".to_string(),
                    key: None,
                }),
                ..Default::default()
            }),
            ..connection()
        };
        assert_eq!(
            ca_bundle_refs(&conn),
            (None, Some("vcenter-ca-secret".to_string()))
        );
    }

    /// An inline PEM needs no RBAC at all — nothing is read from the API.
    #[test]
    fn an_inline_ca_bundle_yields_no_extra_grants() {
        let conn = ProviderConnection {
            ca_bundle: Some(CABundleSource {
                inline: Some("-----BEGIN CERTIFICATE-----".to_string()),
                ..Default::default()
            }),
            ..connection()
        };
        assert_eq!(ca_bundle_refs(&conn), (None, None));
    }

    // ----------------------------------------------------------------------
    // workload_status
    // ----------------------------------------------------------------------

    #[test]
    fn workload_status_reports_ready_replicas() {
        let deployment = Deployment {
            status: Some(DeploymentStatus {
                ready_replicas: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };
        let status = workload_status(Some(&deployment), "banlieue-system", "wl", Some(3));

        assert_eq!(status.mode, banlieue_api::banlieue::WorkloadMode::Managed);
        assert_eq!(status.deployment_name.as_deref(), Some("wl"));
        assert_eq!(status.namespace, "banlieue-system");
        assert_eq!(status.ready_replicas, Some(1));
        assert_eq!(status.observed_generation, Some(3));
        assert_eq!(
            status.service_account_name.as_deref(),
            Some("wl"),
            "a Managed provider runs as the ServiceAccount named like its workload (ADR-0126)"
        );
    }

    /// A Deployment with no status yet, or none at all, must read as zero ready
    /// rather than being reported as healthy.
    #[test]
    fn workload_status_treats_a_missing_deployment_as_zero_ready() {
        let status = workload_status(None, "banlieue-system", "wl", None);
        assert_eq!(status.ready_replicas, Some(0));
        assert_eq!(status.observed_generation, None);
    }

    #[test]
    fn workload_status_treats_an_unpopulated_status_as_zero_ready() {
        let deployment = Deployment {
            status: Some(DeploymentStatus::default()),
            ..Default::default()
        };
        let status = workload_status(Some(&deployment), "ns", "wl", None);
        assert_eq!(status.ready_replicas, Some(0));
    }

    /// ADR-0060 Decision 3: External reports its mode and nothing about a
    /// Deployment — liveness is the provider's own Ready condition and Lease.
    #[test]
    fn external_workload_status_names_no_deployment() {
        let status = external_workload_status(
            "banlieue-system",
            "banlieue-provider-cloud-hypervisor-hv01",
            Some(2),
        );
        assert_eq!(status.mode, banlieue_api::banlieue::WorkloadMode::External);
        assert!(status.deployment_name.is_none() && status.ready_replicas.is_none());
        assert_eq!(status.namespace, "banlieue-system");
        assert_eq!(status.observed_generation, Some(2));
        assert_eq!(
            status.service_account_name.as_deref(),
            Some("banlieue-provider-cloud-hypervisor-hv01"),
            "an External provider's identity is what a host mints for (ADR-0126)"
        );
    }

    /// A Managed class with no image cannot produce a Deployment; the
    /// reconciler must stop before applying anything.
    #[test]
    fn a_managed_class_needs_an_image_and_an_external_one_does_not() {
        let mut class = class_spec();
        assert!(missing_image(&class).is_none());
        class.image = None;
        assert!(missing_image(&class).is_some());
        class.deployment = Some(banlieue_api::banlieue::ProviderDeployment::External);
        assert!(missing_image(&class).is_none());
    }

    // ----------------------------------------------------------------------
    // Finalizer
    // ----------------------------------------------------------------------

    /// The finalizer exists solely because a ClusterRoleBinding cannot be
    /// garbage-collected by a namespaced owner. Its name is API surface — it
    /// appears in `metadata.finalizers` and in any stuck-deletion runbook.
    #[test]
    fn finalizer_is_namespaced_to_banlieue() {
        assert_eq!(FINALIZER, "banlieue.io/provider-workload");
    }
}

#[cfg(test)]
mod gate_tests {
    use std::sync::Arc;

    use banlieue_api::banlieue::Provider;
    use banlieue_api::common::LocalObjectReference;
    use banlieue_provider_sdk::scope::NamespaceGate;
    use k8s_openapi::api::core::v1::Namespace;
    use kube::runtime::reflector;
    use kube::{Client, Config};

    use super::super::*;

    fn offline_client() -> Client {
        let url = "http://127.0.0.1:1".parse().expect("valid URL");
        Client::try_from(Config::new(url)).expect("client from static config")
    }

    fn provider_in(namespace: &str) -> Provider {
        let mut spec: banlieue_api::banlieue::ProviderSpec =
            serde_json::from_value(serde_json::json!({
                "providerClassRef": { "name": "vsphere" },
                "connection": { "endpoint": "https://vcenter.example.com/sdk" },
            }))
            .expect("valid spec");
        spec.provider_class_ref = LocalObjectReference {
            name: "vsphere".into(),
        };
        let mut p = Provider::new("prod", spec);
        p.metadata.namespace = Some(namespace.to_string());
        p
    }

    /// ADR-0131: a Provider outside the served set is left alone. The client
    /// points nowhere, so any API call would error: returning `Ok` proves
    /// the reconcile stopped before making one (no finalizer, no workload).
    #[tokio::test]
    async fn a_provider_in_an_unserved_namespace_is_skipped_before_any_write() {
        let (reader, _writer) = reflector::store::<Namespace>();
        let ctx = Arc::new(
            Context::new(offline_client(), "banlieue-system".into())
                .with_namespace_gate(NamespaceGate::selected(reader)),
        );
        let action = reconcile(Arc::new(provider_in("team-b")), ctx).await;
        assert_eq!(action.unwrap(), Action::await_change());
    }
}
