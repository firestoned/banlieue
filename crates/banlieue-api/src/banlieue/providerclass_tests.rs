// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `providerclass.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use kube::CustomResourceExt;

    fn sample_image() -> ProviderImage {
        ProviderImage {
            repository: "ghcr.io/firestoned/banlieue".to_string(),
            tag: "v0.1.0".to_string(),
            digest: None,
            pull_policy: None,
            pull_secrets: Vec::new(),
        }
    }

    fn sample_spec() -> ProviderClassSpec {
        ProviderClassSpec {
            backend: "vsphere".to_string(),
            image: sample_image(),
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
    // CRD shape
    // ----------------------------------------------------------------------

    #[test]
    fn providerclass_crd_has_expected_group_and_kind() {
        let crd = ProviderClass::crd();
        assert_eq!(crd.spec.group, "banlieue.io");
        assert_eq!(crd.spec.names.kind, "ProviderClass");
        assert_eq!(crd.spec.names.plural, "providerclasses");
        assert_eq!(crd.spec.versions[0].name, "v1alpha1");
    }

    /// Cluster-scoped is a deliberate ADR-0012 decision: a ProviderClass is a
    /// platform-owner concern referenced by Provider CRs in any namespace.
    #[test]
    fn providerclass_crd_is_cluster_scoped() {
        let crd = ProviderClass::crd();
        assert_eq!(crd.spec.scope, "Cluster");
    }

    #[test]
    fn providerclass_crd_serves_a_status_subresource() {
        let crd = ProviderClass::crd();
        let subresources = crd.spec.versions[0]
            .subresources
            .as_ref()
            .expect("subresources present");
        assert!(subresources.status.is_some());
    }

    // ----------------------------------------------------------------------
    // ProviderImage
    // ----------------------------------------------------------------------

    #[test]
    fn provider_image_reference_joins_repository_and_tag() {
        assert_eq!(
            sample_image().reference(),
            "ghcr.io/firestoned/banlieue:v0.1.0"
        );
    }

    #[test]
    fn provider_image_reference_preserves_registry_port() {
        let image = ProviderImage {
            repository: "registry.internal:5000/banlieue".to_string(),
            tag: "v1.2.3".to_string(),
            digest: None,
            pull_policy: None,
            pull_secrets: Vec::new(),
        };
        assert_eq!(image.reference(), "registry.internal:5000/banlieue:v1.2.3");
    }

    // ----------------------------------------------------------------------
    // Defaults
    // ----------------------------------------------------------------------

    #[test]
    fn replicas_defaults_to_one_when_unset() {
        assert_eq!(
            sample_spec().replicas_or_default(),
            DEFAULT_PROVIDER_REPLICAS
        );
        assert_eq!(sample_spec().replicas_or_default(), 1);
    }

    #[test]
    fn replicas_honours_an_explicit_value() {
        let spec = ProviderClassSpec {
            replicas: Some(3),
            ..sample_spec()
        };
        assert_eq!(spec.replicas_or_default(), 3);
    }

    /// A negative or zero replica count is meaningless for a leader-elected
    /// controller; the accessor clamps rather than emitting an invalid Deployment.
    #[test]
    fn replicas_clamps_negative_values_to_the_default() {
        let spec = ProviderClassSpec {
            replicas: Some(-2),
            ..sample_spec()
        };
        assert_eq!(spec.replicas_or_default(), DEFAULT_PROVIDER_REPLICAS);
    }

    #[test]
    fn workload_namespace_falls_back_to_the_supplied_default() {
        assert_eq!(
            sample_spec().workload_namespace_or("banlieue-system"),
            "banlieue-system"
        );
    }

    #[test]
    fn workload_namespace_honours_an_explicit_value() {
        let spec = ProviderClassSpec {
            workload_namespace: Some("tenant-a".to_string()),
            ..sample_spec()
        };
        assert_eq!(spec.workload_namespace_or("banlieue-system"), "tenant-a");
    }

    // ----------------------------------------------------------------------
    // Serde
    // ----------------------------------------------------------------------

    #[test]
    fn spec_serializes_multiword_fields_as_camel_case() {
        let spec = ProviderClassSpec {
            workload_namespace: Some("banlieue-system".to_string()),
            ..sample_spec()
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("workloadNamespace").is_some());
        assert!(json.get("workload_namespace").is_none());
    }

    /// Empty collections and `paused: false` must not be serialized, so a
    /// server-side apply from the operator never claims ownership of fields the
    /// author did not set.
    #[test]
    fn spec_omits_empty_collections_and_false_paused() {
        let json = serde_json::to_value(sample_spec()).unwrap();
        assert!(json.get("nodeSelector").is_none());
        assert!(json.get("tolerations").is_none());
        assert!(json.get("additionalRules").is_none());
        assert!(json.get("paused").is_none());
        assert!(json.get("logging").is_none());
    }

    #[test]
    fn spec_deserializes_from_a_minimal_document() {
        let spec: ProviderClassSpec = serde_json::from_str(
            r#"{"backend":"vsphere","image":{"repository":"ghcr.io/firestoned/banlieue","tag":"v0.1.0"}}"#,
        )
        .expect("minimal spec deserializes");
        assert_eq!(spec.backend, "vsphere");
        assert_eq!(spec.image.reference(), "ghcr.io/firestoned/banlieue:v0.1.0");
        assert!(!spec.paused);
        assert!(spec.replicas.is_none());
    }

    #[test]
    fn spec_round_trips_through_json() {
        let spec = ProviderClassSpec {
            workload_namespace: Some("banlieue-system".to_string()),
            replicas: Some(2),
            paused: true,
            ..sample_spec()
        };
        let round_tripped: ProviderClassSpec =
            serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
        assert_eq!(round_tripped, spec);
    }

    #[test]
    fn image_pull_policy_serializes_as_kubernetes_spells_it() {
        let image = ProviderImage {
            pull_policy: Some(ImagePullPolicy::IfNotPresent),
            ..sample_image()
        };
        let json = serde_json::to_value(&image).unwrap();
        assert_eq!(json["pullPolicy"], "IfNotPresent");
    }

    /// Go's YAML 1.1 parser reads bare `On`/`Off`/`Yes`/`No` as booleans, which
    /// makes the apiserver reject a CRD whose enum emits one. None of these
    /// variants may drift into such a token.
    #[test]
    fn image_pull_policy_variants_are_not_yaml_booleans() {
        const YAML_BOOLEANS: [&str; 8] = ["on", "off", "yes", "no", "true", "false", "y", "n"];
        for policy in [
            ImagePullPolicy::Always,
            ImagePullPolicy::IfNotPresent,
            ImagePullPolicy::Never,
        ] {
            let rendered = serde_json::to_value(policy).unwrap();
            let rendered = rendered.as_str().unwrap().to_ascii_lowercase();
            assert!(
                !YAML_BOOLEANS.contains(&rendered.as_str()),
                "{rendered} is a YAML 1.1 boolean"
            );
        }
    }

    #[test]
    fn logging_spec_default_is_empty() {
        let logging = LoggingSpec::default();
        assert!(logging.is_empty());
        assert!(logging.level.is_none());
        assert!(logging.format.is_none());
    }

    #[test]
    fn logging_spec_with_a_level_is_not_empty() {
        let logging = LoggingSpec {
            level: Some("debug".to_string()),
            format: None,
        };
        assert!(!logging.is_empty());
    }

    // ----------------------------------------------------------------------
    // Status
    // ----------------------------------------------------------------------

    #[test]
    fn status_default_is_empty() {
        let status = ProviderClassStatus::default();
        assert!(status.conditions.is_empty());
        assert!(status.observed_generation.is_none());
        assert!(status.providers.is_none());
    }

    #[test]
    fn status_serializes_provider_count_as_camel_case() {
        let status = ProviderClassStatus {
            providers: Some(2),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["providers"], 2);
        assert!(json.get("conditions").is_none());
    }

    // ---------- image references ------------------------------------------

    fn image(tag: &str, digest: Option<&str>) -> ProviderImage {
        ProviderImage {
            repository: "ghcr.io/firestoned/banlieue".to_string(),
            tag: tag.to_string(),
            digest: digest.map(str::to_string),
            pull_policy: None,
            pull_secrets: Vec::new(),
        }
    }

    #[test]
    fn a_tag_alone_produces_repository_colon_tag() {
        assert_eq!(
            image("v0.1.0", None).reference(),
            "ghcr.io/firestoned/banlieue:v0.1.0"
        );
    }

    #[test]
    fn a_digest_pins_the_exact_image() {
        // A mutable tag makes the running version unknowable AND makes the
        // Deployment spec identical across pushes, so nothing rolls: the pod
        // keeps its old layers even under imagePullPolicy: Always, which only
        // applies when a pod is created. A digest changes the spec, so a new
        // image actually triggers a rollout.
        let r = image("v0.1.0", Some("sha256:abc123")).reference();
        assert!(r.ends_with("@sha256:abc123"), "{r}");
    }

    #[test]
    fn tag_and_digest_together_stay_readable_and_immutable() {
        // `repo:tag@digest` is a valid reference. The tag documents what the
        // digest is *meant* to be; the digest is what actually gets pulled.
        assert_eq!(
            image("v0.1.0", Some("sha256:abc123")).reference(),
            "ghcr.io/firestoned/banlieue:v0.1.0@sha256:abc123"
        );
    }

    #[test]
    fn a_digest_with_no_tag_omits_the_colon() {
        // `repo:@sha256:...` is not a valid reference.
        let r = image("", Some("sha256:abc123")).reference();
        assert_eq!(r, "ghcr.io/firestoned/banlieue@sha256:abc123");
        assert!(!r.contains(":@"), "{r}");
    }
}
