// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Builders for the per-instance provider workload.
//!
//! Every `Provider` gets its own Deployment, ServiceAccount, Role, RoleBinding
//! and ClusterRoleBinding (ADR-0003). These builders are **pure**: they take a
//! resolved [`WorkloadInputs`] and return objects, touching no cluster. That is
//! what lets the reconciler and `banlieue bootstrap` share one definition of
//! the workload shape (ADR-0013) — a workload created by the CLI is identical
//! to one the operator spawns.
//!
//! # Permission split
//!
//! Two bindings, because a namespaced Role cannot grant everything a provider
//! needs:
//!
//! - The **Role** holds the sensitive per-instance grants, narrowed with
//!   `resourceNames` to exactly the credentials Secret this Provider names, its
//!   optional CA bundle, and its own leader-election Lease.
//! - The **ClusterRoleBinding** grants the backend's shared ClusterRole, which
//!   covers cluster-scoped and cross-namespace resources (`VMImage` above all)
//!   that no Role can reach.
//!
//! `resourceNames` only constrains verbs that name a single object —
//! Kubernetes ignores it for `list`, `watch`, `create` and `deletecollection`.
//! Every narrowed rule here therefore sticks to `get`-style verbs, and `create`
//! (needed for the Lease) lives in its own unconstrained rule rather than
//! silently widening a named one.

use std::collections::BTreeMap;

use banlieue_api::banlieue::{ImagePullPolicy, Provider, ProviderClassSpec};
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    Capabilities, Container, ContainerPort, EnvVar, EnvVarSource, HTTPGetAction,
    LocalObjectReference, ObjectFieldSelector, PodSecurityContext, PodSpec, PodTemplateSpec, Probe,
    ResourceRequirements, SeccompProfile, SecurityContext, ServiceAccount,
};
use k8s_openapi::api::rbac::v1::{
    ClusterRoleBinding, PolicyRule, Role, RoleBinding, RoleRef, Subject,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::Resource;

use crate::naming::{
    cluster_scoped_name, component, selector_labels, workload_labels_for, workload_name,
};

/// Container name inside every provider pod.
const CONTAINER_NAME: &str = "provider";

/// Metrics port exposed by provider workloads.
const METRICS_PORT: i32 = 8080;

/// Health (livez/readyz) port exposed by provider workloads.
const HEALTH_PORT: i32 = 8081;

/// Name of the health port, referenced by both probes.
const HEALTH_PORT_NAME: &str = "health";

/// Name of the metrics port.
const METRICS_PORT_NAME: &str = "metrics";

/// Liveness probe timings.
const LIVENESS_INITIAL_DELAY_SECS: i32 = 10;
const LIVENESS_PERIOD_SECS: i32 = 30;
const LIVENESS_TIMEOUT_SECS: i32 = 3;

/// Readiness probe timings.
const READINESS_INITIAL_DELAY_SECS: i32 = 2;
const READINESS_PERIOD_SECS: i32 = 5;
const READINESS_TIMEOUT_SECS: i32 = 2;

/// Probe failure threshold shared by both probes.
const PROBE_FAILURE_THRESHOLD: i32 = 3;

/// Grace period allowing an in-flight reconcile to finish on SIGTERM.
const TERMINATION_GRACE_PERIOD_SECS: i64 = 30;

/// UID/GID provider containers run as (matches the distroless `nonroot` user).
const RUN_AS_NONROOT_UID: i64 = 65532;

/// Default CPU/memory requests and limits when the class does not set any.
const DEFAULT_CPU_REQUEST: &str = "50m";
const DEFAULT_MEMORY_REQUEST: &str = "128Mi";
const DEFAULT_CPU_LIMIT: &str = "1000m";
const DEFAULT_MEMORY_LIMIT: &str = "512Mi";

/// Seccomp profile applied at pod and container level.
const SECCOMP_RUNTIME_DEFAULT: &str = "RuntimeDefault";

/// Everything needed to build one Provider's workload, already resolved.
///
/// Namespaces are separate on purpose: the Deployment and ServiceAccount live
/// in `workload_namespace`, while the Role and RoleBinding must be created in
/// `provider_namespace` — next to the credentials Secret they grant access to.
#[derive(Debug)]
pub struct WorkloadInputs<'a> {
    /// Name of the `ProviderClass` the Provider references.
    pub class_name: &'a str,
    /// The resolved class spec supplying image and pod shape.
    pub class: &'a ProviderClassSpec,
    /// Name of the `Provider` this workload serves.
    pub provider_name: &'a str,
    /// Namespace the `Provider` (and its credentials Secret) lives in.
    pub provider_namespace: &'a str,
    /// Namespace the Deployment and ServiceAccount are created in.
    pub workload_namespace: &'a str,
    /// Name of the Secret holding this backend's credentials.
    pub credentials_secret: &'a str,
    /// Taint tolerations to forward to the provider for its import Jobs.
    ///
    /// No node selector is forwarded: an import Job's placement follows the
    /// artifacts PVC it mounts, which the scheduler resolves from the bound
    /// PV. Only permission to land on a tainted node has to be configured.
    pub build_toleration: &'a [String],
    /// ConfigMap holding an optional CA bundle, when the Provider names one.
    pub ca_bundle_config_map: Option<&'a str>,
    /// Secret holding an optional CA bundle, when the Provider names one.
    pub ca_bundle_secret: Option<&'a str>,
    /// Owner reference to the `Provider`, applied only to objects that share
    /// its namespace (a cross-namespace owner reference is invalid).
    pub owner: Option<OwnerReference>,
}

impl WorkloadInputs<'_> {
    /// Derived name shared by every object in this workload.
    fn name(&self) -> String {
        workload_name(self.class_name, self.provider_name)
    }

    /// Name for the **cluster-scoped** objects in this workload.
    ///
    /// Namespace-qualified, unlike [`WorkloadInputs::name`]: a cluster-scoped
    /// object has no namespace to disambiguate it, so two Providers sharing a
    /// name and class in different namespaces would otherwise collide on one
    /// ClusterRoleBinding and fight over its subject.
    fn cluster_scoped_name(&self) -> String {
        cluster_scoped_name(self.class_name, self.provider_namespace, self.provider_name)
    }

    /// Labels applied to every object in this workload.
    fn labels(&self) -> BTreeMap<String, String> {
        workload_labels_for(
            self.class_name,
            self.provider_namespace,
            self.provider_name,
            &self.class.backend,
        )
    }

    /// Whether the workload objects share the Provider's namespace, and can
    /// therefore carry an owner reference.
    fn workload_is_owned(&self) -> bool {
        self.workload_namespace == self.provider_namespace
    }

    /// Owner reference for objects in the Provider's own namespace.
    fn owner_for(&self, owned: bool) -> Option<Vec<OwnerReference>> {
        match (owned, self.owner.as_ref()) {
            (true, Some(owner)) => Some(vec![owner.clone()]),
            _ => None,
        }
    }
}

/// The complete set of objects backing one Provider.
#[derive(Debug)]
pub struct WorkloadSet {
    /// Identity the provider pod runs as.
    pub service_account: ServiceAccount,
    /// Per-instance namespaced permissions (credentials Secret, Lease, events).
    pub role: Role,
    /// Binds [`WorkloadSet::role`] to the ServiceAccount.
    pub role_binding: RoleBinding,
    /// Binds the backend's shared ClusterRole to the ServiceAccount, covering
    /// the cluster-scoped resources a Role cannot reach.
    pub cluster_role_binding: ClusterRoleBinding,
    /// Read-only, cross-namespace access for the import Job (ADR-0016 §4).
    /// Present for every provider; harmless for backends that never import.
    pub import_role: Role,
    /// Binds [`WorkloadSet::import_role`] to the import ServiceAccount in the
    /// build namespace.
    pub import_role_binding: RoleBinding,
    /// The provider controller itself.
    pub deployment: Deployment,
}

/// Build every object backing one Provider.
///
/// `build_namespace` is where import Jobs run — the privileged namespace of
/// ADR-0016. It appears here only as the subject namespace of the import
/// RoleBinding; no object is created there by this function.
#[must_use]
pub fn build_workload(inputs: &WorkloadInputs<'_>, build_namespace: &str) -> WorkloadSet {
    WorkloadSet {
        service_account: build_service_account(inputs),
        role: build_role(inputs),
        role_binding: build_role_binding(inputs),
        cluster_role_binding: build_cluster_role_binding(inputs),
        import_role: build_import_role(inputs),
        import_role_binding: build_import_role_binding(inputs, build_namespace),
        deployment: build_deployment(inputs),
    }
}

/// ServiceAccount the import Job runs as, in the build namespace (ADR-0016 §4).
///
/// Deliberately **not** the provider controller's identity. That one can create
/// Jobs (ADR-0011), so a workload in the privileged build namespace holding it
/// could create further privileged pods. This identity is read-only by
/// construction.
pub const IMPORT_SERVICE_ACCOUNT: &str = "banlieue-import";

/// Name of the per-Provider import Role / RoleBinding.
#[must_use]
pub fn import_role_name(inputs: &WorkloadInputs<'_>) -> String {
    format!("{}-import", inputs.name())
}

/// Read-only, `resourceNames`-scoped access for the import Job.
///
/// The Job runs in the build namespace beside the artifacts PVC, but the
/// `Provider` and its credentials live with the Provider — a cross-namespace
/// read. This Role lives in the Provider's namespace; the binding names a
/// subject in the build namespace.
///
/// Every rule is `get` on a named object. No list or watch: the Job is told
/// exactly which Provider it serves. No write of any kind, and no `jobs`.
#[must_use]
pub fn build_import_role(inputs: &WorkloadInputs<'_>) -> Role {
    let mut rules = vec![
        named_rule(
            "banlieue.io",
            "providers",
            &["get"],
            &[inputs.provider_name.to_string()],
        ),
        named_rule(
            "",
            "secrets",
            &["get"],
            &[inputs.credentials_secret.to_string()],
        ),
    ];
    // Only when the Provider actually names one: a rule with an empty
    // resourceNames list grants access to EVERY ConfigMap in the namespace.
    if let Some(cm) = inputs.ca_bundle_config_map {
        rules.push(named_rule("", "configmaps", &["get"], &[cm.to_string()]));
    }
    // A CA bundle delivered as a Secret is already covered by the secrets rule
    // above only if it is the credentials Secret; otherwise add it by name.
    if let Some(ca_secret) = inputs.ca_bundle_secret
        && ca_secret != inputs.credentials_secret
    {
        rules.push(named_rule(
            "",
            "secrets",
            &["get"],
            &[ca_secret.to_string()],
        ));
    }

    Role {
        metadata: ObjectMeta {
            name: Some(import_role_name(inputs)),
            namespace: Some(inputs.provider_namespace.to_string()),
            labels: Some(inputs.labels()),
            owner_references: inputs.owner_for(true),
            ..Default::default()
        },
        rules: Some(rules),
    }
}

/// Bind [`build_import_role`] to the import ServiceAccount in `build_namespace`.
#[must_use]
pub fn build_import_role_binding(
    inputs: &WorkloadInputs<'_>,
    build_namespace: &str,
) -> RoleBinding {
    RoleBinding {
        metadata: ObjectMeta {
            name: Some(import_role_name(inputs)),
            // Lives with the Role and the objects it grants, not with the
            // subject — that is how cross-namespace RBAC is expressed.
            namespace: Some(inputs.provider_namespace.to_string()),
            labels: Some(inputs.labels()),
            owner_references: inputs.owner_for(true),
            ..Default::default()
        },
        role_ref: RoleRef {
            api_group: Some("rbac.authorization.k8s.io".to_string()),
            kind: "Role".to_string(),
            name: import_role_name(inputs),
        },
        subjects: Some(vec![Subject {
            kind: "ServiceAccount".to_string(),
            name: IMPORT_SERVICE_ACCOUNT.to_string(),
            namespace: Some(build_namespace.to_string()),
            ..Default::default()
        }]),
    }
}

/// Build an owner reference to a `Provider`.
///
/// Marked `controller` and `blockOwnerDeletion` so the Provider is the single
/// controlling owner and cannot be removed while dependents remain.
#[must_use]
pub fn owner_reference(provider_name: &str, uid: &str) -> OwnerReference {
    OwnerReference {
        api_version: Provider::api_version(&()).to_string(),
        kind: Provider::kind(&()).to_string(),
        name: provider_name.to_string(),
        uid: uid.to_string(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    }
}

/// Build the ServiceAccount the provider pod runs as.
#[must_use]
pub fn build_service_account(inputs: &WorkloadInputs<'_>) -> ServiceAccount {
    ServiceAccount {
        metadata: ObjectMeta {
            name: Some(inputs.name()),
            namespace: Some(inputs.workload_namespace.to_string()),
            labels: Some(inputs.labels()),
            owner_references: inputs.owner_for(inputs.workload_is_owned()),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Build the per-instance Role.
///
/// Created in the **Provider's** namespace, because that is where the
/// credentials Secret it grants access to lives.
#[must_use]
pub fn build_role(inputs: &WorkloadInputs<'_>) -> Role {
    let mut rules = Vec::new();

    // Credentials, and the CA bundle when it is a Secret. `get` only: a
    // resourceNames-scoped rule is meaningless for list/watch, and the provider
    // reads both by name.
    let mut secret_names = vec![inputs.credentials_secret.to_string()];
    if let Some(ca_secret) = inputs.ca_bundle_secret {
        secret_names.push(ca_secret.to_string());
    }
    rules.push(named_rule("", "secrets", &["get"], &secret_names));

    if let Some(ca_config_map) = inputs.ca_bundle_config_map {
        rules.push(named_rule(
            "",
            "configmaps",
            &["get"],
            &[ca_config_map.to_string()],
        ));
    }

    // Leader election. `create` cannot be constrained by resourceNames, so it
    // is a separate rule; everything that names the Lease stays narrowed.
    rules.push(PolicyRule {
        api_groups: Some(vec!["coordination.k8s.io".to_string()]),
        resources: Some(vec!["leases".to_string()]),
        verbs: vec!["create".to_string()],
        ..Default::default()
    });
    rules.push(named_rule(
        "coordination.k8s.io",
        "leases",
        &["get", "update", "patch"],
        &[inputs.name()],
    ));

    // Events for state transitions visible in `kubectl describe provider`.
    for group in ["", "events.k8s.io"] {
        rules.push(PolicyRule {
            api_groups: Some(vec![group.to_string()]),
            resources: Some(vec!["events".to_string()]),
            verbs: vec!["create".to_string(), "patch".to_string()],
            ..Default::default()
        });
    }

    rules.extend(inputs.class.additional_rules.iter().cloned());

    Role {
        metadata: ObjectMeta {
            name: Some(inputs.name()),
            namespace: Some(inputs.provider_namespace.to_string()),
            labels: Some(inputs.labels()),
            owner_references: inputs.owner_for(true),
            ..Default::default()
        },
        rules: Some(rules),
    }
}

/// Build the RoleBinding tying the per-instance Role to the ServiceAccount.
#[must_use]
pub fn build_role_binding(inputs: &WorkloadInputs<'_>) -> RoleBinding {
    RoleBinding {
        metadata: ObjectMeta {
            name: Some(inputs.name()),
            namespace: Some(inputs.provider_namespace.to_string()),
            labels: Some(inputs.labels()),
            owner_references: inputs.owner_for(true),
            ..Default::default()
        },
        role_ref: RoleRef {
            api_group: Some("rbac.authorization.k8s.io".to_string()),
            kind: "Role".to_string(),
            name: inputs.name(),
        },
        subjects: Some(vec![service_account_subject(inputs)]),
    }
}

/// Build the ClusterRoleBinding granting the backend's shared ClusterRole.
///
/// Deliberately **never** owned: a cluster-scoped object with a namespaced
/// owner is treated by the garbage collector as having a missing owner and
/// deleted immediately. The operator's finalizer removes this instead.
#[must_use]
pub fn build_cluster_role_binding(inputs: &WorkloadInputs<'_>) -> ClusterRoleBinding {
    ClusterRoleBinding {
        metadata: ObjectMeta {
            name: Some(inputs.cluster_scoped_name()),
            labels: Some(inputs.labels()),
            ..Default::default()
        },
        role_ref: RoleRef {
            api_group: Some("rbac.authorization.k8s.io".to_string()),
            kind: "ClusterRole".to_string(),
            name: shared_cluster_role_name(&inputs.class.backend),
        },
        subjects: Some(vec![service_account_subject(inputs)]),
    }
}

/// Build the provider Deployment.
#[must_use]
pub fn build_deployment(inputs: &WorkloadInputs<'_>) -> Deployment {
    let labels = inputs.labels();

    Deployment {
        metadata: ObjectMeta {
            name: Some(inputs.name()),
            namespace: Some(inputs.workload_namespace.to_string()),
            labels: Some(labels.clone()),
            owner_references: inputs.owner_for(inputs.workload_is_owned()),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(inputs.class.replicas_or_default()),
            selector: LabelSelector {
                match_labels: Some(selector_labels(inputs.provider_name)),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..Default::default()
                }),
                spec: Some(build_pod_spec(inputs)),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Shared per-backend ClusterRole name, e.g. `banlieue-provider-vsphere`.
///
/// This is the ClusterRole shipped in `deploy/provider-<backend>/rbac/`, and
/// `banlieue bootstrap operator` installs it. The operator only *binds* it — it
/// never generates it, so the permission surface stays reviewable in tree.
#[must_use]
pub fn shared_cluster_role_name(backend: &str) -> String {
    format!("banlieue-{}", component(backend))
}

/// Build the pod spec for a provider workload.
fn build_pod_spec(inputs: &WorkloadInputs<'_>) -> PodSpec {
    PodSpec {
        service_account_name: Some(inputs.name()),
        node_selector: (!inputs.class.node_selector.is_empty())
            .then(|| inputs.class.node_selector.clone()),
        tolerations: (!inputs.class.tolerations.is_empty())
            .then(|| inputs.class.tolerations.clone()),
        image_pull_secrets: (!inputs.class.image.pull_secrets.is_empty()).then(|| {
            inputs
                .class
                .image
                .pull_secrets
                .iter()
                .map(|r| LocalObjectReference {
                    name: r.name.clone(),
                })
                .collect()
        }),
        security_context: Some(PodSecurityContext {
            run_as_non_root: Some(true),
            run_as_user: Some(RUN_AS_NONROOT_UID),
            run_as_group: Some(RUN_AS_NONROOT_UID),
            fs_group: Some(RUN_AS_NONROOT_UID),
            seccomp_profile: Some(SeccompProfile {
                type_: SECCOMP_RUNTIME_DEFAULT.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        termination_grace_period_seconds: Some(TERMINATION_GRACE_PERIOD_SECS),
        containers: vec![build_container(inputs)],
        ..Default::default()
    }
}

/// Build the single provider container.
fn build_container(inputs: &WorkloadInputs<'_>) -> Container {
    Container {
        name: CONTAINER_NAME.to_string(),
        image: Some(inputs.class.image.reference()),
        image_pull_policy: inputs
            .class
            .image
            .pull_policy
            .as_ref()
            .map(|p| pull_policy_str(p).to_string()),
        args: Some(build_args(inputs)),
        env: Some(downward_api_env()),
        ports: Some(vec![
            ContainerPort {
                name: Some(METRICS_PORT_NAME.to_string()),
                container_port: METRICS_PORT,
                protocol: Some("TCP".to_string()),
                ..Default::default()
            },
            ContainerPort {
                name: Some(HEALTH_PORT_NAME.to_string()),
                container_port: HEALTH_PORT,
                protocol: Some("TCP".to_string()),
                ..Default::default()
            },
        ]),
        liveness_probe: Some(http_probe(
            "/livez",
            LIVENESS_INITIAL_DELAY_SECS,
            LIVENESS_PERIOD_SECS,
            LIVENESS_TIMEOUT_SECS,
        )),
        readiness_probe: Some(http_probe(
            "/readyz",
            READINESS_INITIAL_DELAY_SECS,
            READINESS_PERIOD_SECS,
            READINESS_TIMEOUT_SECS,
        )),
        resources: Some(
            inputs
                .class
                .resources
                .clone()
                .unwrap_or_else(default_resources),
        ),
        security_context: Some(SecurityContext {
            allow_privilege_escalation: Some(false),
            read_only_root_filesystem: Some(true),
            capabilities: Some(Capabilities {
                drop: Some(vec!["ALL".to_string()]),
                ..Default::default()
            }),
            seccomp_profile: Some(SeccompProfile {
                type_: SECCOMP_RUNTIME_DEFAULT.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Build the container args selecting and scoping the provider role.
///
/// `--namespace` names the **Provider's** namespace, not the workload's: it
/// scopes the provider's Provider watch, and the two differ whenever a class
/// pins `workloadNamespace`.
fn build_args(inputs: &WorkloadInputs<'_>) -> Vec<String> {
    let mut args = vec![
        "provider".to_string(),
        inputs.class.backend.clone(),
        "--provider-name".to_string(),
        inputs.provider_name.to_string(),
        "--namespace".to_string(),
        inputs.provider_namespace.to_string(),
        "--leader-election-id".to_string(),
        inputs.name(),
        "--leader-election-namespace".to_string(),
        inputs.workload_namespace.to_string(),
    ];

    // The Jobs a provider spawns run the same binary it does, so they run the
    // same image. Without this the provider falls back to its compiled-in
    // default and a provider on one image spawns Jobs on another.
    args.push("--import-image".to_string());
    args.push(inputs.class.image.reference());

    // Only tolerations. Placement of an import Job follows its PVC, so a node
    // selector here would over-constrain: harmless on node-local storage,
    // wrong on network-attached storage where the volume can attach anywhere.
    for value in inputs.build_toleration {
        args.push("--build-toleration".to_string());
        args.push(value.clone());
    }

    if let Some(level) = inputs.class.logging.level.as_ref() {
        args.push("--log-level".to_string());
        args.push(level.clone());
    }
    if let Some(format) = inputs.class.logging.format.as_ref() {
        args.push("--log-format".to_string());
        args.push(format.clone());
    }

    args
}

/// Pod identity via the downward API.
///
/// `POD_NAME` / `POD_NAMESPACE` are the leader-election holder identity.
/// `POD_SERVICE_ACCOUNT` lets a provider hand its *own* identity to any Job it
/// spawns — the libvirt image import does this — so the Job inherits exactly
/// the `resourceNames`-scoped Role built above and nothing wider. Passing the
/// name down beats each provider re-deriving it from [`crate::naming`], which
/// would silently break the day the naming scheme changes.
fn downward_api_env() -> Vec<EnvVar> {
    [
        "metadata.name",
        "metadata.namespace",
        "spec.serviceAccountName",
    ]
    .into_iter()
    .zip(["POD_NAME", "POD_NAMESPACE", "POD_SERVICE_ACCOUNT"])
    .map(|(field_path, name)| EnvVar {
        name: name.to_string(),
        value_from: Some(EnvVarSource {
            field_ref: Some(ObjectFieldSelector {
                field_path: field_path.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .collect()
}

/// Build an HTTP probe against the health port.
fn http_probe(path: &str, initial_delay: i32, period: i32, timeout: i32) -> Probe {
    Probe {
        http_get: Some(HTTPGetAction {
            path: Some(path.to_string()),
            port: IntOrString::String(HEALTH_PORT_NAME.to_string()),
            ..Default::default()
        }),
        initial_delay_seconds: Some(initial_delay),
        period_seconds: Some(period),
        timeout_seconds: Some(timeout),
        failure_threshold: Some(PROBE_FAILURE_THRESHOLD),
        ..Default::default()
    }
}

/// Conservative resource requests and limits used when a class sets none.
fn default_resources() -> ResourceRequirements {
    ResourceRequirements {
        requests: Some(BTreeMap::from([
            ("cpu".to_string(), Quantity(DEFAULT_CPU_REQUEST.to_string())),
            (
                "memory".to_string(),
                Quantity(DEFAULT_MEMORY_REQUEST.to_string()),
            ),
        ])),
        limits: Some(BTreeMap::from([
            ("cpu".to_string(), Quantity(DEFAULT_CPU_LIMIT.to_string())),
            (
                "memory".to_string(),
                Quantity(DEFAULT_MEMORY_LIMIT.to_string()),
            ),
        ])),
        ..Default::default()
    }
}

/// The ServiceAccount subject shared by both bindings.
///
/// `namespace` names where the ServiceAccount actually lives, which is not
/// necessarily the binding's own namespace.
fn service_account_subject(inputs: &WorkloadInputs<'_>) -> Subject {
    Subject {
        kind: "ServiceAccount".to_string(),
        name: inputs.name(),
        namespace: Some(inputs.workload_namespace.to_string()),
        ..Default::default()
    }
}

/// Build a `resourceNames`-scoped policy rule.
fn named_rule(group: &str, resource: &str, verbs: &[&str], names: &[String]) -> PolicyRule {
    PolicyRule {
        api_groups: Some(vec![group.to_string()]),
        resources: Some(vec![resource.to_string()]),
        verbs: verbs.iter().map(|v| (*v).to_string()).collect(),
        resource_names: Some(names.to_vec()),
        ..Default::default()
    }
}

/// Render an [`ImagePullPolicy`] the way Kubernetes spells it.
fn pull_policy_str(policy: &ImagePullPolicy) -> &'static str {
    match policy {
        ImagePullPolicy::Always => "Always",
        ImagePullPolicy::IfNotPresent => "IfNotPresent",
        ImagePullPolicy::Never => "Never",
    }
}

#[cfg(test)]
#[path = "workload_tests.rs"]
mod workload_tests;
