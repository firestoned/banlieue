// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `workload.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::banlieue::{ImagePullPolicy, LoggingSpec, ProviderClassSpec, ProviderImage};
    use k8s_openapi::api::rbac::v1::PolicyRule;

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

    fn inputs<'a>(class: &'a ProviderClassSpec) -> WorkloadInputs<'a> {
        WorkloadInputs {
            class_name: "vsphere",
            class,
            provider_name: "prod-vc",
            provider_namespace: "banlieue-system",
            workload_namespace: "banlieue-system",
            credentials_secret: Some("prod-vc-creds"),
            build_toleration: &[],
            ca_bundle_config_map: None,
            ca_bundle_secret: None,
            watch_namespace_selector: None,
            azure_openbao: None,
            owner: None,
        }
    }

    fn container(deployment: &Deployment) -> &k8s_openapi::api::core::v1::Container {
        &deployment.spec.template.spec.as_ref().unwrap().containers[0]
    }

    // ----------------------------------------------------------------------
    // Deployment
    // ----------------------------------------------------------------------

    // ----------------------------------------------------------------------
    // --watch-namespace-selector (ADR-0133 Decision 11)
    // ----------------------------------------------------------------------

    fn args_of(deployment: &Deployment) -> Vec<String> {
        container(deployment).args.clone().unwrap()
    }

    /// With no selector the provider serves its home namespace only, as before.
    #[test]
    fn no_namespace_selector_renders_no_flag() {
        let class = class_spec();
        let args = args_of(&build_deployment(&inputs(&class)));
        assert!(!args.iter().any(|a| a == "--watch-namespace-selector"));
    }

    #[test]
    fn a_namespace_selector_is_passed_to_the_provider() {
        let class = class_spec();
        let mut i = inputs(&class);
        i.watch_namespace_selector = Some("banlieue.io/tenant=true");
        let args = args_of(&build_deployment(&i));
        let at = args
            .iter()
            .position(|a| a == "--watch-namespace-selector")
            .expect("flag rendered");
        assert_eq!(args[at + 1], "banlieue.io/tenant=true");
    }

    /// `--namespace` stays the Provider's own namespace: its home, where the
    /// credentials are. The selector adds namespaces; it never replaces home.
    #[test]
    fn the_selector_does_not_change_the_home_namespace_flag() {
        let class = class_spec();
        let mut i = inputs(&class);
        i.watch_namespace_selector = Some("banlieue.io/tenant=true");
        let args = args_of(&build_deployment(&i));
        let at = args.iter().position(|a| a == "--namespace").unwrap();
        assert_eq!(args[at + 1], "banlieue-system");
    }

    #[test]
    fn deployment_runs_the_backend_subcommand_for_this_provider() {
        let class = class_spec();
        let deployment = build_deployment(&inputs(&class));
        let args = container(&deployment).args.as_ref().unwrap();

        assert_eq!(args[0], "provider");
        assert_eq!(args[1], "vsphere");
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--provider-name" && w[1] == "prod-vc"),
            "args must scope the workload to its Provider: {args:?}"
        );
    }

    /// The whole point of per-instance topology (ADR-0003): each workload gets
    /// its own Lease, so two backends never contend for one leader election.
    #[test]
    fn deployment_uses_a_per_instance_leader_election_lease() {
        let class = class_spec();
        let deployment = build_deployment(&inputs(&class));
        let args = container(&deployment).args.as_ref().unwrap();

        assert!(
            args.windows(2).any(|w| {
                w[0] == "--leader-election-id" && w[1] == "banlieue-provider-vsphere-prod-vc"
            }),
            "expected a per-instance lease name: {args:?}"
        );
    }

    #[test]
    fn deployment_pins_the_image_from_the_class() {
        let class = class_spec();
        let deployment = build_deployment(&inputs(&class));
        assert_eq!(
            container(&deployment).image.as_deref(),
            Some("ghcr.io/firestoned/banlieue:v0.1.0")
        );
    }

    #[test]
    fn deployment_honours_the_class_pull_policy() {
        let mut class = class_spec();
        class.image.as_mut().expect("image").pull_policy = Some(ImagePullPolicy::Always);
        let deployment = build_deployment(&inputs(&class));
        assert_eq!(
            container(&deployment).image_pull_policy.as_deref(),
            Some("Always")
        );
    }

    #[test]
    fn deployment_defaults_to_a_single_replica() {
        let class = class_spec();
        let deployment = build_deployment(&inputs(&class));
        assert_eq!(deployment.spec.replicas, Some(1));
    }

    #[test]
    fn deployment_honours_an_explicit_replica_count() {
        let mut class = class_spec();
        class.replicas = Some(2);
        let deployment = build_deployment(&inputs(&class));
        assert_eq!(deployment.spec.replicas, Some(2));
    }

    #[test]
    fn deployment_binds_the_per_instance_service_account() {
        let class = class_spec();
        let deployment = build_deployment(&inputs(&class));
        let pod_spec = deployment.spec.template.spec.as_ref().unwrap();
        assert_eq!(
            pod_spec.service_account_name.as_deref(),
            Some("banlieue-provider-vsphere-prod-vc")
        );
    }

    /// A provider that spawns Jobs (the libvirt image import) runs them under
    /// its own ServiceAccount, so it must be able to learn that name at
    /// runtime. Re-deriving it from `naming` inside each provider would break
    /// silently the day the scheme changes.
    #[test]
    fn deployment_exposes_its_own_service_account_name_via_the_downward_api() {
        let class = class_spec();
        let deployment = build_deployment(&inputs(&class));
        let env = deployment.spec.template.spec.as_ref().unwrap().containers[0]
            .env
            .as_ref()
            .expect("container must carry downward-API env");

        let sa = env
            .iter()
            .find(|e| e.name == "POD_SERVICE_ACCOUNT")
            .expect("POD_SERVICE_ACCOUNT must be present");
        assert_eq!(
            sa.value_from
                .as_ref()
                .and_then(|s| s.field_ref.as_ref())
                .map(|f| f.field_path.as_str()),
            Some("spec.serviceAccountName"),
            "must come from the downward API, never a hardcoded value"
        );
        // The leader-election identity must not have been displaced.
        for name in ["POD_NAME", "POD_NAMESPACE"] {
            assert!(
                env.iter().any(|e| e.name == name),
                "{name} must still be present"
            );
        }
    }

    /// A Deployment whose selector does not match its pod template is rejected
    /// by the apiserver; this is easy to break and silent until apply time.
    #[test]
    fn deployment_selector_matches_its_pod_template_labels() {
        let class = class_spec();
        let deployment = build_deployment(&inputs(&class));
        let spec = &deployment.spec;
        let selector = spec.selector.match_labels.as_ref().unwrap();
        let template_labels = spec
            .template
            .metadata
            .as_ref()
            .unwrap()
            .labels
            .as_ref()
            .unwrap();

        for (key, value) in selector {
            assert_eq!(
                template_labels.get(key),
                Some(value),
                "selector {key} not matched by pod template"
            );
        }
    }

    #[test]
    fn deployment_container_runs_hardened() {
        let class = class_spec();
        let deployment = build_deployment(&inputs(&class));
        let security = container(&deployment).security_context.as_ref().unwrap();

        assert_eq!(security.allow_privilege_escalation, Some(false));
        assert_eq!(security.read_only_root_filesystem, Some(true));
        assert_eq!(
            security.capabilities.as_ref().unwrap().drop.as_deref(),
            Some(["ALL".to_string()].as_slice())
        );
    }

    #[test]
    fn deployment_pod_runs_as_non_root() {
        let class = class_spec();
        let deployment = build_deployment(&inputs(&class));
        let pod_spec = deployment.spec.template.spec.as_ref().unwrap();
        let security = pod_spec.security_context.as_ref().unwrap();
        assert_eq!(security.run_as_non_root, Some(true));
    }

    #[test]
    fn deployment_passes_logging_configuration_when_set() {
        let mut class = class_spec();
        class.logging = LoggingSpec {
            level: Some("debug".to_string()),
            format: Some("json".to_string()),
        };
        let deployment = build_deployment(&inputs(&class));
        let args = container(&deployment).args.as_ref().unwrap();

        assert!(
            args.windows(2)
                .any(|w| w[0] == "--log-level" && w[1] == "debug")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--log-format" && w[1] == "json")
        );
    }

    #[test]
    fn deployment_omits_logging_flags_when_unset() {
        let class = class_spec();
        let deployment = build_deployment(&inputs(&class));
        let args = container(&deployment).args.as_ref().unwrap();
        assert!(!args.iter().any(|a| a == "--log-level"));
        assert!(!args.iter().any(|a| a == "--log-format"));
    }

    #[test]
    fn deployment_lands_in_the_workload_namespace() {
        let class = class_spec();
        let mut input = inputs(&class);
        input.workload_namespace = "banlieue-system";
        input.provider_namespace = "tenant-a";
        let deployment = build_deployment(&input);
        assert_eq!(
            deployment.metadata.namespace.as_deref(),
            Some("banlieue-system")
        );
    }

    /// `--namespace` scopes the provider's Provider watch. It must point at the
    /// Provider's namespace, not the workload's, or the provider watches the
    /// wrong place when a class pins `workloadNamespace`.
    #[test]
    fn deployment_scopes_the_watch_to_the_provider_namespace() {
        let class = class_spec();
        let mut input = inputs(&class);
        input.workload_namespace = "banlieue-system";
        input.provider_namespace = "tenant-a";
        let deployment = build_deployment(&input);
        let args = container(&deployment).args.as_ref().unwrap();
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--namespace" && w[1] == "tenant-a"),
            "{args:?}"
        );
    }

    // ----------------------------------------------------------------------
    // Role — least privilege is the reason per-instance exists
    // ----------------------------------------------------------------------

    /// `resourceNames` is what makes the credentials grant worth having: this
    /// Role must reach exactly one Secret, not every Secret in the namespace.
    #[test]
    fn role_grants_get_on_only_the_referenced_credentials_secret() {
        let class = class_spec();
        let role = build_role(&inputs(&class));
        let rules = role.rules.as_ref().unwrap();

        let secret_rule = rules
            .iter()
            .find(|r| {
                r.resources
                    .as_ref()
                    .is_some_and(|rs| rs.contains(&"secrets".to_string()))
            })
            .expect("a secrets rule");

        assert_eq!(
            secret_rule.resource_names.as_deref(),
            Some(["prod-vc-creds".to_string()].as_slice())
        );
        assert_eq!(secret_rule.verbs, vec!["get".to_string()]);
    }

    /// Kubernetes ignores `resourceNames` for `list`/`watch`, so granting them
    /// alongside named resources would silently widen the grant to every object
    /// of that type. The Role must never ask for them on Secrets.
    #[test]
    fn role_never_grants_list_or_watch_on_secrets() {
        let class = class_spec();
        let role = build_role(&inputs(&class));

        for rule in role.rules.as_ref().unwrap() {
            let Some(resources) = rule.resources.as_ref() else {
                continue;
            };
            if !resources.contains(&"secrets".to_string()) {
                continue;
            }
            assert!(
                !rule.verbs.contains(&"list".to_string())
                    && !rule.verbs.contains(&"watch".to_string()),
                "resourceNames does not constrain list/watch: {rule:?}"
            );
        }
    }

    #[test]
    fn role_includes_the_ca_bundle_sources_when_present() {
        let class = class_spec();
        let mut input = inputs(&class);
        input.ca_bundle_config_map = Some("vcenter-ca");
        input.ca_bundle_secret = Some("vcenter-ca-secret");
        let role = build_role(&input);
        let rules = role.rules.as_ref().unwrap();

        let cm_rule = rules
            .iter()
            .find(|r| {
                r.resources
                    .as_ref()
                    .is_some_and(|rs| rs.contains(&"configmaps".to_string()))
            })
            .expect("a configmaps rule");
        assert_eq!(
            cm_rule.resource_names.as_deref(),
            Some(["vcenter-ca".to_string()].as_slice())
        );

        let secret_rule = rules
            .iter()
            .find(|r| {
                r.resources
                    .as_ref()
                    .is_some_and(|rs| rs.contains(&"secrets".to_string()))
            })
            .unwrap();
        let names = secret_rule.resource_names.as_ref().unwrap();
        assert!(names.contains(&"prod-vc-creds".to_string()));
        assert!(names.contains(&"vcenter-ca-secret".to_string()));
    }

    #[test]
    fn role_omits_a_configmap_rule_when_no_ca_bundle_is_referenced() {
        let class = class_spec();
        let role = build_role(&inputs(&class));
        assert!(
            !role.rules.as_ref().unwrap().iter().any(|r| r
                .resources
                .as_ref()
                .is_some_and(|rs| rs.contains(&"configmaps".to_string()))),
            "no CA bundle means no configmap grant"
        );
    }

    /// The provider's leader-election Lease is per-instance, so every verb that
    /// `resourceNames` can constrain is narrowed to that one Lease name.
    #[test]
    fn role_scopes_lease_access_to_the_per_instance_lease() {
        let class = class_spec();
        let role = build_role(&inputs(&class));
        let lease_rules: Vec<_> = role
            .rules
            .as_ref()
            .unwrap()
            .iter()
            .filter(|r| {
                r.resources
                    .as_ref()
                    .is_some_and(|rs| rs.contains(&"leases".to_string()))
            })
            .collect();
        assert!(!lease_rules.is_empty(), "expected a leases grant");

        let named = lease_rules
            .iter()
            .find(|r| r.resource_names.is_some())
            .expect("a resourceNames-scoped leases rule");
        assert_eq!(
            named.resource_names.as_deref(),
            Some(["banlieue-provider-vsphere-prod-vc".to_string()].as_slice())
        );
        assert!(named.verbs.contains(&"get".to_string()));
        assert!(named.verbs.contains(&"update".to_string()));

        // `create` cannot be constrained by resourceNames — Kubernetes ignores
        // the field for it — so it must live in its own unconstrained rule
        // rather than silently widening a named one.
        assert!(
            !named.verbs.contains(&"create".to_string()),
            "create must not share a rule with resourceNames"
        );
        let unnamed_verbs: Vec<_> = lease_rules
            .iter()
            .filter(|r| r.resource_names.is_none())
            .flat_map(|r| r.verbs.iter().cloned())
            .collect();
        assert_eq!(
            unnamed_verbs,
            vec!["create".to_string()],
            "create is the only verb that may go unscoped"
        );
    }

    #[test]
    fn role_appends_additional_rules_from_the_class() {
        let mut class = class_spec();
        class.additional_rules = vec![PolicyRule {
            api_groups: Some(vec!["example.com".to_string()]),
            resources: Some(vec!["widgets".to_string()]),
            verbs: vec!["get".to_string()],
            ..Default::default()
        }];
        let role = build_role(&inputs(&class));
        assert!(
            role.rules.as_ref().unwrap().iter().any(|r| r
                .resources
                .as_ref()
                .is_some_and(|rs| rs.contains(&"widgets".to_string()))),
            "additionalRules must be appended"
        );
    }

    #[test]
    fn role_is_created_next_to_the_secret_it_grants() {
        let class = class_spec();
        let mut input = inputs(&class);
        input.workload_namespace = "banlieue-system";
        input.provider_namespace = "tenant-a";
        let role = build_role(&input);
        assert_eq!(role.metadata.namespace.as_deref(), Some("tenant-a"));
    }

    // ----------------------------------------------------------------------
    // Bindings
    // ----------------------------------------------------------------------

    #[test]
    fn role_binding_targets_the_service_account_in_its_own_namespace() {
        let class = class_spec();
        let mut input = inputs(&class);
        input.workload_namespace = "banlieue-system";
        input.provider_namespace = "tenant-a";
        let binding = build_role_binding(&input);

        assert_eq!(binding.metadata.namespace.as_deref(), Some("tenant-a"));
        let subject = &binding.subjects.as_ref().unwrap()[0];
        assert_eq!(subject.kind, "ServiceAccount");
        assert_eq!(subject.name, "banlieue-provider-vsphere-prod-vc");
        assert_eq!(
            subject.namespace.as_deref(),
            Some("banlieue-system"),
            "subject must name the namespace the ServiceAccount actually lives in"
        );
        assert_eq!(binding.role_ref.kind, "Role");
    }

    #[test]
    fn cluster_role_binding_grants_the_shared_backend_cluster_role() {
        let class = class_spec();
        let binding = build_cluster_role_binding(&inputs(&class));
        assert_eq!(binding.role_ref.kind, "ClusterRole");
        assert_eq!(binding.role_ref.name, "banlieue-provider-vsphere");
        assert_eq!(
            binding.metadata.name.as_deref(),
            Some("banlieue-provider-vsphere-banlieue-system-prod-vc"),
            "a cluster-scoped object must be namespace-qualified"
        );
    }

    /// A cluster-scoped object owned by a namespaced one is deleted immediately
    /// by the garbage collector, so this binding must never carry an owner.
    #[test]
    fn cluster_role_binding_never_carries_an_owner_reference() {
        let class = class_spec();
        let mut input = inputs(&class);
        input.owner = Some(owner_reference("prod-vc", "uid-1234"));
        let binding = build_cluster_role_binding(&input);
        assert!(binding.metadata.owner_references.is_none());
    }

    // ----------------------------------------------------------------------
    // The shared-serving ClusterRole (ADR-0134)
    // ----------------------------------------------------------------------

    fn ch_class_spec() -> ProviderClassSpec {
        let mut c = class_spec();
        c.backend = "cloud-hypervisor".to_string();
        c
    }

    /// A host that is not shared keeps today's namespaced-only token.
    #[test]
    fn no_selector_means_no_shared_cluster_role_binding() {
        let class = ch_class_spec();
        assert!(build_shared_cluster_role_binding(&inputs(&class)).is_none());
        assert!(
            build_workload(&inputs(&class), "build")
                .shared_cluster_role_binding
                .is_none()
        );
    }

    #[test]
    fn a_selector_binds_the_shared_cluster_role_to_the_provider_account() {
        let class = ch_class_spec();
        let mut i = inputs(&class);
        i.watch_namespace_selector = Some("banlieue.io/tenant=true");
        let binding = build_shared_cluster_role_binding(&i).unwrap();
        assert_eq!(binding.role_ref.kind, "ClusterRole");
        assert_eq!(
            binding.role_ref.name,
            "banlieue-provider-cloud-hypervisor-shared"
        );
        let subject = &binding.subjects.as_ref().unwrap()[0];
        assert_eq!(subject.kind, "ServiceAccount");
        assert_eq!(subject.name, workload_name("vsphere", "prod-vc"));
        assert!(binding.metadata.owner_references.is_none());
        assert_ne!(
            binding.metadata.name,
            build_cluster_role_binding(&i).metadata.name,
            "the two bindings must not share a name"
        );
        assert_eq!(
            binding.metadata.name.as_deref(),
            Some(
                shared_cluster_role_binding_name("vsphere", "banlieue-system", "prod-vc").as_str()
            )
        );
        assert!(
            build_workload(&i, "build")
                .shared_cluster_role_binding
                .is_some()
        );
    }

    /// Only a backend that ships a `-shared` ClusterRole gets the binding: the
    /// others hold their machine access in their one ClusterRole already.
    #[test]
    fn a_backend_without_a_shared_cluster_role_gets_no_binding() {
        let class = class_spec();
        let mut i = inputs(&class);
        i.watch_namespace_selector = Some("banlieue.io/tenant=true");
        assert!(build_shared_cluster_role_binding(&i).is_none());
        assert!(!wants_shared_cluster_role("vsphere", Some("a=b")));
        assert!(wants_shared_cluster_role("cloud-hypervisor", Some("a=b")));
        assert!(!wants_shared_cluster_role("cloud-hypervisor", None));
    }

    // ----------------------------------------------------------------------
    // Ownership
    // ----------------------------------------------------------------------

    #[test]
    fn same_namespace_objects_carry_the_owner_reference() {
        let class = class_spec();
        let mut input = inputs(&class);
        input.owner = Some(owner_reference("prod-vc", "uid-1234"));
        let set = build_workload(&input, "banlieue-imagebuild");

        for (kind, meta) in [
            ("ServiceAccount", &set.service_account.metadata),
            (
                "Deployment",
                &set.deployment.as_ref().expect("Managed deploys").metadata,
            ),
            ("Role", &set.role.metadata),
            ("RoleBinding", &set.role_binding.metadata),
        ] {
            let owners = meta
                .owner_references
                .as_ref()
                .unwrap_or_else(|| panic!("{kind} should be owned"));
            assert_eq!(owners[0].uid, "uid-1234");
            assert_eq!(owners[0].controller, Some(true));
            assert_eq!(owners[0].block_owner_deletion, Some(true));
        }
    }

    /// Cross-namespace owner references are invalid; when a class pins
    /// `workloadNamespace` elsewhere the workload objects must be left
    /// unowned and cleaned up by the finalizer instead.
    #[test]
    fn cross_namespace_objects_are_left_unowned() {
        let class = class_spec();
        let mut input = inputs(&class);
        input.workload_namespace = "banlieue-system";
        input.provider_namespace = "tenant-a";
        input.owner = Some(owner_reference("prod-vc", "uid-1234"));
        let set = build_workload(&input, "banlieue-imagebuild");

        assert!(
            set.deployment
                .as_ref()
                .expect("Managed deploys")
                .metadata
                .owner_references
                .is_none(),
            "Deployment is in another namespace than its Provider"
        );
        assert!(set.service_account.metadata.owner_references.is_none());
        // The Role lives with the Provider, so it can still be owned.
        assert!(set.role.metadata.owner_references.is_some());
    }

    // ----------------------------------------------------------------------
    // Whole set
    // ----------------------------------------------------------------------

    /// The four namespaced objects share one derived name; the cluster-scoped
    /// ClusterRoleBinding does NOT, because it has no namespace to disambiguate
    /// it (see the collision test below).
    #[test]
    fn namespaced_workload_objects_share_one_derived_name() {
        let class = class_spec();
        let set = build_workload(&inputs(&class), "banlieue-imagebuild");
        let expected = "banlieue-provider-vsphere-prod-vc";

        assert_eq!(set.service_account.metadata.name.as_deref(), Some(expected));
        assert_eq!(
            set.deployment
                .as_ref()
                .expect("Managed deploys")
                .metadata
                .name
                .as_deref(),
            Some(expected)
        );
        assert_eq!(set.role.metadata.name.as_deref(), Some(expected));
        assert_eq!(set.role_binding.metadata.name.as_deref(), Some(expected));
    }

    /// Two Providers with the same name and class in different namespaces must
    /// not share a ClusterRoleBinding. They would each server-side-apply a
    /// subject pointing at their own namespace, so last writer wins and the
    /// loser silently loses its permissions — reachable in any multi-tenant
    /// install, which is precisely what per-instance topology is for.
    #[test]
    fn cluster_scoped_objects_do_not_collide_across_namespaces() {
        let class = class_spec();

        // Workloads default to their Provider's own namespace, so set both.
        let mut a = inputs(&class);
        a.provider_namespace = "tenant-a";
        a.workload_namespace = "tenant-a";
        let mut b = inputs(&class);
        b.provider_namespace = "tenant-b";
        b.workload_namespace = "tenant-b";

        let a_binding = build_cluster_role_binding(&a);
        let b_binding = build_cluster_role_binding(&b);

        assert_ne!(
            a_binding.metadata.name, b_binding.metadata.name,
            "same Provider name in two namespaces collided on one ClusterRoleBinding"
        );

        // …and each must point at its own namespace's ServiceAccount.
        assert_eq!(
            a_binding.subjects.as_ref().unwrap()[0].namespace.as_deref(),
            Some("tenant-a")
        );
        assert_eq!(
            b_binding.subjects.as_ref().unwrap()[0].namespace.as_deref(),
            Some("tenant-b")
        );
    }

    #[test]
    fn workload_set_labels_every_object_with_its_provider() {
        let class = class_spec();
        let set = build_workload(&inputs(&class), "banlieue-imagebuild");

        for labels in [
            set.service_account.metadata.labels.as_ref(),
            set.deployment
                .as_ref()
                .expect("Managed deploys")
                .metadata
                .labels
                .as_ref(),
            set.role.metadata.labels.as_ref(),
            set.role_binding.metadata.labels.as_ref(),
            set.cluster_role_binding.metadata.labels.as_ref(),
        ] {
            assert_eq!(
                labels.unwrap().get(crate::naming::LABEL_PROVIDER),
                Some(&"prod-vc".to_string())
            );
        }
    }

    // ---------- import identity (ADR-0016 §4) -----------------------------

    /// The import Job runs in the privileged build namespace but must read the
    /// Provider and its credentials, which live with the Provider. That is a
    /// cross-namespace read, so the subject must name the build namespace.
    #[test]
    fn the_import_binding_names_a_subject_in_the_build_namespace() {
        let class = class_spec();
        let rb = build_import_role_binding(&inputs(&class), "banlieue-imagebuild");
        let subject = &rb.subjects.as_ref().expect("subjects")[0];
        assert_eq!(subject.kind, "ServiceAccount");
        assert_eq!(subject.name, IMPORT_SERVICE_ACCOUNT);
        assert_eq!(
            subject.namespace.as_deref(),
            Some("banlieue-imagebuild"),
            "the import identity lives in the build namespace, not the Provider's"
        );
        assert_eq!(
            rb.metadata.namespace.as_deref(),
            Some("banlieue-system"),
            "the binding must live where the Role and the Secret are"
        );
    }

    /// The import Job is a data mover. It reads three objects by name and
    /// nothing else — no list, no watch, no write, and above all no `jobs`.
    ///
    /// ADR-0016 is explicit that it must NOT reuse the provider controller's
    /// ServiceAccount: that identity can create Jobs (ADR-0011), so a workload
    /// in the privileged build namespace holding it could create further
    /// privileged pods.
    #[test]
    fn the_import_role_is_read_only_and_scoped_by_name() {
        let mut class = class_spec();
        class.backend = "libvirt".to_string();
        let mut i = inputs(&class);
        i.ca_bundle_config_map = Some("prod-vc-ca");
        let role = build_import_role(&i);

        let rules = role.rules.expect("rules");
        for r in &rules {
            for v in &r.verbs {
                assert_eq!(
                    v, "get",
                    "the import identity must be read-only; found {v} on {:?}",
                    r.resources
                );
            }
            assert!(
                r.resource_names.as_ref().is_some_and(|n| !n.is_empty()),
                "every import rule must be resourceNames-scoped: {:?}",
                r.resources
            );
        }

        let resources: Vec<String> = rules
            .iter()
            .flat_map(|r| r.resources.clone().unwrap_or_default())
            .collect();
        assert!(resources.contains(&"providers".to_string()));
        assert!(resources.contains(&"secrets".to_string()));
        assert!(resources.contains(&"configmaps".to_string()));
        assert!(
            !resources.contains(&"jobs".to_string()),
            "the import identity must never be able to create Jobs"
        );
    }

    /// No CA ConfigMap on the Provider means no ConfigMap rule at all — an
    /// empty `resourceNames` would widen the grant to every ConfigMap.
    #[test]
    fn no_ca_config_map_means_no_config_map_rule() {
        let class = class_spec();
        let role = build_import_role(&inputs(&class));
        let resources: Vec<String> = role
            .rules
            .unwrap_or_default()
            .iter()
            .flat_map(|r| r.resources.clone().unwrap_or_default())
            .collect();
        assert!(!resources.contains(&"configmaps".to_string()));
    }

    /// The import Role must be a distinct object from the controller's, or
    /// binding one would grant the other's permissions.
    #[test]
    fn the_import_role_is_not_the_controller_role() {
        let class = class_spec();
        let i = inputs(&class);
        assert_ne!(
            build_import_role(&i).metadata.name,
            build_role(&i).metadata.name
        );
    }

    // ---------- build scheduling propagation ------------------------------

    /// A provider creates import Jobs that mount the artifacts PVC. Where the
    /// cluster's storage is node-local that PV is pinned to the node the build
    /// ran on, so the Job must be able to land there — which means the
    /// provider needs the same selector and toleration the imagebuilder was
    /// given. It cannot default them: they name site-specific labels.
    ///
    /// Observed as `0/4 nodes are available: 1 node(s) had untolerated
    /// taint(s), 3 node(s) didn't match PersistentVolume's node affinity`.
    #[test]
    fn the_provider_deployment_forwards_build_scheduling() {
        let class = class_spec();
        let tolerations = vec!["dedicated=imagebuild:NoSchedule".to_string()];
        let mut i = inputs(&class);
        i.build_toleration = &tolerations;
        let deployment = build_deployment(&i);
        let args = container(&deployment).args.as_ref().unwrap();

        assert!(
            !args.iter().any(|a| a == "--build-node-selector"),
            "an import Job's placement follows its PVC; a selector would \
             over-constrain it: {args:?}"
        );
        assert!(
            args.windows(2).any(|w| {
                w[0] == "--build-toleration" && w[1] == "dedicated=imagebuild:NoSchedule"
            }),
            "provider must receive the build toleration: {args:?}"
        );
    }

    #[test]
    fn no_build_scheduling_means_no_flags() {
        // Passing empty flags would make clap reject them; omit instead.
        let class = class_spec();
        let deployment = build_deployment(&inputs(&class));
        let args = container(&deployment).args.as_ref().unwrap();
        assert!(
            !args.iter().any(|a| a == "--build-node-selector"),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a == "--build-toleration"), "{args:?}");
    }

    /// The import Job runs the **same binary** as the provider that spawned
    /// it, so it must run the same image. Leaving the provider to fall back to
    /// its compiled-in default means a provider on one image spawns Jobs on
    /// another — version skew by construction, and on any cluster not running
    /// the released tag the Job simply cannot pull (observed as
    /// `ImagePullBackOff` on `:v0.1.0` while the provider ran `:local-dev`).
    #[test]
    fn the_provider_is_told_which_image_its_import_jobs_should_run() {
        let class = class_spec();
        let deployment = build_deployment(&inputs(&class));
        let args = container(&deployment).args.as_ref().unwrap();
        let want = class.image.as_ref().expect("image").reference();

        assert!(
            args.windows(2)
                .any(|w| w[0] == "--import-image" && w[1] == want),
            "provider must be told to run {want} for imports: {args:?}"
        );
        assert_eq!(
            container(&deployment).image.as_deref(),
            Some(want.as_str()),
            "and it must be the image the provider itself runs"
        );
    }

    // ----------------------------------------------------------------------
    // External (ADR-0060 Decision 3)
    // ----------------------------------------------------------------------

    fn external_class() -> ProviderClassSpec {
        ProviderClassSpec {
            backend: "cloud-hypervisor".to_string(),
            image: None,
            deployment: Some(banlieue_api::banlieue::ProviderDeployment::External),
            additional_rules: vec![PolicyRule {
                api_groups: Some(vec!["infrastructure.banlieue.io".into()]),
                resources: Some(vec!["cloudhypervisormachines".into()]),
                verbs: vec!["get".into(), "list".into(), "watch".into()],
                ..Default::default()
            }],
            ..class_spec()
        }
    }

    fn external_inputs(class: &ProviderClassSpec) -> WorkloadInputs<'_> {
        WorkloadInputs {
            class_name: "cloud-hypervisor",
            provider_name: "ch-host-1",
            credentials_secret: None,
            ..inputs(class)
        }
    }

    fn rule_for<'r>(role: &'r Role, group: &str, resource: &str) -> Vec<&'r PolicyRule> {
        role.rules
            .as_ref()
            .unwrap()
            .iter()
            .filter(|r| {
                r.api_groups.as_deref() == Some(&[group.to_string()])
                    && r.resources.as_deref() == Some(&[resource.to_string()])
            })
            .collect()
    }

    #[test]
    fn external_gets_an_identity_and_rbac_but_no_deployment_or_import_access() {
        let class = external_class();
        let set = build_workload(&external_inputs(&class), "banlieue-imagebuild");
        assert!(set.deployment.is_none(), "nothing is deployed for External");
        assert!(set.import_role.is_none() && set.import_role_binding.is_none());
        assert_eq!(
            set.service_account.metadata.name.as_deref(),
            Some("banlieue-provider-cloud-hypervisor-ch-host-1")
        );
        assert!(set.cluster_role_binding.metadata.name.is_some());
    }

    /// No credentialsRef means no Secret rule at all — never one with an
    /// empty resourceNames list, which would grant every Secret.
    #[test]
    fn no_credentials_means_no_secret_rule() {
        let class = external_class();
        let role = build_role(&external_inputs(&class));
        assert!(rule_for(&role, "", "secrets").is_empty());
    }

    /// The External Role is scoped to this one Provider by name, which a
    /// name-filtered watch honours, and lets the provider renew only its own
    /// token.
    #[test]
    fn the_external_role_is_scoped_to_its_own_provider_and_token() {
        let class = external_class();
        let i = external_inputs(&class);
        let role = build_role(&i);
        for resource in ["providers", "providers/status"] {
            let rules = rule_for(&role, "banlieue.io", resource);
            assert!(!rules.is_empty(), "{resource}");
            for r in rules {
                assert_eq!(
                    r.resource_names.as_deref(),
                    Some(&["ch-host-1".to_string()][..]),
                    "{resource}"
                );
            }
        }
        let token = rule_for(&role, "", "serviceaccounts/token");
        assert_eq!(token.len(), 1);
        assert_eq!(token[0].verbs, vec!["create".to_string()]);
        assert_eq!(
            token[0].resource_names.as_deref(),
            Some(&["banlieue-provider-cloud-hypervisor-ch-host-1".to_string()][..])
        );
        // The class's own rules are appended, as for Managed.
        assert_eq!(
            rule_for(
                &role,
                "infrastructure.banlieue.io",
                "cloudhypervisormachines"
            )
            .len(),
            1
        );
    }

    /// Nothing runs in a workload namespace for External, so the identity
    /// always lives with the Provider, where its Role is.
    #[test]
    fn external_ignores_workload_namespace() {
        let mut class = external_class();
        class.workload_namespace = Some("elsewhere".into());
        let i = WorkloadInputs {
            workload_namespace: "elsewhere",
            ..external_inputs(&class)
        };
        let sa = build_service_account(&i);
        assert_eq!(sa.metadata.namespace.as_deref(), Some("banlieue-system"));
    }

    #[test]
    fn managed_is_unchanged_it_still_deploys_and_reads_its_credentials() {
        let class = class_spec();
        let set = build_workload(&inputs(&class), "banlieue-imagebuild");
        assert!(set.deployment.is_some());
        assert!(set.import_role.is_some());
        let role = build_role(&inputs(&class));
        assert_eq!(rule_for(&role, "", "secrets").len(), 1);
        assert!(
            rule_for(&role, "", "serviceaccounts/token").is_empty(),
            "Managed pods use projected tokens"
        );
    }

    // ----------------------------------------------------------------------
    // Azure: workload identity federation (ADR-0159 Decision 1)
    // ----------------------------------------------------------------------

    fn azure_class() -> ProviderClassSpec {
        ProviderClassSpec {
            backend: "azure".to_string(),
            ..class_spec()
        }
    }

    /// An azure Provider names no credentialsRef (admission forbids it).
    fn azure_inputs(class: &ProviderClassSpec) -> WorkloadInputs<'_> {
        WorkloadInputs {
            class_name: "azure",
            provider_name: "az-cc",
            credentials_secret: None,
            ..inputs(class)
        }
    }

    fn pod_spec(deployment: &Deployment) -> &k8s_openapi::api::core::v1::PodSpec {
        deployment.spec.template.spec.as_ref().unwrap()
    }

    #[test]
    fn azure_pod_projects_an_entra_audience_token_for_one_hour() {
        let class = azure_class();
        let deployment = build_deployment(&azure_inputs(&class));
        let volumes = pod_spec(&deployment).volumes.clone().expect("volumes");
        assert_eq!(volumes.len(), 1);
        let volume = &volumes[0];
        assert_eq!(volume.name, "azure-identity-token");
        let sources = volume
            .projected
            .as_ref()
            .and_then(|p| p.sources.clone())
            .expect("a projected volume");
        assert_eq!(sources.len(), 1);
        let token = sources[0]
            .service_account_token
            .as_ref()
            .expect("a ServiceAccount token projection");
        assert_eq!(
            token.audience.as_deref(),
            Some("api://AzureADTokenExchange")
        );
        assert_eq!(token.expiration_seconds, Some(3600));
        assert_eq!(token.path, "azure-identity-token");
    }

    #[test]
    fn azure_container_mounts_the_token_read_only_and_names_the_file() {
        let class = azure_class();
        let deployment = build_deployment(&azure_inputs(&class));
        let c = container(&deployment);
        let mounts = c.volume_mounts.clone().expect("volume mounts");
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].name, "azure-identity-token");
        assert_eq!(mounts[0].mount_path, "/var/run/secrets/azure/tokens");
        assert_eq!(mounts[0].read_only, Some(true));
        let env = c.env.clone().expect("env");
        let file = env
            .iter()
            .find(|e| e.name == "AZURE_FEDERATED_TOKEN_FILE")
            .expect("AZURE_FEDERATED_TOKEN_FILE");
        assert_eq!(
            file.value.as_deref(),
            Some("/var/run/secrets/azure/tokens/azure-identity-token")
        );
        assert_eq!(azure_federated_token_file(), file.value.clone().unwrap());
        // The downward API identity is still there, first and unchanged.
        let names: Vec<&str> = env.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "POD_NAME",
                "POD_NAMESPACE",
                "POD_SERVICE_ACCOUNT",
                "AZURE_FEDERATED_TOKEN_FILE"
            ]
        );
    }

    // ---- ADR-0176 Decision 8: the OpenBao assertion source ----

    fn openbao(namespace: Option<&str>) -> banlieue_api::banlieue::AzureOpenBao {
        banlieue_api::banlieue::AzureOpenBao {
            address: "https://openbao.example.com".to_string(),
            namespace: namespace.map(str::to_string),
            auth_mount: "k8s-jwt".to_string(),
            login_role: "azure-az-cc".to_string(),
            token_role: "azure-az-cc".to_string(),
            audience: "openbao".to_string(),
        }
    }

    fn with_openbao<'a>(
        class: &'a ProviderClassSpec,
        bao: &'a banlieue_api::banlieue::AzureOpenBao,
    ) -> WorkloadInputs<'a> {
        WorkloadInputs {
            azure_openbao: Some(bao),
            ..azure_inputs(class)
        }
    }

    fn env_value(c: &k8s_openapi::api::core::v1::Container, name: &str) -> Option<String> {
        c.env
            .as_ref()?
            .iter()
            .find(|e| e.name == name)
            .and_then(|e| e.value.clone())
    }

    #[test]
    fn without_an_openbao_block_nothing_about_openbao_is_rendered() {
        let class = azure_class();
        let deployment = build_deployment(&azure_inputs(&class));
        let c = container(&deployment);
        assert_eq!(pod_spec(&deployment).volumes.as_ref().unwrap().len(), 1);
        assert_eq!(c.volume_mounts.as_ref().unwrap().len(), 1);
        assert!(
            c.env
                .as_ref()
                .unwrap()
                .iter()
                .all(|e| !e.name.starts_with("BANLIEUE_OPENBAO")
                    && e.name != "BANLIEUE_ASSERTION_SOURCE")
        );
    }

    #[test]
    fn an_openbao_block_projects_a_second_token_with_its_own_audience() {
        let class = azure_class();
        let mut bao = openbao(None);
        bao.audience = "bao-login".to_string();
        let deployment = build_deployment(&with_openbao(&class, &bao));
        let volumes = pod_spec(&deployment).volumes.clone().expect("volumes");
        assert_eq!(volumes.len(), 2);
        assert_eq!(
            volumes[0].name, "azure-identity-token",
            "cloud token first, unchanged"
        );
        assert_eq!(volumes[1].name, "openbao-identity-token");
        let token = volumes[1]
            .projected
            .as_ref()
            .unwrap()
            .sources
            .as_ref()
            .unwrap()[0]
            .service_account_token
            .as_ref()
            .expect("a ServiceAccount token projection");
        assert_eq!(token.audience.as_deref(), Some("bao-login"));
        assert_eq!(token.expiration_seconds, Some(3600));
        assert_eq!(token.path, "openbao-identity-token");

        // The two tokens never share an audience: the cloud token stays as it was.
        let cloud = volumes[0]
            .projected
            .as_ref()
            .unwrap()
            .sources
            .as_ref()
            .unwrap()[0]
            .service_account_token
            .as_ref()
            .unwrap();
        assert_eq!(
            cloud.audience.as_deref(),
            Some("api://AzureADTokenExchange")
        );
    }

    #[test]
    fn the_openbao_token_is_mounted_read_only_in_its_own_directory() {
        let class = azure_class();
        let bao = openbao(None);
        let deployment = build_deployment(&with_openbao(&class, &bao));
        let mounts = container(&deployment)
            .volume_mounts
            .clone()
            .expect("mounts");
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[1].name, "openbao-identity-token");
        assert_eq!(mounts[1].mount_path, "/var/run/secrets/openbao");
        assert_eq!(mounts[1].read_only, Some(true));
        assert_ne!(mounts[0].mount_path, mounts[1].mount_path);
    }

    #[test]
    fn the_openbao_settings_are_rendered_from_the_block_and_only_from_it() {
        let class = azure_class();
        let bao = openbao(Some("banlieue"));
        let deployment = build_deployment(&with_openbao(&class, &bao));
        let c = container(&deployment);
        let get = |n: &str| env_value(c, n);
        assert_eq!(get("BANLIEUE_ASSERTION_SOURCE").as_deref(), Some("openbao"));
        assert_eq!(
            get("BANLIEUE_OPENBAO_ADDR").as_deref(),
            Some("https://openbao.example.com")
        );
        assert_eq!(
            get("BANLIEUE_OPENBAO_NAMESPACE").as_deref(),
            Some("banlieue")
        );
        assert_eq!(
            get("BANLIEUE_OPENBAO_AUTH_MOUNT").as_deref(),
            Some("k8s-jwt")
        );
        assert_eq!(
            get("BANLIEUE_OPENBAO_LOGIN_ROLE").as_deref(),
            Some("azure-az-cc")
        );
        assert_eq!(
            get("BANLIEUE_OPENBAO_TOKEN_ROLE").as_deref(),
            Some("azure-az-cc")
        );
        assert_eq!(
            get("BANLIEUE_OPENBAO_SA_TOKEN_FILE").as_deref(),
            Some("/var/run/secrets/openbao/openbao-identity-token")
        );
        // The cloud token file is still named: the same pod can fall back.
        assert!(get("AZURE_FEDERATED_TOKEN_FILE").is_some());
    }

    #[test]
    fn an_unset_namespace_renders_no_namespace_variable() {
        let class = azure_class();
        let bao = openbao(None);
        let deployment = build_deployment(&with_openbao(&class, &bao));
        assert_eq!(
            env_value(container(&deployment), "BANLIEUE_OPENBAO_NAMESPACE"),
            None
        );
    }

    /// OpenBao is an Azure assertion source: another backend ignores the block.
    #[test]
    fn a_backend_other_than_azure_renders_nothing_for_openbao() {
        let class = class_spec();
        let bao = openbao(None);
        let deployment = build_deployment(&WorkloadInputs {
            azure_openbao: Some(&bao),
            ..inputs(&class)
        });
        assert!(pod_spec(&deployment).volumes.is_none());
        assert!(container(&deployment).volume_mounts.is_none());
        assert_eq!(
            env_value(container(&deployment), "BANLIEUE_ASSERTION_SOURCE"),
            None
        );
    }

    /// The login token is a ServiceAccount token, not a Secret: the Role still
    /// grants no Secret access.
    #[test]
    fn an_openbao_block_adds_no_secret_access() {
        let class = azure_class();
        let bao = openbao(None);
        let i = with_openbao(&class, &bao);
        let set = build_workload(&i, "banlieue-imagebuild");
        let rules = set.role.rules.unwrap_or_default();
        assert!(rules.iter().all(|r| {
            !r.resources
                .clone()
                .unwrap_or_default()
                .iter()
                .any(|x| x == "secrets")
        }));
    }

    /// The operator renders names the provider reads; the two must not drift.
    #[test]
    fn the_rendered_variable_names_are_the_ones_the_client_reads() {
        use banlieue_azure::assertion as client;
        assert_eq!(ENV_ASSERTION_SOURCE, "BANLIEUE_ASSERTION_SOURCE");
        assert_eq!(ENV_OPENBAO_ADDR, client::ENV_OPENBAO_ADDR);
        assert_eq!(ENV_OPENBAO_NAMESPACE, client::ENV_OPENBAO_NAMESPACE);
        assert_eq!(ENV_OPENBAO_AUTH_MOUNT, client::ENV_OPENBAO_AUTH_MOUNT);
        assert_eq!(ENV_OPENBAO_LOGIN_ROLE, client::ENV_OPENBAO_LOGIN_ROLE);
        assert_eq!(ENV_OPENBAO_TOKEN_ROLE, client::ENV_OPENBAO_TOKEN_ROLE);
        assert_eq!(ENV_OPENBAO_SA_TOKEN_FILE, client::ENV_OPENBAO_SA_TOKEN_FILE);
    }

    /// The token is the only credential: the Role grants no Secret at all.
    #[test]
    fn azure_role_grants_no_secret_access() {
        let class = azure_class();
        let i = azure_inputs(&class);
        assert!(rule_for(&build_role(&i), "", "secrets").is_empty());
        assert!(rule_for(&build_import_role(&i), "", "secrets").is_empty());
    }

    /// The Entra federated credential's subject is
    /// `system:serviceaccount:<namespace>:<this name>`, so the docs pin it.
    #[test]
    fn azure_pod_runs_as_the_derived_service_account() {
        let class = azure_class();
        let i = azure_inputs(&class);
        let set = build_workload(&i, "banlieue-imagebuild");
        assert_eq!(
            set.service_account.metadata.name.as_deref(),
            Some("banlieue-provider-azure-az-cc")
        );
        assert_eq!(
            set.service_account.metadata.namespace.as_deref(),
            Some("banlieue-system")
        );
        let deployment = set.deployment.expect("azure is Managed");
        assert_eq!(
            pod_spec(&deployment).service_account_name.as_deref(),
            Some("banlieue-provider-azure-az-cc")
        );
    }

    /// Every other backend's pod spec is exactly what it was: no volumes, no
    /// mounts, and only the downward API environment.
    #[test]
    fn other_backends_get_no_token_volume_mount_or_env() {
        for backend in ["vsphere", "libvirt", "proxmox"] {
            let class = ProviderClassSpec {
                backend: backend.to_string(),
                ..class_spec()
            };
            let deployment = build_deployment(&inputs(&class));
            let spec = serde_json::to_value(pod_spec(&deployment)).unwrap();
            assert!(spec.get("volumes").is_none(), "{backend}: {spec}");
            let c = &spec["containers"][0];
            assert!(c.get("volumeMounts").is_none(), "{backend}: {c}");
            assert_eq!(
                c["env"],
                serde_json::to_value(downward_api_env(DOWNWARD_API_ENV)).unwrap(),
                "{backend}"
            );
        }
    }

    // ----------------------------------------------------------------------
    // Job Role in the build namespace (least privilege for Job create)
    // ----------------------------------------------------------------------

    fn class_for(backend: &str) -> ProviderClassSpec {
        ProviderClassSpec {
            backend: backend.to_string(),
            ..class_spec()
        }
    }

    fn verbs_on(role: &Role, group: &str, resource: &str) -> Vec<String> {
        role.rules
            .as_ref()
            .unwrap()
            .iter()
            .filter(|r| {
                r.api_groups.as_deref() == Some(&[group.to_string()])
                    && r.resources.as_deref() == Some(&[resource.to_string()])
            })
            .flat_map(|r| r.verbs.clone())
            .collect()
    }

    /// Each backend that runs Jobs gets exactly the verbs its own Job code
    /// uses, in a Role in the build namespace.
    #[test]
    fn a_job_running_backend_gets_its_job_verbs_in_the_build_namespace() {
        let expected: [(&str, &[&str], &[&str]); 3] = [
            ("libvirt", &["get", "create", "patch"], &[]),
            (
                "vsphere",
                &["get", "list", "watch", "create", "patch", "delete"],
                &[],
            ),
            (
                "azure",
                &["get", "list", "watch", "create", "patch", "delete"],
                &["list"],
            ),
        ];
        for (backend, jobs, pods) in expected {
            let class = class_for(backend);
            let set = build_workload(&inputs(&class), "banlieue-imagebuild");
            let role = set
                .jobs_role
                .unwrap_or_else(|| panic!("{backend}: no Job Role"));
            assert_eq!(
                role.metadata.namespace.as_deref(),
                Some("banlieue-imagebuild"),
                "{backend}"
            );
            assert_eq!(verbs_on(&role, "batch", "jobs"), jobs, "{backend}");
            assert_eq!(verbs_on(&role, "", "pods"), pods, "{backend}");
            assert_eq!(
                role.rules.as_ref().unwrap().len(),
                1 + usize::from(!pods.is_empty()),
                "{backend}: nothing but jobs (and azure's pods list)"
            );
            for rule in role.rules.as_ref().unwrap() {
                assert!(
                    !rule.verbs.iter().any(|v| v == "update" || v == "*"),
                    "{backend}: {rule:?}"
                );
            }
        }
    }

    /// The binding lives with the Role in the build namespace and names the
    /// provider's own ServiceAccount where it lives, never the import one.
    #[test]
    fn the_job_role_binding_names_the_provider_account_across_namespaces() {
        let class = class_for("vsphere");
        let mut i = inputs(&class);
        i.provider_namespace = "tenant-a";
        i.workload_namespace = "banlieue-system";
        i.owner = Some(owner_reference("prod-vc", "uid-1"));
        let set = build_workload(&i, "banlieue-imagebuild");
        let role = set.jobs_role.unwrap();
        let binding = set.jobs_role_binding.unwrap();
        assert_eq!(
            binding.metadata.namespace.as_deref(),
            Some("banlieue-imagebuild")
        );
        assert_eq!(binding.role_ref.kind, "Role");
        assert_eq!(Some(binding.role_ref.name.clone()), role.metadata.name);
        let subject = &binding.subjects.as_ref().unwrap()[0];
        assert_eq!(subject.name, workload_name("vsphere", "prod-vc"));
        assert_eq!(subject.namespace.as_deref(), Some("banlieue-system"));
        assert_ne!(subject.name, IMPORT_SERVICE_ACCOUNT);
        // Unowned: the owner would be in another namespace. The labels pin
        // the Provider, which is how the finalizer finds and removes them.
        assert!(role.metadata.owner_references.is_none());
        assert!(binding.metadata.owner_references.is_none());
        let labels = role.metadata.labels.as_ref().unwrap();
        assert_eq!(labels["banlieue.io/provider"], "prod-vc");
        assert_eq!(labels["banlieue.io/provider-namespace"], "tenant-a");
        assert_eq!(binding.metadata.labels, role.metadata.labels);
    }

    /// Every Provider's Job Role shares one build namespace, so the name
    /// carries the Provider's namespace or two would collide.
    #[test]
    fn job_role_names_do_not_collide_across_provider_namespaces() {
        let class = class_for("libvirt");
        let mut a = inputs(&class);
        a.provider_namespace = "tenant-a";
        let mut b = inputs(&class);
        b.provider_namespace = "tenant-b";
        let name_a = build_jobs_role(&a, "build").unwrap().metadata.name;
        let name_b = build_jobs_role(&b, "build").unwrap().metadata.name;
        assert_ne!(name_a, name_b);
        assert_eq!(
            name_a.as_deref(),
            Some(jobs_role_name("vsphere", "tenant-a", "prod-vc").as_str())
        );
        assert_ne!(
            name_a,
            build_role(&a).metadata.name,
            "distinct from the credentials Role"
        );
    }

    /// Backends that run no Job get no Job Role; neither does an External
    /// provider, which runs nothing in the cluster.
    #[test]
    fn backends_without_jobs_and_external_providers_get_no_job_role() {
        for backend in ["proxmox", "cloud-hypervisor"] {
            assert!(job_rules(backend).is_none(), "{backend}");
            let class = class_for(backend);
            let set = build_workload(&inputs(&class), "banlieue-imagebuild");
            assert!(
                set.jobs_role.is_none() && set.jobs_role_binding.is_none(),
                "{backend}"
            );
        }
        let class = ProviderClassSpec {
            backend: "libvirt".to_string(),
            deployment: Some(banlieue_api::banlieue::ProviderDeployment::External),
            image: None,
            ..class_spec()
        };
        let set = build_workload(&inputs(&class), "banlieue-imagebuild");
        assert!(set.jobs_role.is_none() && set.jobs_role_binding.is_none());
    }

    /// The per-instance Role in the Provider's namespace never carries Jobs:
    /// that namespace holds the operator's ServiceAccount.
    #[test]
    fn the_per_instance_role_grants_no_jobs() {
        for backend in ["vsphere", "libvirt", "azure"] {
            let class = class_for(backend);
            let role = build_role(&inputs(&class));
            assert!(verbs_on(&role, "batch", "jobs").is_empty(), "{backend}");
            assert!(verbs_on(&role, "", "pods").is_empty(), "{backend}");
        }
    }
}
