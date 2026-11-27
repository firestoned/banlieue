// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Builders for the per-instance provider workload.
//!
//! A `Provider` whose class runs in-cluster gets its own Deployment,
//! ServiceAccount, Role, RoleBinding and ClusterRoleBinding (ADR-0003). A
//! class with an External workload (a Cloud Hypervisor host provider, which
//! runs on its host) gets no Deployment: only its identity and the bindings
//! it needs, including the opt-in tenant ClusterRole of ADR-0134 when the
//! `Provider` serves other namespaces. These builders are **pure**: they take a
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
//! A provider that runs Jobs (vSphere, libvirt, Azure) gets a third grant: a
//! Role in the build namespace holding exactly its Job verbs
//! ([`build_jobs_role`]). Jobs stay out of the shared ClusterRole, where
//! `create` would reach every namespace.
//!
//! `resourceNames` only constrains verbs that name a single object —
//! Kubernetes ignores it for `list`, `watch`, `create` and `deletecollection`.
//! Every narrowed rule here therefore sticks to `get`-style verbs, and `create`
//! (needed for the Lease) lives in its own unconstrained rule rather than
//! silently widening a named one.

use std::collections::BTreeMap;

use banlieue_api::banlieue::{
    AzureOpenBao, ImagePullPolicy, Provider, ProviderClassSpec, ProviderDeployment,
};
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvVar, LocalObjectReference, PodSpec, PodTemplateSpec, ProjectedVolumeSource,
    ResourceRequirements, ServiceAccount, ServiceAccountTokenProjection, Volume, VolumeMount,
    VolumeProjection,
};
use k8s_openapi::api::rbac::v1::{ClusterRoleBinding, PolicyRule, Role, RoleBinding, Subject};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use kube::Resource;

use crate::naming::{
    cluster_scoped_name, component, selector_labels, workload_labels_for, workload_name,
};
use crate::pod::{
    DEFAULT_CPU_REQUEST, DEFAULT_MEMORY_REQUEST, LIVENESS_INITIAL_DELAY_SECS, LIVENESS_PATH,
    LIVENESS_PERIOD_SECS, READINESS_INITIAL_DELAY_SECS, READINESS_PATH, READINESS_PERIOD_SECS,
    TERMINATION_GRACE_PERIOD_SECS, container_ports, container_security_context, downward_api_env,
    http_probe, pod_security_context, resources,
};
use crate::rbac::{named_rule, role_ref, rule, sa_subject};

/// Container name inside every provider pod.
const CONTAINER_NAME: &str = "provider";

/// Name of the health port, referenced by both probes.
///
/// Public because the NetworkPolicy templates in `deploy/network-policies/`
/// open ingress by this name (ADR-0094), and their test pins it.
pub const HEALTH_PORT_NAME: &str = "health";

/// Name of the metrics port.
///
/// Public for the same reason as [`HEALTH_PORT_NAME`] (ADR-0094).
pub const METRICS_PORT_NAME: &str = "metrics";

/// Probe timeouts; the delays and periods are shared with `bootstrap`.
const LIVENESS_TIMEOUT_SECS: i32 = 3;
const READINESS_TIMEOUT_SECS: i32 = 2;

/// Default CPU/memory limits when the class does not set any.
const DEFAULT_CPU_LIMIT: &str = "1000m";
const DEFAULT_MEMORY_LIMIT: &str = "512Mi";

/// Backend whose provider authenticates by workload identity federation with
/// Entra ID (ADR-0159 Decision 1) and so gets a projected token.
pub const AZURE_BACKEND: &str = "azure";

/// Audience Entra ID requires on a federated ServiceAccount token
/// (ADR-0159 Decision 1). The Entra federated credential must name it too.
pub const AZURE_TOKEN_AUDIENCE: &str = "api://AzureADTokenExchange";

/// Lifetime of the projected token, in seconds: one hour (ADR-0159
/// Decision 1). The kubelet refreshes it before it expires.
pub const AZURE_TOKEN_EXPIRATION_SECS: i64 = 3600;

/// Name of the projected token volume, and of the file inside it.
pub const AZURE_TOKEN_VOLUME: &str = "azure-identity-token";

/// Directory the projected token volume is mounted at, read-only.
pub const AZURE_TOKEN_MOUNT_PATH: &str = "/var/run/secrets/azure/tokens";

/// Environment variable naming the token file, the name the Azure SDKs'
/// workload identity credential reads.
pub const AZURE_FEDERATED_TOKEN_FILE_ENV: &str = "AZURE_FEDERATED_TOKEN_FILE";

/// Backend whose provider authenticates by `AssumeRoleWithWebIdentity` at
/// AWS STS (ADR-0174 Decision 6) and so gets a projected token.
pub const EC2_BACKEND: &str = "ec2";

/// Audience AWS STS requires on a web identity token (ADR-0174 Decision 6).
/// The account's IAM OIDC provider must list it as a client ID.
pub const EC2_TOKEN_AUDIENCE: &str = "sts.amazonaws.com";

/// Lifetime of the EC2 provider's projected token, in seconds: ten minutes,
/// the shortest the kubelet issues (ADR-0174 Decision 6). The token is only
/// exchanged at STS, never sent to EC2, so a short life costs nothing; the
/// kubelet refreshes it before it expires.
pub const EC2_TOKEN_EXPIRATION_SECS: i64 = 600;

/// Name of the EC2 provider's projected token volume, and of the file in it.
pub const EC2_TOKEN_VOLUME: &str = "aws-identity-token";

/// Directory the EC2 provider's token volume is mounted at, read-only.
pub const EC2_TOKEN_MOUNT_PATH: &str = "/var/run/secrets/aws/tokens";

/// Environment variable naming the token file: the name the AWS SDKs read,
/// and the `env` of `banlieue provider ec2 --web-identity-token-file`.
pub const AWS_WEB_IDENTITY_TOKEN_FILE_ENV: &str = "AWS_WEB_IDENTITY_TOKEN_FILE";

/// The projected ServiceAccount token a cloud provider exchanges for its
/// cloud credential, and how its pod sees it. One per backend that
/// authenticates by workload identity federation, never a Secret
/// (ADR-0159 Decision 1, ADR-0174 Decision 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudToken {
    /// The token's `aud` claim, which the cloud's token exchange checks.
    pub audience: &'static str,
    /// Lifetime the kubelet requests, in seconds.
    pub expiration_seconds: i64,
    /// Name of the projected volume, and of the file inside it.
    pub volume: &'static str,
    /// Directory the volume is mounted at, read-only.
    pub mount_path: &'static str,
    /// Environment variable that names the token file to the provider.
    pub env: &'static str,
}

impl CloudToken {
    /// Full path of the token file inside the provider container.
    #[must_use]
    pub fn file(&self) -> String {
        format!("{}/{}", self.mount_path, self.volume)
    }
}

/// Azure's token: exchanged with Entra ID (ADR-0159 Decision 1).
const AZURE_TOKEN: CloudToken = CloudToken {
    audience: AZURE_TOKEN_AUDIENCE,
    expiration_seconds: AZURE_TOKEN_EXPIRATION_SECS,
    volume: AZURE_TOKEN_VOLUME,
    mount_path: AZURE_TOKEN_MOUNT_PATH,
    env: AZURE_FEDERATED_TOKEN_FILE_ENV,
};

/// EC2's token: exchanged at STS by `AssumeRoleWithWebIdentity`
/// (ADR-0174 Decision 6).
const EC2_TOKEN: CloudToken = CloudToken {
    audience: EC2_TOKEN_AUDIENCE,
    expiration_seconds: EC2_TOKEN_EXPIRATION_SECS,
    volume: EC2_TOKEN_VOLUME,
    mount_path: EC2_TOKEN_MOUNT_PATH,
    env: AWS_WEB_IDENTITY_TOKEN_FILE_ENV,
};

/// The projected token `backend`'s provider authenticates with, or `None`
/// for a backend that reads a credentials Secret (or needs none).
#[must_use]
pub fn cloud_token(backend: &str) -> Option<&'static CloudToken> {
    match backend {
        AZURE_BACKEND => Some(&AZURE_TOKEN),
        EC2_BACKEND => Some(&EC2_TOKEN),
        _ => None,
    }
}

/// Name of the OpenBao login token's projected volume, and of the file in it
/// (ADR-0176 Decision 8).
pub const OPENBAO_TOKEN_VOLUME: &str = "openbao-identity-token";

/// Directory the OpenBao login token is mounted at, read-only. Its own
/// directory: the cloud-audience token is never beside it.
pub const OPENBAO_TOKEN_MOUNT_PATH: &str = "/var/run/secrets/openbao";

/// What the provider reads to choose where its assertion comes from, and the
/// OpenBao settings. The names are the client's (`banlieue_azure::assertion`);
/// a test holds the two to each other.
pub const ENV_ASSERTION_SOURCE: &str = "BANLIEUE_ASSERTION_SOURCE";
/// OpenBao's address.
pub const ENV_OPENBAO_ADDR: &str = "BANLIEUE_OPENBAO_ADDR";
/// OpenBao namespace path; rendered only when the block names one.
pub const ENV_OPENBAO_NAMESPACE: &str = "BANLIEUE_OPENBAO_NAMESPACE";
/// The JWT auth mount.
pub const ENV_OPENBAO_AUTH_MOUNT: &str = "BANLIEUE_OPENBAO_AUTH_MOUNT";
/// The auth role bound to this workload's ServiceAccount.
pub const ENV_OPENBAO_LOGIN_ROLE: &str = "BANLIEUE_OPENBAO_LOGIN_ROLE";
/// The identity OIDC role whose token is the assertion.
pub const ENV_OPENBAO_TOKEN_ROLE: &str = "BANLIEUE_OPENBAO_TOKEN_ROLE";
/// The file holding the login token.
pub const ENV_OPENBAO_SA_TOKEN_FILE: &str = "BANLIEUE_OPENBAO_SA_TOKEN_FILE";
/// The value of [`ENV_ASSERTION_SOURCE`] that selects OpenBao.
const ASSERTION_SOURCE_OPENBAO: &str = "openbao";

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
    /// Name of the Secret holding this backend's credentials, when the
    /// Provider names one. `None` grants no Secret access at all
    /// (ADR-0060 Decision 4).
    pub credentials_secret: Option<&'a str>,
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
    /// Label selector (already rendered as a string) for the namespaces the
    /// provider serves besides its own, from `Provider.spec.namespaceSelector`
    /// (ADR-0133). Rendered as `--watch-namespace-selector`. `None` leaves the
    /// provider on its home namespace only.
    pub watch_namespace_selector: Option<&'a str>,
    /// The Azure `Provider`'s `spec.connection.azure.openbao` block, when it
    /// declares one (ADR-0176 Decision 8): the pod then gets a second
    /// projected token and the settings the provider reads. Ignored for any
    /// other backend; `None` renders nothing about OpenBao.
    pub azure_openbao: Option<&'a AzureOpenBao>,
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

    /// Whether this Provider's process runs outside the cluster.
    fn is_external(&self) -> bool {
        self.class.deployment_mode() == ProviderDeployment::External
    }

    /// Namespace of the ServiceAccount. External runs nothing in a workload
    /// namespace, so its identity always lives with the Provider, next to
    /// the Role that grants it.
    fn service_account_namespace(&self) -> &str {
        if self.is_external() {
            return self.provider_namespace;
        }
        self.workload_namespace
    }

    /// Whether the workload objects share the Provider's namespace, and can
    /// therefore carry an owner reference.
    fn workload_is_owned(&self) -> bool {
        self.service_account_namespace() == self.provider_namespace
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
    /// Binds the backend's `-shared` ClusterRole (machine access in tenant
    /// namespaces) to the ServiceAccount, only for a Provider that declares a
    /// namespace selector and a backend that ships one (ADR-0134).
    pub shared_cluster_role_binding: Option<ClusterRoleBinding>,
    /// Read-only, cross-namespace access for the import Job (ADR-0016 §4).
    /// Present for every Managed provider; harmless for backends that never
    /// import. `None` for External: nothing imports through the cluster.
    pub import_role: Option<Role>,
    /// Binds [`WorkloadSet::import_role`] to the import ServiceAccount in the
    /// build namespace.
    pub import_role_binding: Option<RoleBinding>,
    /// The provider's own Job access, in the build namespace only: present
    /// for a Managed Provider of a backend that runs Jobs (vSphere, libvirt,
    /// Azure), `None` otherwise. See [`build_jobs_role`].
    pub jobs_role: Option<Role>,
    /// Binds [`WorkloadSet::jobs_role`] to the provider's ServiceAccount, a
    /// subject in another namespace.
    pub jobs_role_binding: Option<RoleBinding>,
    /// The provider controller itself. `None` for External (ADR-0060
    /// Decision 3): the process runs elsewhere.
    pub deployment: Option<Deployment>,
}

/// Build every object backing one Provider.
///
/// `build_namespace` is where import Jobs run, the privileged namespace of
/// ADR-0016. It is the subject namespace of the import RoleBinding, and the
/// namespace of the provider's Job Role and RoleBinding ([`build_jobs_role`]),
/// the only objects this function places there.
#[must_use]
pub fn build_workload(inputs: &WorkloadInputs<'_>, build_namespace: &str) -> WorkloadSet {
    let managed = !inputs.is_external();
    WorkloadSet {
        service_account: build_service_account(inputs),
        role: build_role(inputs),
        role_binding: build_role_binding(inputs),
        cluster_role_binding: build_cluster_role_binding(inputs),
        shared_cluster_role_binding: build_shared_cluster_role_binding(inputs),
        import_role: managed.then(|| build_import_role(inputs)),
        import_role_binding: managed.then(|| build_import_role_binding(inputs, build_namespace)),
        jobs_role: build_jobs_role(inputs, build_namespace),
        jobs_role_binding: build_jobs_role_binding(inputs, build_namespace),
        deployment: managed.then(|| build_deployment(inputs)),
    }
}

/// Suffix of the per-Provider Job Role and RoleBinding in the build namespace.
const JOBS_ROLE_SUFFIX: &str = "jobs";

/// Every verb the vSphere and Azure providers use on their Jobs.
const JOB_RUNNER_VERBS: &[&str] = &["get", "list", "watch", "create", "patch", "delete"];

/// The rules a provider of `backend` needs on the Jobs it runs in the build
/// namespace, derived from each provider's own `Api<Job>` / `Api<Pod>` calls;
/// `None` for a backend that runs no Job (proxmox, cloud-hypervisor).
///
/// - **libvirt** (ADR-0011): reads its import Job by its deterministic name
///   and server-side-applies it when absent (`create` for an apply that
///   creates, `patch` because apply is a patch). No `list`/`watch` and no
///   `delete`: finished Jobs are reaped by `ttlSecondsAfterFinished`.
/// - **vsphere** (ADR-0020): the same, plus `delete` for
///   `banlieue.io/force-reimport` and a failed Job's retry, and
///   `list`/`watch` for the `VMImage` controller's `.watches()` on its Jobs.
/// - **azure** (ADR-0162 Decision 1): the vSphere set, plus `pods` `list`:
///   a finished Job's only output is its pod's termination message, read by
///   the `batch.kubernetes.io/job-name` label. No `get`, `watch` or write on
///   pods, and no `pods/log`.
///
/// None of them uses `update`. These rules sit in a Role in the build
/// namespace, never in the backend's cluster-wide ClusterRole: bound
/// cluster-wide, `jobs` `create` is pod create in every namespace, as any
/// ServiceAccount there.
#[must_use]
pub fn job_rules(backend: &str) -> Option<Vec<PolicyRule>> {
    match backend {
        "libvirt" => Some(vec![rule("batch", "jobs", &["get", "create", "patch"])]),
        "vsphere" => Some(vec![rule("batch", "jobs", JOB_RUNNER_VERBS)]),
        AZURE_BACKEND => Some(vec![
            rule("batch", "jobs", JOB_RUNNER_VERBS),
            rule("", "pods", &["list"]),
        ]),
        _ => None,
    }
}

/// Name of a Provider's Job Role and RoleBinding in the build namespace.
///
/// Namespace-qualified like the cluster-scoped objects: every Provider's Job
/// Role shares the one build namespace, so two Providers with the same name
/// and class in different namespaces would otherwise collide there.
#[must_use]
pub fn jobs_role_name(class: &str, provider_namespace: &str, provider: &str) -> String {
    cluster_scoped_name(
        class,
        provider_namespace,
        &format!("{provider}-{JOBS_ROLE_SUFFIX}"),
    )
}

/// The rules of [`build_jobs_role`] for this Provider, or `None` when it gets
/// no Job access: an External provider runs nothing in the cluster.
fn wanted_job_rules(inputs: &WorkloadInputs<'_>) -> Option<Vec<PolicyRule>> {
    if inputs.is_external() {
        return None;
    }
    job_rules(&inputs.class.backend)
}

/// The provider's Job access, as a Role **in the build namespace**, where its
/// Jobs and the artifacts PVC they mount live (ADR-0016). The provider's own
/// code only ever creates Jobs there (`Api::namespaced` on its
/// `--build-namespace`, whose default is the operator's build namespace).
///
/// Never owned: its Provider lives in another namespace, and a
/// cross-namespace owner reference is invalid. The labels pin the Provider's
/// name and namespace, so the finalizer's label-based cleanup (and the orphan
/// prune) removes it, as it does the ClusterRoleBinding.
#[must_use]
pub fn build_jobs_role(inputs: &WorkloadInputs<'_>, build_namespace: &str) -> Option<Role> {
    let rules = wanted_job_rules(inputs)?;
    Some(Role {
        metadata: ObjectMeta {
            name: Some(jobs_role_name(
                inputs.class_name,
                inputs.provider_namespace,
                inputs.provider_name,
            )),
            namespace: Some(build_namespace.to_string()),
            labels: Some(inputs.labels()),
            ..Default::default()
        },
        rules: Some(rules),
    })
}

/// Bind [`build_jobs_role`] to the provider's ServiceAccount, which lives in
/// the workload namespace: the binding sits with the Role and the Jobs it
/// grants, and names a subject elsewhere. Never owned, for the reason the
/// Role is not.
#[must_use]
pub fn build_jobs_role_binding(
    inputs: &WorkloadInputs<'_>,
    build_namespace: &str,
) -> Option<RoleBinding> {
    wanted_job_rules(inputs)?;
    let name = jobs_role_name(
        inputs.class_name,
        inputs.provider_namespace,
        inputs.provider_name,
    );
    Some(RoleBinding {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(build_namespace.to_string()),
            labels: Some(inputs.labels()),
            ..Default::default()
        },
        role_ref: role_ref("Role", name),
        subjects: Some(vec![service_account_subject(inputs)]),
    })
}

/// ServiceAccount the import Job runs as, in the build namespace (ADR-0016 §4).
///
/// Deliberately **not** the provider controller's identity. That one can create
/// Jobs (ADR-0011), so a workload in the privileged build namespace holding it
/// could create further privileged pods. This identity is read-only by
/// construction.
pub const IMPORT_SERVICE_ACCOUNT: &str =
    banlieue_provider_sdk::defaults::DEFAULT_IMPORT_SERVICE_ACCOUNT;

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
    let mut rules = vec![named_rule(
        "banlieue.io",
        "providers",
        &["get"],
        &[inputs.provider_name.to_string()],
    )];
    if let Some(credentials) = inputs.credentials_secret {
        rules.push(named_rule(
            "",
            "secrets",
            &["get"],
            &[credentials.to_string()],
        ));
    }
    // Only when the Provider actually names one: a rule with an empty
    // resourceNames list grants access to EVERY ConfigMap in the namespace.
    if let Some(cm) = inputs.ca_bundle_config_map {
        rules.push(named_rule("", "configmaps", &["get"], &[cm.to_string()]));
    }
    // A CA bundle delivered as a Secret is already covered by the secrets rule
    // above only if it is the credentials Secret; otherwise add it by name.
    if let Some(ca_secret) = inputs.ca_bundle_secret
        && Some(ca_secret) != inputs.credentials_secret
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
        role_ref: role_ref("Role", import_role_name(inputs)),
        subjects: Some(vec![sa_subject(IMPORT_SERVICE_ACCOUNT, build_namespace)]),
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
            namespace: Some(inputs.service_account_namespace().to_string()),
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
    // Only names that exist: a rule with an empty resourceNames list would
    // grant EVERY Secret in the namespace.
    let secret_names: Vec<String> = inputs
        .credentials_secret
        .into_iter()
        .chain(inputs.ca_bundle_secret)
        .map(str::to_string)
        .collect();
    if !secret_names.is_empty() {
        rules.push(named_rule("", "secrets", &["get"], &secret_names));
    }

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
    rules.push(rule("coordination.k8s.io", "leases", &["create"]));
    rules.push(named_rule(
        "coordination.k8s.io",
        "leases",
        &["get", "update", "patch"],
        &[inputs.name()],
    ));

    // Events for state transitions visible in `kubectl describe provider`.
    for group in ["", "events.k8s.io"] {
        rules.push(rule(group, "events", &["create", "patch"]));
    }

    if inputs.is_external() {
        rules.extend(external_rules(inputs));
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

/// What an External provider needs on top of the common rules, each scoped
/// to its own objects (ADR-0060 Decision 5):
///
/// - its own `Provider` and `Provider` status, by name. `resourceNames`
///   covers `list` and `watch` too when the request filters on
///   `metadata.name`, which is exactly the provider's watch;
/// - `create` on its own ServiceAccount's `token` subresource, to renew the
///   bound token it authenticates with.
///
/// A Managed provider needs none of this: its Provider access comes from
/// the backend's ClusterRole, and its pod gets a projected token.
fn external_rules(inputs: &WorkloadInputs<'_>) -> Vec<PolicyRule> {
    let me = [inputs.provider_name.to_string()];
    vec![
        named_rule("banlieue.io", "providers", &["get", "list", "watch"], &me),
        named_rule(
            "banlieue.io",
            "providers/status",
            &["get", "update", "patch"],
            &me,
        ),
        named_rule("", "serviceaccounts/token", &["create"], &[inputs.name()]),
    ]
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
        role_ref: role_ref("Role", inputs.name()),
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
        role_ref: role_ref(
            "ClusterRole",
            shared_cluster_role_name(&inputs.class.backend),
        ),
        subjects: Some(vec![service_account_subject(inputs)]),
    }
}

/// Backends whose provider holds its machine access in tenant namespaces in a
/// second, opt-in `<shared ClusterRole>-shared` ClusterRole rather than in its
/// one shared ClusterRole (ADR-0134). A host-resident provider's token is
/// otherwise namespaced-only, so a host that is not shared must not carry it.
const SHARED_SCOPE_CLUSTER_ROLE_BACKENDS: &[&str] = &["cloud-hypervisor"];

/// Suffix of the opt-in shared-serving ClusterRole and its binding.
const SHARED_SCOPE_SUFFIX: &str = "shared";

/// Whether a Provider of `backend` with this rendered `selector` is bound to
/// the `-shared` ClusterRole.
#[must_use]
pub fn wants_shared_cluster_role(backend: &str, selector: Option<&str>) -> bool {
    selector.is_some() && SHARED_SCOPE_CLUSTER_ROLE_BACKENDS.contains(&backend)
}

/// Name of the opt-in shared-serving ClusterRole, e.g.
/// `banlieue-provider-cloud-hypervisor-shared`.
#[must_use]
pub fn shared_scope_cluster_role_name(backend: &str) -> String {
    format!(
        "{}-{SHARED_SCOPE_SUFFIX}",
        shared_cluster_role_name(backend)
    )
}

/// Name of the cluster-scoped binding of the shared-serving ClusterRole,
/// distinct from [`build_cluster_role_binding`]'s.
#[must_use]
pub fn shared_cluster_role_binding_name(
    class: &str,
    provider_namespace: &str,
    provider: &str,
) -> String {
    cluster_scoped_name(
        class,
        provider_namespace,
        &format!("{provider}-{SHARED_SCOPE_SUFFIX}"),
    )
}

/// Build the ClusterRoleBinding granting the `-shared` ClusterRole, or `None`
/// unless the Provider declares a namespace selector and the backend ships
/// that role (ADR-0134 Decision 1). Never owned, for the reason
/// [`build_cluster_role_binding`] is not.
#[must_use]
pub fn build_shared_cluster_role_binding(
    inputs: &WorkloadInputs<'_>,
) -> Option<ClusterRoleBinding> {
    if !wants_shared_cluster_role(&inputs.class.backend, inputs.watch_namespace_selector) {
        return None;
    }
    Some(ClusterRoleBinding {
        metadata: ObjectMeta {
            name: Some(shared_cluster_role_binding_name(
                inputs.class_name,
                inputs.provider_namespace,
                inputs.provider_name,
            )),
            labels: Some(inputs.labels()),
            ..Default::default()
        },
        role_ref: role_ref(
            "ClusterRole",
            shared_scope_cluster_role_name(&inputs.class.backend),
        ),
        subjects: Some(vec![service_account_subject(inputs)]),
    })
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
        spec: DeploymentSpec {
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
        },
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

/// Whether this workload's provider authenticates by workload identity
/// federation with Entra ID, which alone honours an OpenBao block (ADR-0176).
fn uses_azure_workload_identity(inputs: &WorkloadInputs<'_>) -> bool {
    inputs.class.backend == AZURE_BACKEND
}

/// The OpenBao block that applies: only an Azure workload honours one.
fn openbao<'a>(inputs: &WorkloadInputs<'a>) -> Option<&'a AzureOpenBao> {
    inputs
        .azure_openbao
        .filter(|_| uses_azure_workload_identity(inputs))
}

/// Full path of the OpenBao login token inside the provider container.
#[must_use]
pub fn openbao_token_file() -> String {
    format!("{OPENBAO_TOKEN_MOUNT_PATH}/{OPENBAO_TOKEN_VOLUME}")
}

/// The ServiceAccount token the provider logs in to OpenBao with
/// (ADR-0176): the audience the block declares, one hour, refreshed by the
/// kubelet. A second token, never the cloud-audience one.
fn openbao_token_volume(bao: &AzureOpenBao) -> Volume {
    Volume {
        name: OPENBAO_TOKEN_VOLUME.to_string(),
        projected: Some(ProjectedVolumeSource {
            sources: Some(vec![VolumeProjection {
                service_account_token: Some(ServiceAccountTokenProjection {
                    audience: Some(bao.audience.clone()),
                    expiration_seconds: Some(AZURE_TOKEN_EXPIRATION_SECS),
                    path: OPENBAO_TOKEN_VOLUME.to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Read-only mount of [`openbao_token_volume`].
fn openbao_token_mount() -> VolumeMount {
    VolumeMount {
        name: OPENBAO_TOKEN_VOLUME.to_string(),
        mount_path: OPENBAO_TOKEN_MOUNT_PATH.to_string(),
        read_only: Some(true),
        ..Default::default()
    }
}

/// The settings the provider reads to take its assertion from OpenBao: a
/// rendering of the block, which stays the one source of truth.
fn openbao_env(bao: &AzureOpenBao) -> Vec<EnvVar> {
    let var = |name: &str, value: &str| EnvVar {
        name: name.to_string(),
        value: Some(value.to_string()),
        ..Default::default()
    };
    let mut env = vec![
        var(ENV_ASSERTION_SOURCE, ASSERTION_SOURCE_OPENBAO),
        var(ENV_OPENBAO_ADDR, &bao.address),
    ];
    if let Some(namespace) = &bao.namespace {
        env.push(var(ENV_OPENBAO_NAMESPACE, namespace));
    }
    env.extend([
        var(ENV_OPENBAO_AUTH_MOUNT, &bao.auth_mount),
        var(ENV_OPENBAO_LOGIN_ROLE, &bao.login_role),
        var(ENV_OPENBAO_TOKEN_ROLE, &bao.token_role),
        var(ENV_OPENBAO_SA_TOKEN_FILE, &openbao_token_file()),
    ]);
    env
}

/// Full path of the projected token file inside an Azure provider container.
#[must_use]
pub fn azure_federated_token_file() -> String {
    AZURE_TOKEN.file()
}

/// Full path of the projected token file inside an EC2 provider container:
/// the file `banlieue provider ec2` reads by default.
#[must_use]
pub fn ec2_web_identity_token_file() -> String {
    EC2_TOKEN.file()
}

/// The projected ServiceAccount token `token` describes: its audience and
/// lifetime, refreshed by the kubelet. It is the provider's only cloud
/// credential; the provider never writes it anywhere.
pub(crate) fn cloud_token_volume(token: &CloudToken) -> Volume {
    Volume {
        name: token.volume.to_string(),
        projected: Some(ProjectedVolumeSource {
            sources: Some(vec![VolumeProjection {
                service_account_token: Some(ServiceAccountTokenProjection {
                    audience: Some(token.audience.to_string()),
                    expiration_seconds: Some(token.expiration_seconds),
                    path: token.volume.to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Read-only mount of [`cloud_token_volume`].
pub(crate) fn cloud_token_mount(token: &CloudToken) -> VolumeMount {
    VolumeMount {
        name: token.volume.to_string(),
        mount_path: token.mount_path.to_string(),
        read_only: Some(true),
        ..Default::default()
    }
}

/// The variable naming the token file to the provider.
pub(crate) fn cloud_token_env(token: &CloudToken) -> EnvVar {
    EnvVar {
        name: token.env.to_string(),
        value: Some(token.file()),
        ..Default::default()
    }
}

/// Container environment: the downward API identity, plus the token file for
/// a cloud provider. Tenant, client, subscription, region, role and authority
/// host are not passed here: the provider reads them from its own
/// `Provider`'s `spec.connection`, the one source of truth.
fn container_env(inputs: &WorkloadInputs<'_>) -> Vec<EnvVar> {
    let mut env = downward_api_env(DOWNWARD_API_ENV);
    if let Some(token) = cloud_token(&inputs.class.backend) {
        env.push(cloud_token_env(token));
    }
    if let Some(bao) = openbao(inputs) {
        env.extend(openbao_env(bao));
    }
    env
}

/// The pod's volumes: only a cloud provider mounts anything, the cloud token
/// first and unchanged, then (Azure only) the OpenBao login token when
/// declared.
fn pod_volumes(inputs: &WorkloadInputs<'_>) -> Option<Vec<Volume>> {
    let token = cloud_token(&inputs.class.backend)?;
    let mut volumes = vec![cloud_token_volume(token)];
    volumes.extend(openbao(inputs).map(openbao_token_volume));
    Some(volumes)
}

/// The container's mounts, matching [`pod_volumes`].
fn container_mounts(inputs: &WorkloadInputs<'_>) -> Option<Vec<VolumeMount>> {
    let token = cloud_token(&inputs.class.backend)?;
    let mut mounts = vec![cloud_token_mount(token)];
    mounts.extend(openbao(inputs).map(|_| openbao_token_mount()));
    Some(mounts)
}

/// Build the pod spec for a provider workload.
fn build_pod_spec(inputs: &WorkloadInputs<'_>) -> PodSpec {
    PodSpec {
        service_account_name: Some(inputs.name()),
        node_selector: (!inputs.class.node_selector.is_empty())
            .then(|| inputs.class.node_selector.clone()),
        tolerations: (!inputs.class.tolerations.is_empty())
            .then(|| inputs.class.tolerations.clone()),
        image_pull_secrets: inputs
            .class
            .image
            .as_ref()
            .filter(|i| !i.pull_secrets.is_empty())
            .map(|i| {
                i.pull_secrets
                    .iter()
                    .map(|r| LocalObjectReference {
                        name: r.name.clone(),
                    })
                    .collect()
            }),
        security_context: Some(pod_security_context()),
        termination_grace_period_seconds: Some(TERMINATION_GRACE_PERIOD_SECS),
        containers: vec![build_container(inputs)],
        // Only a cloud provider (Azure, EC2) mounts anything: every other
        // backend's pod spec is unchanged, with no volumes at all.
        volumes: pod_volumes(inputs),
        ..Default::default()
    }
}

/// Build the single provider container.
fn build_container(inputs: &WorkloadInputs<'_>) -> Container {
    Container {
        name: CONTAINER_NAME.to_string(),
        image: inputs.class.image.as_ref().map(|i| i.reference()),
        image_pull_policy: inputs
            .class
            .image
            .as_ref()
            .and_then(|i| i.pull_policy.as_ref())
            .map(|p| pull_policy_str(p).to_string()),
        args: Some(build_args(inputs)),
        env: Some(container_env(inputs)),
        volume_mounts: container_mounts(inputs),
        ports: Some(container_ports()),
        liveness_probe: Some(http_probe(
            LIVENESS_PATH,
            LIVENESS_INITIAL_DELAY_SECS,
            LIVENESS_PERIOD_SECS,
            Some(LIVENESS_TIMEOUT_SECS),
        )),
        readiness_probe: Some(http_probe(
            READINESS_PATH,
            READINESS_INITIAL_DELAY_SECS,
            READINESS_PERIOD_SECS,
            Some(READINESS_TIMEOUT_SECS),
        )),
        resources: Some(
            inputs
                .class
                .resources
                .clone()
                .unwrap_or_else(default_resources),
        ),
        security_context: Some(container_security_context()),
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

    // The namespaces served besides home (ADR-0133). `--namespace` above stays
    // the Provider's own namespace: the credentials are there, and the provider
    // reads them from there whatever namespace a machine is in.
    if let Some(selector) = inputs.watch_namespace_selector {
        args.push("--watch-namespace-selector".to_string());
        args.push(selector.to_string());
    }

    // The Jobs a provider spawns run the same binary it does, so they run the
    // same image. Without this the provider falls back to its compiled-in
    // default and a provider on one image spawns Jobs on another.
    if let Some(image) = inputs.class.image.as_ref() {
        args.push("--import-image".to_string());
        args.push(image.reference());
    }

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
const DOWNWARD_API_ENV: &[(&str, &str)] = &[
    ("POD_NAME", "metadata.name"),
    ("POD_NAMESPACE", "metadata.namespace"),
    ("POD_SERVICE_ACCOUNT", "spec.serviceAccountName"),
];

/// Conservative resource requests and limits used when a class sets none.
fn default_resources() -> ResourceRequirements {
    resources(
        DEFAULT_CPU_REQUEST,
        DEFAULT_MEMORY_REQUEST,
        DEFAULT_CPU_LIMIT,
        DEFAULT_MEMORY_LIMIT,
    )
}

/// The ServiceAccount subject shared by both bindings.
///
/// `namespace` names where the ServiceAccount actually lives, which is not
/// necessarily the binding's own namespace.
fn service_account_subject(inputs: &WorkloadInputs<'_>) -> Subject {
    sa_subject(inputs.name(), inputs.service_account_namespace())
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
