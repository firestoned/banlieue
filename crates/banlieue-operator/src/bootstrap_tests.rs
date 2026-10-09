// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `bootstrap.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    /// Every backend with a shipped `deploy/provider-<backend>/rbac/` manifest.
    /// Add a backend here when its ClusterRole lands, so the RBAC-coverage
    /// guards below start policing it too.
    const BACKENDS_WITH_ROLES: [&str; 4] = ["vsphere", "libvirt", "proxmox", "cloud-hypervisor"];

    fn opts() -> InstallOptions {
        InstallOptions {
            namespace: DEFAULT_NAMESPACE.to_string(),
            version: "v0.1.0".to_string(),
            registry: None,
            build_node_selector: Vec::new(),
            build_toleration: Vec::new(),
            image_digest: None,
        }
    }

    // ----------------------------------------------------------------------
    // Image resolution
    // ----------------------------------------------------------------------

    #[test]
    fn image_defaults_to_the_public_registry() {
        assert_eq!(resolve_image(&opts()), "ghcr.io/firestoned/banlieue:v0.1.0");
    }

    /// Air-gapped installs mirror the image to a private registry; the tag is
    /// preserved so the mirrored artifact is identifiable.
    #[test]
    fn registry_override_repoints_the_image() {
        let opts = InstallOptions {
            registry: Some("registry.internal:5000".to_string()),
            ..opts()
        };
        assert_eq!(
            resolve_image(&opts),
            "registry.internal:5000/banlieue:v0.1.0"
        );
    }

    /// Bootstrap must install the image matching the binary that ran it, or the
    /// CRDs it applies can disagree with the controller that consumes them.
    #[test]
    fn default_image_tag_tracks_the_binary_version() {
        assert_eq!(DEFAULT_IMAGE_TAG, concat!("v", env!("CARGO_PKG_VERSION")));
    }

    // ----------------------------------------------------------------------
    // Roles
    // ----------------------------------------------------------------------

    #[test]
    fn each_role_names_its_workload_and_subcommand() {
        assert_eq!(InstallRole::Controller.name(), "banlieue-controller");
        assert_eq!(InstallRole::Controller.args(), vec!["controller"]);

        assert_eq!(InstallRole::Operator.name(), "banlieue-operator");
        assert_eq!(InstallRole::Operator.args(), vec!["operator"]);

        assert_eq!(InstallRole::Imagebuilder.name(), "banlieue-imagebuilder");
        assert_eq!(InstallRole::Imagebuilder.args(), vec!["imagebuilder"]);

        let vsphere = InstallRole::Provider("vsphere".to_string());
        assert_eq!(vsphere.name(), "banlieue-provider-vsphere");
        assert_eq!(vsphere.args(), vec!["provider", "vsphere"]);
    }

    /// A statically installed provider carries no `--provider-name`: it serves
    /// every Provider of its class in its watch scope, which is the escape
    /// hatch's whole purpose. (The watch scope itself — `--namespace` — is
    /// appended in `build_container`, asserted below.)
    #[test]
    fn a_statically_installed_provider_is_not_scoped_to_one_provider() {
        let args = InstallRole::Provider("vsphere".to_string()).args();
        assert!(!args.iter().any(|a| a == "--provider-name"), "{args:?}");
    }

    #[test]
    fn every_role_ships_a_cluster_role_that_parses() {
        let roles = [
            InstallRole::Controller,
            InstallRole::Operator,
            InstallRole::Imagebuilder,
        ]
        .into_iter()
        .chain(
            BACKENDS_WITH_ROLES
                .iter()
                .map(|b| InstallRole::Provider((*b).to_string())),
        );
        for role in roles {
            let cluster_role = role.cluster_role().expect("embedded ClusterRole parses");
            assert_eq!(
                cluster_role.metadata.name.as_deref(),
                Some(role.name()),
                "ClusterRole name must match the role it serves"
            );
            assert!(
                cluster_role.rules.as_ref().is_some_and(|r| !r.is_empty()),
                "{} ClusterRole has no rules",
                role.name()
            );
        }
    }

    /// Flatten a ClusterRole into the set of (apiGroup, resource, verb) triples
    /// it grants.
    fn granted_triples(role: &InstallRole) -> std::collections::BTreeSet<(String, String, String)> {
        role.cluster_role()
            .unwrap()
            .rules
            .unwrap_or_default()
            .into_iter()
            .flat_map(|r| {
                let groups = r.api_groups.unwrap_or_default();
                let resources = r.resources.unwrap_or_default();
                let verbs = r.verbs;
                let mut out = Vec::new();
                for g in &groups {
                    for res in &resources {
                        for v in &verbs {
                            out.push((g.clone(), res.clone(), v.clone()));
                        }
                    }
                }
                out
            })
            .collect()
    }

    /// The operator binds each provider's shared ClusterRole to a per-instance
    /// ServiceAccount, and RBAC refuses to let a grantor hand out permissions it
    /// does not itself hold. Any triple the provider role grants must therefore
    /// also appear in the operator's (ADR-0012).
    ///
    /// Compares **verbs**, not just apiGroups: an earlier group-only version of
    /// this test passed while the provider role granted `secrets: list,watch`
    /// and the operator held only `get`, so every ClusterRoleBinding was
    /// rejected at runtime. Caught by the kind e2e; guarded properly here now.
    #[test]
    fn operator_cluster_role_covers_every_permission_it_grants_to_providers() {
        let operator = granted_triples(&InstallRole::Operator);

        for backend in BACKENDS_WITH_ROLES {
            for (group, resource, verb) in
                granted_triples(&InstallRole::Provider(backend.to_string()))
            {
                assert!(
                    operator.contains(&(group.clone(), resource.clone(), verb.clone())),
                    "operator ClusterRole cannot grant {verb} on {resource:?} (apiGroup \
                     {group:?}) to the {backend} provider because it does not hold it — RBAC \
                     will reject the ClusterRoleBinding. Either add it to \
                     deploy/operator/rbac/clusterrole.yaml, or (better) drop it from the \
                     provider role if it is not actually needed."
                );
            }
        }
    }

    /// A ClusterRoleBinding grants cluster-wide, so `list`/`watch` on Secrets in
    /// a provider's shared ClusterRole would let any provider pod read every
    /// Secret in the cluster — defeating the `resourceNames` narrowing that
    /// per-instance topology exists to provide (ADR-0003). The provider reads
    /// its credentials by name, so `get` is sufficient.
    #[test]
    fn no_provider_cluster_role_grants_blanket_secret_enumeration() {
        for backend in BACKENDS_WITH_ROLES {
            for (group, resource, verb) in
                granted_triples(&InstallRole::Provider(backend.to_string()))
            {
                if group.is_empty() && (resource == "secrets" || resource == "configmaps") {
                    assert!(
                        verb != "list" && verb != "watch",
                        "the {backend} provider ClusterRole grants {verb} on {resource} \
                         cluster-wide, which reads every {resource} in the cluster; read by \
                         name with `get` instead"
                    );
                }
            }
        }
    }

    /// **TM-01 regression guard (ADR-0041).** A `ClusterRole` reached through a
    /// `ClusterRoleBinding` has no namespace scope, so ANY Secret rule in one
    /// reaches every Secret in the cluster. `banlieue-imagebuilder` shipped
    /// exactly that for a month — `secrets: get,list,watch,create,patch`, no
    /// `resourceNames` — under a comment claiming it was "scoped to the
    /// imagebuild namespace". It was not, and a comment cannot make it so.
    ///
    /// The single legitimate exception is `banlieue-operator`, which must hold
    /// `secrets: get` solely so RBAC's escalation-prevention permits it to
    /// delegate that verb into a per-Provider, `resourceNames`-scoped Role. It
    /// never exercises the grant itself (SEC-007, accepted-with-monitoring).
    ///
    /// Every other component reads Secrets through a namespaced Role, or not at
    /// all. If this test fails, do not add an exception: move the grant to a
    /// `Role`.
    #[test]
    fn only_the_operator_cluster_role_may_grant_secret_access() {
        let mut roles = vec![InstallRole::Controller, InstallRole::Imagebuilder];
        roles.extend(
            BACKENDS_WITH_ROLES
                .iter()
                .map(|b| InstallRole::Provider((*b).to_string())),
        );

        for role in roles {
            for (group, resource, verb) in granted_triples(&role) {
                assert!(
                    !(group.is_empty() && (resource == "secrets" || resource == "configmaps")),
                    "the {} ClusterRole grants {verb} on {resource} cluster-wide. A ClusterRole \
                     + ClusterRoleBinding has NO namespace scope, so this reaches every \
                     {resource} in every namespace — including every Provider's hypervisor \
                     credentials. Move it to a namespaced Role (ADR-0041), or scope it with \
                     resourceNames in an operator-minted Role.",
                    role.name()
                );
            }
        }
    }

    /// The reconciler holds a finalizer on every Provider, which writes
    /// `metadata.finalizers` on the **main** resource. That needs
    /// update/patch on `providers` — NOT the `providers/finalizers`
    /// subresource, which is the separate admission permission for
    /// `blockOwnerDeletion` on owner references.
    ///
    /// Conflating the two makes every reconcile 403 at `ensure_finalizer` and
    /// no workload is ever created. Unit tests cannot see that; the kind e2e
    /// caught it (ADR-0014). This guards the regression cheaply.
    #[test]
    fn operator_cluster_role_can_write_the_provider_finalizer() {
        let rules = InstallRole::Operator
            .cluster_role()
            .unwrap()
            .rules
            .unwrap_or_default();

        let can_patch_providers = rules.iter().any(|r| {
            r.api_groups
                .as_ref()
                .is_some_and(|g| g.contains(&"banlieue.io".to_string()))
                && r.resources
                    .as_ref()
                    .is_some_and(|res| res.contains(&"providers".to_string()))
                && (r.verbs.contains(&"patch".to_string())
                    || r.verbs.contains(&"update".to_string()))
        });

        assert!(
            can_patch_providers,
            "operator must hold update/patch on `providers` to add its finalizer; \
             `providers/finalizers` is a different permission and does not cover it"
        );
    }

    /// `blockOwnerDeletion: true` on a dependent requires `update` on the
    /// owner's `/finalizers` subresource. Every namespaced object the operator
    /// creates sets that flag, so this rule must survive too.
    #[test]
    fn operator_cluster_role_can_set_block_owner_deletion() {
        let rules = InstallRole::Operator
            .cluster_role()
            .unwrap()
            .rules
            .unwrap_or_default();

        assert!(
            rules.iter().any(|r| {
                r.resources
                    .as_ref()
                    .is_some_and(|res| res.contains(&"providers/finalizers".to_string()))
                    && r.verbs.contains(&"update".to_string())
            }),
            "owner references with blockOwnerDeletion need update on providers/finalizers"
        );
    }

    /// The ProviderClass reconciler reads the shared per-backend ClusterRole to
    /// decide whether a class is usable. Without `get`, every class reconcile
    /// 403s — the same failure mode as bug-104, caught here instead.
    #[test]
    fn operator_cluster_role_can_read_cluster_roles() {
        let granted = granted_triples(&InstallRole::Operator);
        assert!(
            granted.contains(&(
                "rbac.authorization.k8s.io".to_string(),
                "clusterroles".to_string(),
                "get".to_string()
            )),
            "operator must be able to read ClusterRoles to assess ProviderClass readiness"
        );
    }

    /// …but must never be able to create or modify one. Minting the permissions
    /// it hands out is precisely the escalation path ADR-0012 refuses.
    #[test]
    fn operator_cannot_write_cluster_roles() {
        let granted = granted_triples(&InstallRole::Operator);
        for verb in ["create", "update", "patch", "delete"] {
            assert!(
                !granted.contains(&(
                    "rbac.authorization.k8s.io".to_string(),
                    "clusterroles".to_string(),
                    verb.to_string()
                )),
                "operator must not hold {verb} on clusterroles — bootstrap installs them"
            );
        }
    }

    // ----------------------------------------------------------------------
    // Manifests
    // ----------------------------------------------------------------------

    #[test]
    fn operator_install_includes_every_crd() {
        let manifests = build_operator_install(&opts(), &["vsphere"], false).unwrap();
        let kinds: Vec<_> = manifests
            .crds
            .iter()
            .map(|c| c.spec.names.kind.clone())
            .collect();

        for expected in [
            "Provider",
            "ProviderClass",
            "VMClass",
            "VMImage",
            "VirtualMachine",
        ] {
            assert!(
                kinds.contains(&expected.to_string()),
                "missing CRD {expected}"
            );
        }
    }

    #[test]
    fn operator_install_deploys_both_the_controller_and_the_operator() {
        let manifests = build_operator_install(&opts(), &["vsphere"], false).unwrap();
        let names: Vec<_> = manifests
            .deployments
            .iter()
            .filter_map(|d| d.metadata.name.clone())
            .collect();
        assert!(names.contains(&"banlieue-controller".to_string()));
        assert!(names.contains(&"banlieue-operator".to_string()));
    }

    /// The normal path leaves a ProviderClass per compiled-in backend, so
    /// registering a backend afterwards is just `kubectl apply` of a Provider.
    #[test]
    fn operator_install_seeds_a_provider_class_per_compiled_backend() {
        let manifests = build_operator_install(&opts(), &BACKENDS_WITH_ROLES, false).unwrap();
        let names: Vec<_> = manifests
            .provider_classes
            .iter()
            .filter_map(|c| c.metadata.name.clone())
            .collect();
        assert_eq!(
            names,
            vec![
                "vsphere".to_string(),
                "libvirt".to_string(),
                "proxmox".to_string(),
                "cloud-hypervisor".to_string()
            ]
        );

        let vsphere = &manifests.provider_classes[0];
        assert_eq!(vsphere.spec.backend, "vsphere");
        assert_eq!(
            vsphere.spec.image.as_ref().expect("image").reference(),
            "ghcr.io/firestoned/banlieue:v0.1.0"
        );
    }

    /// The operator binds the shared per-backend ClusterRole but cannot create
    /// it (that would be the escalation path ADR-0012 refuses). If bootstrap
    /// does not install it, every ClusterRoleBinding the operator writes points
    /// at a nonexistent role and the provider pod runs with no permissions —
    /// which looks like a healthy install right up until nothing works.
    #[test]
    fn operator_install_ships_the_shared_cluster_role_each_backend_needs() {
        let manifests = build_operator_install(&opts(), &["vsphere"], false).unwrap();
        let names: Vec<_> = manifests
            .cluster_roles
            .iter()
            .filter_map(|c| c.metadata.name.clone())
            .collect();

        assert!(
            names.contains(&"banlieue-provider-vsphere".to_string()),
            "bootstrap must install the ClusterRole the operator will bind, got {names:?}"
        );
    }

    /// …and the name it installs must be exactly the one the workload builder
    /// puts in the ClusterRoleBinding's roleRef, or they silently miss.
    #[test]
    fn the_installed_cluster_role_name_matches_what_the_binding_references() {
        let manifests = build_operator_install(&opts(), &["vsphere"], false).unwrap();
        let installed: Vec<_> = manifests
            .cluster_roles
            .iter()
            .filter_map(|c| c.metadata.name.clone())
            .collect();

        let referenced = crate::workload::shared_cluster_role_name("vsphere");
        assert!(
            installed.contains(&referenced),
            "workload binds roleRef {referenced:?} but bootstrap installs {installed:?}"
        );
    }

    /// A compiled-in backend with no ClusterRole manifest must fail the install
    /// rather than produce one where that backend can never work.
    #[test]
    fn a_compiled_backend_without_a_cluster_role_fails_the_operator_install() {
        let result = build_operator_install(&opts(), &["vsphere", "hyperv"], false);
        let err = result.expect_err("a backend with no ClusterRole must fail the install");
        assert!(
            format!("{err:#}").contains("hyperv"),
            "error should name the offending backend, got: {err:#}"
        );
    }

    #[test]
    fn provider_classes_can_be_skipped() {
        let manifests = build_operator_install(&opts(), &["vsphere"], true).unwrap();
        assert!(manifests.provider_classes.is_empty());
    }

    #[test]
    fn operator_install_honours_the_namespace() {
        let opts = InstallOptions {
            namespace: "banlieue-prod".to_string(),
            ..opts()
        };
        let manifests = build_operator_install(&opts, &["vsphere"], false).unwrap();

        assert_eq!(
            manifests.namespace.metadata.name.as_deref(),
            Some("banlieue-prod")
        );
        for deployment in &manifests.deployments {
            assert_eq!(
                deployment.metadata.namespace.as_deref(),
                Some("banlieue-prod")
            );
        }
        for binding in &manifests.cluster_role_bindings {
            assert_eq!(
                binding.subjects.as_ref().unwrap()[0].namespace.as_deref(),
                Some("banlieue-prod"),
                "ClusterRoleBinding subject must follow the namespace"
            );
        }
    }

    /// CRDs are cluster-scoped; giving them a namespace is a silent no-op that
    /// misleads anyone reading `--dry-run` output.
    #[test]
    fn crds_are_not_given_a_namespace() {
        let manifests = build_operator_install(&opts(), &["vsphere"], false).unwrap();
        for crd in &manifests.crds {
            assert!(crd.metadata.namespace.is_none());
        }
    }

    // ----------------------------------------------------------------------
    // Single-role installs
    // ----------------------------------------------------------------------

    #[test]
    fn a_provider_install_ships_only_that_backends_workload() {
        let manifests =
            build_role_install(&InstallRole::Provider("vsphere".to_string()), &opts()).unwrap();

        assert_eq!(manifests.deployments.len(), 1);
        assert_eq!(
            manifests.deployments[0].metadata.name.as_deref(),
            Some("banlieue-provider-vsphere")
        );
        assert!(
            manifests.crds.is_empty(),
            "a role install must not re-apply CRDs"
        );
        assert!(manifests.provider_classes.is_empty());
    }

    /// Security review 2026-07-31: with the shared ClusterRole stripped of cluster-wide Secret
    /// access, a standalone provider reads credentials and CA bundles through
    /// a namespaced Role in the install namespace — and its watch is scoped
    /// there too, so out-of-namespace Providers are never even reconciled.
    #[test]
    fn a_standalone_provider_install_scopes_credentials_to_the_install_namespace() {
        let manifests =
            build_role_install(&InstallRole::Provider("vsphere".to_string()), &opts()).unwrap();

        assert_eq!(manifests.roles.len(), 1);
        let role = &manifests.roles[0];
        assert_eq!(role.metadata.namespace.as_deref(), Some(DEFAULT_NAMESPACE));
        let rules = role.rules.as_ref().expect("the Role has rules");
        let granted: Vec<&str> = rules
            .iter()
            .flat_map(|r| r.resources.as_ref().unwrap().iter().map(String::as_str))
            .collect();
        assert_eq!(granted, ["secrets", "configmaps"], "{rules:?}");
        for rule in rules {
            assert_eq!(rule.verbs, ["get"], "{rule:?}");
            assert!(
                rule.resource_names.is_none(),
                "a standalone provider serves every Provider in the namespace, \
                 so it cannot name their Secrets up front: {rule:?}"
            );
        }

        assert_eq!(manifests.role_bindings.len(), 1);
        let binding = &manifests.role_bindings[0];
        assert_eq!(binding.role_ref.kind, "Role");
        assert_eq!(binding.role_ref.name, "banlieue-provider-vsphere");
        assert_eq!(
            binding.metadata.namespace.as_deref(),
            Some(DEFAULT_NAMESPACE)
        );

        let container = &manifests.deployments[0]
            .spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0];
        let args = container.args.as_ref().unwrap();
        let ns_flag = args
            .iter()
            .position(|a| a == "--namespace")
            .expect("the standalone provider's watch is scoped, got {args:?}");
        assert_eq!(args[ns_flag + 1], DEFAULT_NAMESPACE);
    }

    /// Only standalone providers get the namespaced credential Role — other
    /// roles keep their existing permission shape.
    #[test]
    fn an_imagebuilder_install_ships_namespaced_cloud_config_rbac() {
        let manifests = build_role_install(&InstallRole::Imagebuilder, &opts()).unwrap();

        assert_eq!(
            manifests.roles.len(),
            1,
            "the imagebuilder reads cloudConfigs Secrets through a namespaced Role (ADR-0041)"
        );
        assert_eq!(manifests.role_bindings.len(), 1);

        let role = &manifests.roles[0];
        assert_eq!(
            role.metadata.namespace.as_deref(),
            Some(DEFAULT_IMAGEBUILD_NAMESPACE),
            "the Role must live where the Secrets do — the build namespace, not the install \
             namespace"
        );
        let rule = &role.rules.as_ref().unwrap()[0];
        assert_eq!(
            rule.resources.as_deref(),
            Some(&["secrets".to_string()][..])
        );
        // Read the referenced cloud-configs, then server-side-apply the merged
        // Secret: apply is a CREATE when absent and a PATCH when present.
        for verb in ["get", "list", "watch", "create", "patch"] {
            assert!(
                rule.verbs.iter().any(|v| v == verb),
                "cloud-config merge needs {verb} on secrets"
            );
        }
        assert!(
            !rule.verbs.iter().any(|v| v == "delete"),
            "nothing in the merge path deletes a Secret"
        );
    }

    /// The Role lives with the objects it grants (the build namespace); the
    /// subject lives with the workload (the install namespace). That asymmetry
    /// is how cross-namespace RBAC is expressed, and getting it backwards
    /// produces a binding that silently grants nothing.
    #[test]
    fn the_imagebuilder_role_binding_crosses_namespaces_in_the_right_direction() {
        let manifests = build_role_install(&InstallRole::Imagebuilder, &opts()).unwrap();
        let binding = &manifests.role_bindings[0];

        assert_eq!(
            binding.metadata.namespace.as_deref(),
            Some(DEFAULT_IMAGEBUILD_NAMESPACE),
            "a RoleBinding must live in the same namespace as the Role it references"
        );
        assert_eq!(binding.role_ref.kind, "Role");
        assert_eq!(
            binding.role_ref.name,
            manifests.roles[0].metadata.name.clone().unwrap()
        );

        let subject = &binding.subjects.as_ref().unwrap()[0];
        assert_eq!(subject.name, "banlieue-imagebuilder");
        assert_eq!(
            subject.namespace.as_deref(),
            Some(DEFAULT_NAMESPACE),
            "the imagebuilder pod runs in the install namespace, not the build namespace"
        );
    }

    /// The registry push (ADR-0064) runs as a Job beside the artifacts PVC:
    /// the imagebuilder creates, reads and replaces it, and reads its pod's
    /// termination message — in the build namespace only, never a Secret
    /// verb beyond the cloud-config ones.
    #[test]
    fn the_imagebuilder_build_namespace_role_covers_the_push_job() {
        let manifests = build_role_install(&InstallRole::Imagebuilder, &opts()).unwrap();
        let rules = manifests.roles[0].rules.as_ref().unwrap();
        let verbs = |group: &str, resource: &str| -> Vec<String> {
            rules
                .iter()
                .filter(|r| {
                    r.api_groups.as_deref() == Some(&[group.to_string()][..])
                        && r.resources
                            .as_deref()
                            .is_some_and(|rs| rs.contains(&resource.to_string()))
                })
                .flat_map(|r| r.verbs.clone())
                .collect()
        };
        let jobs = verbs("batch", "jobs");
        for v in ["get", "create", "patch", "delete"] {
            assert!(jobs.contains(&v.to_string()), "jobs {v}: {jobs:?}");
        }
        assert_eq!(verbs("", "pods"), vec!["list".to_string()]);
        assert_eq!(
            manifests.roles[0].metadata.namespace.as_deref(),
            Some(DEFAULT_IMAGEBUILD_NAMESPACE)
        );
    }

    #[test]
    fn a_role_install_binds_its_service_account_to_its_cluster_role() {
        let manifests = build_role_install(&InstallRole::Imagebuilder, &opts()).unwrap();
        let binding = &manifests.cluster_role_bindings[0];

        assert_eq!(binding.role_ref.name, "banlieue-imagebuilder");
        let subject = &binding.subjects.as_ref().unwrap()[0];
        assert_eq!(subject.name, "banlieue-imagebuilder");
        assert_eq!(subject.namespace.as_deref(), Some(DEFAULT_NAMESPACE));
    }

    /// A backend with no `deploy/provider-<backend>/rbac/clusterrole.yaml` must
    /// fail the install loudly. Silently omitting the ClusterRole would produce
    /// a workload whose ServiceAccount is bound to a role that does not exist —
    /// a pod with no permissions, failing later with opaque 403s instead of now
    /// with a clear message.
    #[test]
    fn a_backend_without_a_cluster_role_fails_the_install() {
        let result = build_role_install(&InstallRole::Provider("hyperv".to_string()), &opts());
        let err = result.expect_err("a backend with no embedded ClusterRole must error");
        assert!(
            format!("{err:#}").contains("ClusterRole"),
            "error should name the missing ClusterRole, got: {err:#}"
        );
    }

    /// A standalone provider install must ship the namespaced Role that carries
    /// its Secret access (CHAIN-002) — the shared ClusterRole deliberately has
    /// none, because it is bound cluster-wide.
    #[test]
    fn a_provider_install_ships_its_namespaced_rbac() {
        let manifests =
            build_role_install(&InstallRole::Provider("vsphere".to_string()), &opts()).unwrap();

        assert_eq!(manifests.roles.len(), 1, "expected one namespaced Role");
        assert_eq!(manifests.role_bindings.len(), 1);

        let role = &manifests.roles[0];
        assert_eq!(role.metadata.namespace.as_deref(), Some(DEFAULT_NAMESPACE));

        let grants_secret_get = role.rules.as_ref().unwrap().iter().any(|r| {
            r.resources
                .as_ref()
                .is_some_and(|res| res.contains(&"secrets".to_string()))
                && r.verbs.contains(&"get".to_string())
        });
        assert!(
            grants_secret_get,
            "the namespaced Role is where a standalone provider's Secret access lives"
        );
    }

    /// `to_yaml` and `apply` are two traversals of the same struct and drifted
    /// once: the namespaced Role was emitted into `--dry-run` output but never
    /// applied, so GitOps installs worked while direct installs produced a
    /// provider with no Secret access. This pins the YAML half; the kind e2e
    /// (`kind-e2e-escape-hatch`) pins the apply half, which is what caught it.
    #[test]
    fn dry_run_output_includes_the_namespaced_rbac() {
        let manifests =
            build_role_install(&InstallRole::Provider("vsphere".to_string()), &opts()).unwrap();
        let yaml = manifests.to_yaml().unwrap();

        assert!(
            yaml.contains("kind: Role\n"),
            "Role missing from --dry-run output"
        );
        assert!(
            yaml.contains("kind: RoleBinding"),
            "RoleBinding missing from --dry-run output"
        );
    }

    // ----------------------------------------------------------------------
    // Dry run
    // ----------------------------------------------------------------------

    /// `--dry-run` must emit a stream `kubectl apply -f -` accepts, and must be
    /// usable with no cluster and no kubeconfig.
    #[test]
    fn dry_run_emits_a_multi_document_yaml_stream() {
        let manifests = build_operator_install(&opts(), &["vsphere"], false).unwrap();
        let yaml = manifests.to_yaml().expect("manifests serialize");

        assert!(yaml.starts_with("---"), "stream must open with a separator");
        assert!(yaml.contains("kind: CustomResourceDefinition"));
        assert!(yaml.contains("kind: Deployment"));
        assert!(yaml.contains("kind: ProviderClass"));
    }

    /// Applying in the wrong order fails: a CRD must exist before a CR of that
    /// kind, and RBAC must exist before the pod that uses it.
    #[test]
    fn dry_run_orders_crds_before_the_custom_resources_that_need_them() {
        let manifests = build_operator_install(&opts(), &["vsphere"], false).unwrap();
        let yaml = manifests.to_yaml().unwrap();

        let crd_at = yaml.find("kind: CustomResourceDefinition").unwrap();
        let deployment_at = yaml.find("kind: Deployment").unwrap();
        let provider_class_at = yaml.find("kind: ProviderClass").unwrap();
        let namespace_at = yaml.find("kind: Namespace").unwrap();

        assert!(namespace_at < crd_at, "Namespace first");
        assert!(crd_at < deployment_at, "CRDs before workloads");
        assert!(
            crd_at < provider_class_at,
            "the ProviderClass CRD must exist before a ProviderClass is applied"
        );
    }

    // ---------- per-backend additional Role rules ------------------------

    #[test]
    fn only_libvirt_gets_the_job_grant() {
        // Creating a Job is the ability to run an arbitrary pod as the
        // provider's own ServiceAccount. libvirt needs it for image import
        // (ADR-0011) in the Provider's OWN namespace, which this per-instance
        // Role is scoped to. vSphere's own per-zone import Jobs (ADR-0020)
        // live in a different namespace (`banlieue-imagebuild`) — a rule
        // here could never reach them — so its Jobs access comes entirely
        // from the cluster-wide ClusterRole instead; empty here does NOT
        // mean vSphere has no Jobs access, see `backend_additional_rules`'s
        // doc comment.
        assert!(backend_additional_rules("vsphere").is_empty());
        assert!(backend_additional_rules("proxmox").is_empty());

        let rules = backend_additional_rules("libvirt");
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules[0].api_groups.as_deref(),
            Some(["batch".to_string()].as_slice())
        );
        assert_eq!(
            rules[0].resources.as_deref(),
            Some(["jobs".to_string()].as_slice())
        );
    }

    #[test]
    fn the_job_grant_is_the_minimum_the_reconciler_uses() {
        // Reads are by the deterministic name from import_job_name, so no
        // list/watch; finished Jobs are reaped by ttlSecondsAfterFinished, so
        // no delete.
        let verbs = backend_additional_rules("libvirt")[0].verbs.clone();
        assert_eq!(verbs, vec!["get", "create", "patch"]);
        for forbidden in ["list", "watch", "delete", "*"] {
            assert!(
                !verbs.iter().any(|v| v == forbidden),
                "{forbidden} is not needed and widens the grant"
            );
        }
    }

    #[test]
    fn a_seeded_libvirt_class_carries_the_job_grant() {
        // The seeded ProviderClass is what the operator turns into a Role; a
        // class without these rules produces a provider that 403s on its first
        // import.
        let class = build_provider_class("libvirt", &opts());
        assert_eq!(
            class.spec.additional_rules,
            backend_additional_rules("libvirt")
        );
    }

    /// Server-side apply to an object that does not exist yet is a **create**.
    /// A role with `patch` but not `create` therefore works for updates and
    /// fails the first time it has to make the object — which is the only time
    /// that matters for a build pipeline.
    ///
    /// This was a real 403 on a live cluster: the imagebuilder's role carried
    /// `patch` with a comment claiming it "covers server-side apply
    /// (create-or-update in one call)", and every OSArtifact creation was
    /// rejected. Unit tests could not see it — the reconciler's SSA call is
    /// mocked, and RBAC is only evaluated by a real apiserver.
    #[test]
    fn a_role_that_server_side_applies_an_object_can_also_create_it() {
        // (role, apiGroup, resource) pairs where the reconciler uses SSA to
        // bring an object into existence, not merely to update one.
        let applied: &[(InstallRole, &str, &str)] =
            &[(InstallRole::Imagebuilder, "build.kairos.io", "osartifacts")];

        for (role, group, resource) in applied {
            let granted = granted_triples(role);
            for verb in ["create", "patch"] {
                assert!(
                    granted.contains(&(
                        (*group).to_string(),
                        (*resource).to_string(),
                        verb.to_string()
                    )),
                    "{} must grant {verb} on {resource} ({group}): server-side \
                     apply creates the object when it is absent",
                    role.name()
                );
            }
        }
    }

    // ---------- namespace hardening (ADR-0016) ---------------------------

    /// The bootstrap CLI must produce the same PodSecurity posture as the
    /// shipped `deploy/controller/namespace.yaml`. It did not: `build_namespace`
    /// set only the app label, so installing via the documented CLI path
    /// (ADR-0013) yielded a control-plane namespace with **no admission floor**,
    /// while the manifest path enforced `restricted`.
    ///
    /// ADR-0016's entire argument — confine the privileged exception to the
    /// build namespace so `banlieue-system` keeps `restricted` — is void if
    /// `banlieue-system` never had it.
    #[test]
    fn the_control_plane_namespace_enforces_restricted() {
        let ns = build_namespace(DEFAULT_NAMESPACE);
        let labels = ns.metadata.labels.expect("namespace must carry labels");
        assert_eq!(
            labels
                .get("pod-security.kubernetes.io/enforce")
                .map(String::as_str),
            Some("restricted"),
            "the control-plane namespace must enforce restricted"
        );
        for k in ["audit", "warn"] {
            assert_eq!(
                labels
                    .get(&format!("pod-security.kubernetes.io/{k}"))
                    .map(String::as_str),
                Some("restricted"),
                "{k} must also be restricted"
            );
        }
    }

    /// kairos' OSArtifact builder needs `privileged: true`, which `baseline`
    /// denies as well as `restricted` — so the build namespace must enforce
    /// `privileged`, i.e. no enforcement at all (ADR-0016).
    #[test]
    fn the_build_namespace_admits_privileged_builds() {
        let ns = build_imagebuild_namespace(DEFAULT_IMAGEBUILD_NAMESPACE);
        let labels = ns.metadata.labels.expect("namespace must carry labels");
        assert_eq!(
            labels
                .get("pod-security.kubernetes.io/enforce")
                .map(String::as_str),
            Some("privileged"),
            "kairos build pods cannot run under restricted or baseline"
        );
    }

    /// Enforcement is off there, so audit and warn must stay at `restricted`:
    /// otherwise a *new* privileged workload appearing in the build namespace
    /// is indistinguishable from the one we knowingly allowed.
    #[test]
    fn the_build_namespace_still_audits_and_warns_at_restricted() {
        let ns = build_imagebuild_namespace(DEFAULT_IMAGEBUILD_NAMESPACE);
        let labels = ns.metadata.labels.expect("namespace must carry labels");
        for k in ["audit", "warn"] {
            assert_eq!(
                labels
                    .get(&format!("pod-security.kubernetes.io/{k}"))
                    .map(String::as_str),
                Some("restricted"),
                "{k} must remain restricted so violations stay visible"
            );
        }
    }

    /// The two namespaces must be distinct — the whole point of ADR-0016.
    #[test]
    fn the_build_namespace_is_not_the_control_plane_namespace() {
        assert_ne!(
            DEFAULT_NAMESPACE, DEFAULT_IMAGEBUILD_NAMESPACE,
            "granting privileged in the control-plane namespace would remove \
             the admission floor from the controller, the operator (an RBAC \
             grantor) and every provider pod"
        );
    }

    // ---------- build scheduling through the installer --------------------

    /// The operator forwards build scheduling to provider workloads and the
    /// imagebuilder sets it on OSArtifacts, but neither can be configured if
    /// the installer cannot pass it. Without this the documented install path
    /// produces a cluster where the flags must be patched on by hand.
    #[test]
    fn both_scheduling_consumers_receive_the_flags() {
        let opts = InstallOptions {
            build_node_selector: vec!["banlieue.io/imagebuild=true".to_string()],
            build_toleration: vec!["dedicated=imagebuild:NoSchedule".to_string()],
            image_digest: None,
            ..opts()
        };

        for role in [InstallRole::Operator, InstallRole::Imagebuilder] {
            let args = role.args_with(&opts);
            assert!(
                args.windows(2).any(|w| {
                    w[0] == "--build-node-selector" && w[1] == "banlieue.io/imagebuild=true"
                }),
                "{} must receive the selector: {args:?}",
                role.name()
            );
            assert!(
                args.windows(2).any(|w| {
                    w[0] == "--build-toleration" && w[1] == "dedicated=imagebuild:NoSchedule"
                }),
                "{} must receive the toleration: {args:?}",
                role.name()
            );
        }
    }

    /// The controller has nothing to do with image builds; giving it flags it
    /// does not declare would make it fail to start.
    #[test]
    fn the_controller_is_not_given_build_scheduling_flags() {
        let opts = InstallOptions {
            build_node_selector: vec!["a=b".to_string()],
            build_toleration: vec!["k=v:NoSchedule".to_string()],
            image_digest: None,
            ..opts()
        };
        let args = InstallRole::Controller.args_with(&opts);
        assert!(
            !args.iter().any(|a| a == "--build-node-selector"),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a == "--build-toleration"), "{args:?}");
    }

    #[test]
    fn unset_scheduling_emits_no_flags() {
        // clap rejects an empty value, so an unset selector must be omitted
        // rather than passed as "".
        let args = InstallRole::Imagebuilder.args_with(&opts());
        assert_eq!(args, vec!["imagebuilder".to_string()]);
    }

    // ---------- digest pinning --------------------------------------------

    /// A Deployment built from a mutable tag has a spec that is identical
    /// across image pushes, so a new image triggers no rollout and pods keep
    /// running old layers. `imagePullPolicy: Always` does not save it — that
    /// only applies when a pod is created. Observed on the homelab cluster:
    /// a provider pod ran an hour-old digest while looking perfectly healthy.
    #[test]
    fn a_seeded_class_can_be_pinned_to_a_digest() {
        let opts = InstallOptions {
            image_digest: Some("sha256:0f756fa0".to_string()),
            ..opts()
        };
        let class = build_provider_class("libvirt", &opts);
        let reference = class.spec.image.as_ref().expect("image").reference();
        assert!(
            reference.ends_with("@sha256:0f756fa0"),
            "the class must pin the digest: {reference}"
        );
        assert!(
            reference.contains(":v0.1.0"),
            "and keep the tag as documentation of intent: {reference}"
        );
    }

    #[test]
    fn without_a_digest_a_class_still_references_its_tag() {
        let class = build_provider_class("libvirt", &opts());
        assert!(
            !class
                .spec
                .image
                .as_ref()
                .expect("image")
                .reference()
                .contains('@')
        );
    }

    // ------------------------------------------------------------------
    // The CRD list must not drift from crdgen's
    // ------------------------------------------------------------------

    /// `banlieue bootstrap` installs whatever `build_crds` lists, while
    /// GitOps installs whatever `crdgen` wrote into `deploy/crds/`. Nothing
    /// links the two but this test — and a CRD present in one and absent
    /// from the other is invisible until a controller's first patch fails
    /// with "no matches for kind". Found exactly that way: `LibvirtMachine`
    /// (ADR-0050) reached `deploy/crds/` and `crdgen`, but not here.
    #[test]
    fn bootstrap_installs_every_crd_that_is_generated_into_deploy() {
        use std::collections::BTreeSet;

        let installed: BTreeSet<String> = build_crds()
            .iter()
            .map(|c| c.metadata.name.clone().unwrap_or_default())
            .collect();

        let generated: BTreeSet<String> =
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/crds"))
                .expect("deploy/crds must exist")
                .filter_map(|e| {
                    let path = e.ok()?.path();
                    if path.extension()? != "yaml" {
                        return None;
                    }
                    let doc = std::fs::read_to_string(&path).ok()?;
                    let crd: serde_yaml::Value = serde_yaml::from_str(&doc).ok()?;
                    Some(crd.get("metadata")?.get("name")?.as_str()?.to_string())
                })
                .collect();

        assert!(!generated.is_empty(), "no CRDs found in deploy/crds");
        let missing: Vec<&String> = generated.difference(&installed).collect();
        assert!(
            missing.is_empty(),
            "these CRDs are in deploy/crds but `banlieue bootstrap` would not \
             install them: {missing:?}. Add them to build_crds()."
        );
        let extra: Vec<&String> = installed.difference(&generated).collect();
        assert!(
            extra.is_empty(),
            "these CRDs are installed by bootstrap but were never generated \
             into deploy/crds: {extra:?}. Run the regen-crds skill."
        );
    }

    // ----------------------------------------------------------------------
    // cloud-hypervisor: External (ADR-0060 Decisions 3 and 5)
    // ----------------------------------------------------------------------

    fn triples(rules: &[PolicyRule]) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for r in rules {
            for g in r.api_groups.clone().unwrap_or_default() {
                for res in r.resources.clone().unwrap_or_default() {
                    for v in &r.verbs {
                        out.push((g.clone(), res.clone(), v.clone()));
                    }
                }
            }
        }
        out
    }

    #[test]
    fn the_seeded_cloud_hypervisor_class_is_external_and_carries_its_machine_rules() {
        let class = build_provider_class("cloud-hypervisor", &opts());
        assert_eq!(
            class.spec.deployment_mode(),
            banlieue_api::banlieue::ProviderDeployment::External
        );
        assert!(
            class.spec.image.is_none(),
            "nothing is deployed, so no image"
        );
        let t = triples(&class.spec.additional_rules);
        for want in [
            ("cloudhypervisormachines", "patch"),
            ("cloudhypervisormachines/status", "patch"),
            ("cloudhypervisormachines/finalizers", "update"),
        ] {
            assert!(
                t.iter().any(|(g, r, v)| g == "infrastructure.banlieue.io"
                    && r == want.0
                    && v == want.1),
                "{want:?} missing from {t:?}"
            );
        }
        assert!(
            !t.iter().any(|(_, _, v)| v == "create" || v == "delete"),
            "the controller owns machines; the provider never creates or deletes them"
        );
        assert!(
            crate::reconciler::providerclass::assess(&class.spec, true).is_ready(),
            "the seeded class must pass the operator's own readiness check"
        );
    }

    /// RBAC refuses a grantor any permission it does not hold. Every rule the
    /// operator writes into an External provider's Role must therefore be in
    /// its ClusterRole — except the token rule, which it holds only in its
    /// namespaced Role so it can never mint tokens cluster-wide.
    #[test]
    fn the_operator_holds_every_rule_it_grants_an_external_provider() {
        let class = build_provider_class("cloud-hypervisor", &opts());
        let inputs = crate::workload::WorkloadInputs {
            class_name: "cloud-hypervisor",
            class: &class.spec,
            provider_name: "host-a",
            provider_namespace: "banlieue-system",
            workload_namespace: "banlieue-system",
            credentials_secret: None,
            build_toleration: &[],
            ca_bundle_config_map: None,
            ca_bundle_secret: None,
            owner: None,
        };
        let role = crate::workload::build_role(&inputs);
        let cluster = granted_triples(&InstallRole::Operator);
        let namespaced = triples(&build_operator_token_role(&opts()).rules.unwrap_or_default());
        for (g, r, v) in triples(&role.rules.unwrap_or_default()) {
            let held = cluster.contains(&(g.clone(), r.clone(), v.clone()))
                || namespaced.contains(&(g.clone(), r.clone(), v.clone()));
            assert!(held, "operator cannot grant {v} on {r:?} (group {g:?})");
            if r == "serviceaccounts/token" {
                assert!(
                    !cluster.contains(&(g.clone(), r.clone(), v.clone())),
                    "token minting must never be cluster-wide for the operator"
                );
            }
        }
    }

    #[test]
    fn bootstrap_emits_the_operator_token_role_as_the_manifest_declares() {
        let manifest = include_str!("../../../deploy/operator/rbac/role.yaml");
        let first = manifest.split("\n---\n").next().unwrap();
        let declared: Role = serde_yaml::from_str(first).unwrap();
        let built = build_operator_token_role(&opts());
        assert_eq!(built.metadata.name, declared.metadata.name);
        assert_eq!(built.rules, declared.rules);

        let install = build_operator_install(&opts(), &["cloud-hypervisor"], false).unwrap();
        assert!(
            install
                .roles
                .iter()
                .any(|r| r.metadata.name == declared.metadata.name)
        );
        assert!(
            install
                .role_bindings
                .iter()
                .any(|b| b.metadata.name == declared.metadata.name)
        );
    }

    // ----------------------------------------------------------------------
    // Cluster API aggregation and clusterctl packaging (ADR-0096)
    // ----------------------------------------------------------------------

    /// Label CAPI's manager ClusterRole aggregates on.
    const CAPI_AGGREGATE_LABEL: &str = "cluster.x-k8s.io/aggregate-to-manager";

    /// Label ADR-0005 stamps on every CRD that satisfies a CAPI v1beta2
    /// contract.
    const CAPI_CONTRACT_LABEL: &str = "cluster.x-k8s.io/v1beta2";

    /// The group the aggregate role is confined to.
    const INFRASTRUCTURE_GROUP: &str = "infrastructure.banlieue.io";

    /// What CAPI's manager needs on an infrastructure kind: MachineSets create
    /// machines from templates, and deleting a Machine deletes its
    /// infrastructure object.
    const CAPI_VERBS: [&str; 7] = [
        "get", "list", "watch", "create", "update", "patch", "delete",
    ];

    fn capi_role() -> ClusterRole {
        build_capi_aggregate_cluster_role().expect("embedded CAPI aggregate role parses")
    }

    #[test]
    fn capi_aggregate_role_is_named_and_aggregated_to_the_capi_manager() {
        let role = capi_role();
        assert_eq!(CAPI_AGGREGATE_CLUSTER_ROLE, "banlieue-capi-infrastructure");
        assert_eq!(
            role.metadata.name.as_deref(),
            Some(CAPI_AGGREGATE_CLUSTER_ROLE)
        );
        let labels = role.metadata.labels.unwrap_or_default();
        assert_eq!(
            labels.get(CAPI_AGGREGATE_LABEL).map(String::as_str),
            Some("true"),
            "the role exists only to be aggregated into CAPI's manager"
        );
    }

    /// ADR-0096 Decision 4: nothing in `banlieue.io`, no Secrets, IPAM, Events
    /// or Leases. One rule outside the infrastructure group is one too many.
    #[test]
    fn capi_aggregate_role_grants_nothing_outside_the_infrastructure_group() {
        let rules = capi_role().rules.unwrap_or_default();
        assert!(!rules.is_empty(), "the aggregate role grants nothing");
        for rule in &rules {
            assert_eq!(
                rule.api_groups.as_deref(),
                Some(&[INFRASTRUCTURE_GROUP.to_string()][..]),
                "rule {rule:?} reaches outside {INFRASTRUCTURE_GROUP}"
            );
            assert!(
                rule.non_resource_urls.is_none(),
                "no non-resource URLs: {rule:?}"
            );
            assert!(
                rule.resource_names.is_none(),
                "the role grants by kind, not by name: {rule:?}"
            );
            for resource in rule.resources.as_deref().unwrap_or_default() {
                assert!(!resource.contains('*'), "no wildcard resources: {resource}");
            }
            for verb in &rule.verbs {
                assert!(
                    CAPI_VERBS.contains(&verb.as_str()),
                    "verb {verb:?} is not one the contract needs"
                );
            }
        }
    }

    /// The role covers exactly the CRDs that carry the CAPI contract label,
    /// plus `/status` for those with a status subresource, with the full verb
    /// set on each. Derived from the generated CRDs, so a new infrastructure
    /// kind left out of the role fails here instead of failing a CAPI user at
    /// runtime with a 403.
    #[test]
    fn capi_aggregate_role_covers_exactly_the_capi_contract_kinds() {
        let mut expected = std::collections::BTreeSet::new();
        for crd in build_crds() {
            let labels = crd.metadata.labels.clone().unwrap_or_default();
            if !labels.contains_key(CAPI_CONTRACT_LABEL) {
                continue;
            }
            assert_eq!(crd.spec.group, INFRASTRUCTURE_GROUP);
            let plural = crd.spec.names.plural.clone();
            let has_status = crd
                .spec
                .versions
                .iter()
                .any(|v| v.subresources.as_ref().is_some_and(|s| s.status.is_some()));
            for verb in CAPI_VERBS {
                expected.insert((plural.clone(), verb.to_string()));
                if has_status {
                    expected.insert((format!("{plural}/status"), verb.to_string()));
                }
            }
        }
        assert!(
            !expected.is_empty(),
            "no CRD carries the CAPI contract label"
        );

        let mut granted = std::collections::BTreeSet::new();
        for rule in capi_role().rules.unwrap_or_default() {
            for resource in rule.resources.unwrap_or_default() {
                for verb in &rule.verbs {
                    granted.insert((resource.clone(), verb.clone()));
                }
            }
        }
        assert_eq!(granted, expected);
    }

    /// The label used to sit on `banlieue-controller`, handing CAPI's manager
    /// create and delete on every `banlieue.io` kind, IPAM claims, Events and
    /// Leases. ADR-0096 takes it off.
    #[test]
    fn controller_cluster_role_is_not_aggregated_to_the_capi_manager() {
        let role = InstallRole::Controller.cluster_role().unwrap();
        let labels = role.metadata.labels.unwrap_or_default();
        assert!(
            !labels.contains_key(CAPI_AGGREGATE_LABEL),
            "banlieue-controller must not be aggregated into CAPI's manager"
        );
    }

    /// No other ClusterRole the install ships may carry the label either.
    #[test]
    fn only_the_capi_aggregate_role_is_aggregated_to_the_capi_manager() {
        let manifests = build_operator_install(&opts(), &BACKENDS_WITH_ROLES, false).unwrap();
        let aggregated: Vec<_> = manifests
            .cluster_roles
            .iter()
            .filter(|r| {
                r.metadata
                    .labels
                    .as_ref()
                    .is_some_and(|l| l.contains_key(CAPI_AGGREGATE_LABEL))
            })
            .filter_map(|r| r.metadata.name.clone())
            .collect();
        assert_eq!(aggregated, vec![CAPI_AGGREGATE_CLUSTER_ROLE.to_string()]);
    }

    /// `bootstrap operator` is the clusterctl components file (ADR-0096
    /// Decision 2), so it must install the aggregate role, exactly as shipped.
    #[test]
    fn operator_install_ships_the_capi_aggregate_role() {
        let manifests = build_operator_install(&opts(), &["vsphere"], false).unwrap();
        let role = manifests
            .cluster_roles
            .iter()
            .find(|r| r.metadata.name.as_deref() == Some(CAPI_AGGREGATE_CLUSTER_ROLE))
            .expect("bootstrap operator must install the CAPI aggregate role");
        assert_eq!(role, &capi_role());
    }

    /// A single-role install never re-applies cluster-wide platform objects.
    #[test]
    fn role_installs_do_not_ship_the_capi_aggregate_role() {
        for role in [
            InstallRole::Imagebuilder,
            InstallRole::Provider("vsphere".to_string()),
        ] {
            let manifests = build_role_install(&role, &opts()).unwrap();
            assert!(
                !manifests
                    .cluster_roles
                    .iter()
                    .any(|r| r.metadata.name.as_deref() == Some(CAPI_AGGREGATE_CLUSTER_ROLE)),
                "{} install must not carry the CAPI aggregate role",
                role.name()
            );
        }
    }

    /// clusterctl reads the target namespace from the components file's one
    /// Namespace object, and rejects a file that carries more than one.
    #[test]
    fn operator_install_yaml_carries_exactly_one_namespace() {
        let yaml = build_operator_install(&opts(), &["vsphere"], false)
            .unwrap()
            .to_yaml()
            .unwrap();
        let namespaces: Vec<String> = yaml
            .split("\n---\n")
            .filter_map(|doc| serde_yaml::from_str::<serde_yaml::Value>(doc).ok())
            .filter(|doc| doc.get("kind").and_then(|k| k.as_str()) == Some("Namespace"))
            .filter_map(|doc| {
                doc.get("metadata")
                    .and_then(|m| m.get("name"))
                    .and_then(|n| n.as_str())
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(namespaces, vec![DEFAULT_NAMESPACE.to_string()]);
    }

    const CLUSTERCTL_METADATA: &str = include_str!("../../../config/clusterctl/metadata.yaml");

    /// `(major, minor, contract)` for each `releaseSeries` entry.
    fn release_series() -> Vec<(u64, u64, String)> {
        let doc: serde_yaml::Value = serde_yaml::from_str(CLUSTERCTL_METADATA).unwrap();
        assert_eq!(
            doc.get("apiVersion").and_then(|v| v.as_str()),
            Some("clusterctl.cluster.x-k8s.io/v1alpha3")
        );
        assert_eq!(doc.get("kind").and_then(|v| v.as_str()), Some("Metadata"));
        doc.get("releaseSeries")
            .and_then(|s| s.as_sequence())
            .expect("releaseSeries is a list")
            .iter()
            .map(|entry| {
                (
                    entry
                        .get("major")
                        .and_then(serde_yaml::Value::as_u64)
                        .unwrap(),
                    entry
                        .get("minor")
                        .and_then(serde_yaml::Value::as_u64)
                        .unwrap(),
                    entry
                        .get("contract")
                        .and_then(|c| c.as_str())
                        .unwrap()
                        .to_string(),
                )
            })
            .collect()
    }

    /// clusterctl refuses to install a version whose series is missing from
    /// `metadata.yaml`, so a version bump without a new entry would ship a
    /// release `clusterctl init` cannot install (ADR-0096 Decision 3).
    #[test]
    fn clusterctl_metadata_has_an_entry_for_the_workspace_series() {
        let mut parts = env!("CARGO_PKG_VERSION").split('.');
        let major: u64 = parts.next().unwrap().parse().unwrap();
        let minor: u64 = parts.next().unwrap().parse().unwrap();
        let series = release_series();
        assert!(
            series
                .iter()
                .any(|(ma, mi, contract)| *ma == major && *mi == minor && contract == "v1beta2"),
            "config/clusterctl/metadata.yaml has no v1beta2 entry for {major}.{minor}: {series:?}"
        );
    }

    /// Every series banlieue has released implements the v1beta2 contract
    /// (ADR-0005): none may claim another, and none may repeat.
    #[test]
    fn clusterctl_metadata_maps_every_series_to_v1beta2_once() {
        let series = release_series();
        let mut seen = std::collections::BTreeSet::new();
        for (major, minor, contract) in &series {
            assert_eq!(contract, "v1beta2", "{major}.{minor}");
            assert!(seen.insert((*major, *minor)), "{major}.{minor} repeats");
        }
    }
}
