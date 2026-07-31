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

    fn connection() -> ProviderConnection {
        ProviderConnection {
            endpoint: "https://vcenter.example.com/sdk".to_string(),
            credentials_ref: LocalObjectReference {
                name: "prod-vc-creds".to_string(),
            },
            insecure_skip_tls_verify: false,
            ca_bundle: None,
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
        }
    }

    fn class_spec() -> ProviderClassSpec {
        ProviderClassSpec {
            backend: "vsphere".to_string(),
            image: ProviderImage {
                repository: "ghcr.io/firestoned/banlieue".to_string(),
                tag: "v0.1.0".to_string(),
                digest: None,
                pull_policy: None,
                pull_secrets: Vec::new(),
            },
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

        assert_eq!(status.deployment_name, "wl");
        assert_eq!(status.namespace, "banlieue-system");
        assert_eq!(status.ready_replicas, 1);
        assert_eq!(status.observed_generation, Some(3));
    }

    /// A Deployment with no status yet, or none at all, must read as zero ready
    /// rather than being reported as healthy.
    #[test]
    fn workload_status_treats_a_missing_deployment_as_zero_ready() {
        let status = workload_status(None, "banlieue-system", "wl", None);
        assert_eq!(status.ready_replicas, 0);
        assert_eq!(status.observed_generation, None);
    }

    #[test]
    fn workload_status_treats_an_unpopulated_status_as_zero_ready() {
        let deployment = Deployment {
            status: Some(DeploymentStatus::default()),
            ..Default::default()
        };
        let status = workload_status(Some(&deployment), "ns", "wl", None);
        assert_eq!(status.ready_replicas, 0);
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
