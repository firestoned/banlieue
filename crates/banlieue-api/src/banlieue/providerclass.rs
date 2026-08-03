// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue.io/v1alpha1` ProviderClass CRD.
//!
//! A ProviderClass is the install-time half of a backend: it says *what
//! banlieue runs* for a class of backends (which provider binary role, which
//! image, what pod shape), while a [`Provider`] says *which backend instance*
//! to talk to (endpoint, credentials, capabilities).
//!
//! `banlieue-operator` joins the two: for every Provider it resolves the
//! referenced ProviderClass and creates one Deployment, ServiceAccount, Role
//! and RoleBinding per Provider — the per-instance topology of ADR-0003.
//!
//! Cluster-scoped by design (ADR-0012): what banlieue runs is a platform-owner
//! decision, consumed by Provider CRs in any tenant namespace.
//!
//! [`Provider`]: super::Provider

use crate::common::*;
use k8s_openapi::api::core::v1::{ResourceRequirements, Toleration};
use k8s_openapi::api::rbac::v1::PolicyRule;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Replica count used when `spec.replicas` is unset or not a positive number.
///
/// Provider controllers are leader-elected, so extra replicas buy failover
/// rather than throughput; one is the right default.
pub const DEFAULT_PROVIDER_REPLICAS: i32 = 1;

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "banlieue.io",
    version = "v1alpha1",
    kind = "ProviderClass",
    plural = "providerclasses",
    shortname = "pc",
    status = "ProviderClassStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"Backend","type":"string","jsonPath":".spec.backend"}"#,
    printcolumn = r#"{"name":"Image","type":"string","jsonPath":".spec.image.tag"}"#,
    printcolumn = r#"{"name":"Providers","type":"integer","jsonPath":".status.providers"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type=='Ready')].status"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
/// ProviderClass — what banlieue runs for a class of backends.
///
/// A ProviderClass carries the install metadata for one backend type: which
/// `banlieue provider <backend>` role to run, from which image, with what pod
/// shape and extra permissions. It names no endpoint and holds no credentials
/// — that is a [`Provider`](super::Provider)'s job.
///
/// # Why create one
///
/// - **Make backends self-provisioning.** With a ProviderClass in place,
///   registering a backend is `kubectl apply` of a Provider CR:
///   `banlieue-operator` creates that Provider's Deployment, ServiceAccount,
///   Role and RoleBinding for you. No manifest editing, no helm values.
/// - **Decide the image once.** Every Provider of this class runs the image
///   pinned here, so upgrading a fleet of backends is a one-object edit
///   instead of one edit per backend.
/// - **Separate the two jobs.** Deciding *what banlieue runs* (a platform
///   owner, cluster-scoped) is not the same as registering *a vCenter*
///   (a backend admin, namespaced) — different people, different privileges.
///
/// # How it is used
///
/// `Provider.spec.providerClassRef.name` points at a ProviderClass by name.
/// The operator resolves it, then applies one workload set per Provider, each
/// owned by its Provider CR so deleting the Provider garbage-collects the
/// workload. Each spawned pod runs a server-side filtered watch scoped to its
/// own Provider, so one hung backend cannot stall another (ADR-0003).
///
/// Cluster-scoped: one ProviderClass serves Providers in any namespace.
pub struct ProviderClassSpec {
    /// Which provider backend this class instantiates — the
    /// `banlieue provider <backend>` subcommand spawned Deployments run.
    ///
    /// Well-known values: `vsphere`, `proxmox`, `libvirt`. Kept separate from
    /// the object's name so two classes can pin different images or pod shapes
    /// for the same backend (e.g. a canary class).
    pub backend: String,

    /// Container image every provider workload of this class runs.
    pub image: ProviderImage,

    /// Namespace to create provider workloads in.
    ///
    /// When unset — the recommended setting — each Provider's workload is
    /// created in **that Provider's own namespace**, alongside the credentials
    /// Secret it reads. That keeps every object in one namespace, so all of
    /// them can carry an `ownerReference` to the Provider and be
    /// garbage-collected with it (a cross-namespace owner reference is invalid
    /// and would have the collector delete the dependent immediately).
    ///
    /// Setting this pins every workload of the class to one namespace instead.
    /// The generated Role still has to be created next to the Secret it grants
    /// access to — in the Provider's namespace — so the two split apart, and
    /// the operator falls back to finalizer-driven cleanup for the objects that
    /// can no longer be owned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_namespace: Option<String>,

    /// Replicas for each provider Deployment. Defaults to
    /// [`DEFAULT_PROVIDER_REPLICAS`]. Provider controllers are leader-elected,
    /// so values above one provide failover, not parallelism.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<i32>,

    /// Resource requests and limits for the provider container. When unset the
    /// operator applies its own conservative defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceRequirements>,

    /// Node selector applied to provider pods. Useful when backend access is
    /// only routable from particular nodes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub node_selector: BTreeMap<String, String>,

    /// Tolerations applied to provider pods.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tolerations: Vec<Toleration>,

    /// Log level and format passed to spawned workloads.
    #[serde(default, skip_serializing_if = "LoggingSpec::is_empty")]
    pub logging: LoggingSpec,

    /// Extra RBAC rules appended to the per-instance Role the operator
    /// generates for each Provider of this class.
    ///
    /// Note that Kubernetes forbids granting permissions the grantor does not
    /// itself hold: a rule listed here also has to be present in the operator's
    /// own ClusterRole, or the RoleBinding is rejected (ADR-0012).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_rules: Vec<PolicyRule>,

    /// Suspend lifecycle reconciliation for every Provider of this class.
    /// Existing workloads are left running untouched.
    #[serde(default, skip_serializing_if = "is_false")]
    pub paused: bool,
}

impl ProviderClassSpec {
    /// Replica count to use for a provider Deployment.
    ///
    /// Falls back to [`DEFAULT_PROVIDER_REPLICAS`] when unset, and clamps
    /// non-positive values to it — a leader-elected controller scaled to zero
    /// is a silently broken backend, not a valid configuration.
    #[must_use]
    pub fn replicas_or_default(&self) -> i32 {
        match self.replicas {
            Some(n) if n > 0 => n,
            _ => DEFAULT_PROVIDER_REPLICAS,
        }
    }

    /// Namespace to create provider workloads in, falling back to `default`
    /// (conventionally the Provider's own namespace) when unset.
    #[must_use]
    pub fn workload_namespace_or<'a>(&'a self, default: &'a str) -> &'a str {
        self.workload_namespace.as_deref().unwrap_or(default)
    }
}

/// The container image provider workloads of a class run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderImage {
    /// Image repository without a tag, e.g. `ghcr.io/firestoned/banlieue`.
    pub repository: String,

    /// Image tag, e.g. `v0.1.0`. Never use `latest` in production — a mutable
    /// tag makes the running version unknowable and defeats rollback.
    pub tag: String,

    /// Image digest, e.g. `sha256:0f756fa0…`. When set it is what actually
    /// gets pulled, and any `tag` becomes documentation of intent.
    ///
    /// Pinning here does more than make the running version knowable. A
    /// Deployment referencing a mutable tag has a spec that is byte-identical
    /// across pushes, so a new image triggers **no rollout** — and
    /// `imagePullPolicy: Always` does not help, because it only applies when a
    /// pod is created. Pods keep running the old layers, looking healthy, for
    /// as long as nothing else disturbs them. A digest changes the spec, so
    /// pushing a new image rolls the workload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,

    /// Image pull policy. When unset, Kubernetes' own default applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_policy: Option<ImagePullPolicy>,

    /// Secrets used to pull the image, for private or mirrored registries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_secrets: Vec<LocalObjectReference>,
}

impl ProviderImage {
    /// Fully qualified image reference for a container spec.
    ///
    /// `repository:tag`, `repository@digest`, or `repository:tag@digest` —
    /// the last being the most useful form, since the tag documents what the
    /// digest is meant to be while the digest is what actually gets pulled.
    #[must_use]
    pub fn reference(&self) -> String {
        let tagged = if self.tag.is_empty() {
            // `repo:@sha256:...` is not a valid reference.
            self.repository.clone()
        } else {
            format!("{}:{}", self.repository, self.tag)
        };
        match &self.digest {
            Some(digest) => format!("{tagged}@{digest}"),
            None => tagged,
        }
    }
}

/// Image pull policy, spelled exactly as Kubernetes spells it.
///
/// Kept as an explicit enum rather than a free string so an invalid policy is
/// rejected by the apiserver at admission instead of surfacing as a pod that
/// will not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ImagePullPolicy {
    /// Always pull, ignoring any locally cached layers.
    Always,
    /// Pull only when the image is not already present on the node.
    IfNotPresent,
    /// Never pull; fail if the image is absent from the node.
    Never,
}

/// Log level and format handed to spawned provider workloads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoggingSpec {
    /// Log level, e.g. `info` or `debug,kube=warn`. Passed through as the
    /// workload's log-level flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,

    /// Log format: `json` for SIEM-friendly structured output, anything else
    /// for the human-readable text formatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

impl LoggingSpec {
    /// Whether neither a level nor a format was set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.level.is_none() && self.format.is_none()
    }
}

/// Observed state of a ProviderClass.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderClassStatus {
    /// Standard Kubernetes conditions. `Ready` reflects whether the class is
    /// usable: its backend is compiled into the running operator and its image
    /// reference is well-formed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["type"],
    ))]
    pub conditions: Vec<Condition>,

    /// Number of Provider CRs currently referencing this class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<i32>,

    /// The generation of the spec the operator has reconciled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

#[inline]
fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
#[path = "providerclass_tests.rs"]
mod providerclass_tests;
