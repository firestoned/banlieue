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

    fn inputs<'a>(class: &'a ProviderClassSpec) -> WorkloadInputs<'a> {
        WorkloadInputs {
            class_name: "vsphere",
            class,
            provider_name: "prod-vc",
            provider_namespace: "banlieue-system",
            workload_namespace: "banlieue-system",
            credentials_secret: "prod-vc-creds",
            build_toleration: &[],
            ca_bundle_config_map: None,
            ca_bundle_secret: None,
            owner: None,
        }
    }

    fn container(deployment: &Deployment) -> &k8s_openapi::api::core::v1::Container {
        &deployment
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0]
    }

    // ----------------------------------------------------------------------
    // Deployment
    // ----------------------------------------------------------------------

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
        class.image.pull_policy = Some(ImagePullPolicy::Always);
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
        assert_eq!(deployment.spec.as_ref().unwrap().replicas, Some(1));
    }

    #[test]
    fn deployment_honours_an_explicit_replica_count() {
        let mut class = class_spec();
        class.replicas = Some(2);
        let deployment = build_deployment(&inputs(&class));
        assert_eq!(deployment.spec.as_ref().unwrap().replicas, Some(2));
    }

    #[test]
    fn deployment_binds_the_per_instance_service_account() {
        let class = class_spec();
        let deployment = build_deployment(&inputs(&class));
        let pod_spec = deployment
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap();
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
        let env = deployment
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0]
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
        let spec = deployment.spec.as_ref().unwrap();
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
        let pod_spec = deployment
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap();
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
            ("Deployment", &set.deployment.metadata),
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
            set.deployment.metadata.owner_references.is_none(),
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
        assert_eq!(set.deployment.metadata.name.as_deref(), Some(expected));
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
            set.deployment.metadata.labels.as_ref(),
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
        let want = class.image.reference();

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
}
