// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue bootstrap` — self-contained cluster install (ADR-0013).
//!
//! The binary already contains the authoritative CRD schemas, derived from its
//! own Rust types, which makes it the one installer that cannot be out of sync
//! with what it installs.
//!
//! ```text
//! banlieue bootstrap operator            # the normal path
//! banlieue bootstrap provider <backend>  # air-gapped / manual escape hatch
//! banlieue bootstrap imagebuilder
//! ```
//!
//! This module lives beside the reconciler on purpose: both build workloads,
//! and sharing one set of builders is what keeps a CLI-installed provider
//! identically shaped to one the operator spawns.
//!
//! # Object sources
//!
//! - **CRDs** are generated at runtime from the Rust types via the same
//!   `crdgen_support::prepared()` path `crdgen` uses.
//! - **ClusterRoles** are `include_str!`-embedded from `deploy/*/rbac/`, so the
//!   shipped manifests stay the single source of truth and a GitOps install
//!   grants exactly the same permissions as a bootstrap install. Moving one of
//!   those files breaks the build — which is the point.
//! - Everything else (Namespace, ServiceAccount, bindings, ConfigMap,
//!   Deployment, ProviderClass) is built here, because it is parameterised by
//!   namespace, image tag, and registry.

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{
    ImagePullPolicy, Provider, ProviderClass, ProviderClassSpec, ProviderImage, VMClass, VMImage,
    VirtualMachine,
};
use banlieue_api::crdgen_support::prepared;
use banlieue_api::infrastructure::{VSphereCluster, VSphereMachine, VSphereMachineTemplate};
use banlieue_provider_sdk::client::build_client;
use banlieue_provider_sdk::ssa::server_side_apply;
use clap::{Args, Subcommand};
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    ConfigMap, Container, ContainerPort, EnvVar, EnvVarSource, HTTPGetAction, Namespace,
    ObjectFieldSelector, PodSecurityContext, PodSpec, PodTemplateSpec, Probe, ResourceRequirements,
    SeccompProfile, SecurityContext, ServiceAccount,
};
use k8s_openapi::api::rbac::v1::{
    ClusterRole, ClusterRoleBinding, PolicyRule, Role, RoleBinding, RoleRef, Subject,
};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::{Api, CustomResourceExt};
use std::collections::BTreeMap;
use tracing::info;

use crate::naming::{APP_NAME, LABEL_COMPONENT, LABEL_MANAGED_BY, LABEL_NAME};

/// Namespace banlieue installs into unless `--namespace` says otherwise.
pub const DEFAULT_NAMESPACE: &str = "banlieue-system";

/// Namespace image builds run in.
///
/// Deliberately **not** [`DEFAULT_NAMESPACE`]. kairos-operator's OSArtifact
/// build pods require `privileged: true`, which the `baseline` profile denies
/// as well as `restricted`, so the namespace hosting them must enforce
/// `privileged` — i.e. no admission floor at all. Granting that in the
/// control-plane namespace would remove the floor from the controller, the
/// operator (an RBAC grantor) and every per-backend provider pod, to
/// accommodate one workload. See ADR-0016.
pub const DEFAULT_IMAGEBUILD_NAMESPACE: &str = "banlieue-imagebuild";

/// Image repository, without a tag.
pub const IMAGE_BASE: &str = "ghcr.io/firestoned/banlieue";

/// Image name appended to a `--registry` override.
const IMAGE_NAME: &str = "banlieue";

/// Default image tag: the binary's own version, so bootstrap installs the image
/// matching the binary that ran it.
pub const DEFAULT_IMAGE_TAG: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// Field manager for everything bootstrap writes. Distinct from the operator's,
/// so an object's bootstrap-set fields stay distinguishable from the ones the
/// operator manages later.
pub const FIELD_MANAGER: &str = "banlieue.io/bootstrap";

/// Value of the managed-by label on bootstrap-installed objects.
const MANAGED_BY_BOOTSTRAP: &str = "banlieue-bootstrap";

const METRICS_PORT: i32 = 8080;
const HEALTH_PORT: i32 = 8081;
const HEALTH_PORT_NAME: &str = "health";
const METRICS_PORT_NAME: &str = "metrics";
const RUN_AS_NONROOT_UID: i64 = 65532;
const SECCOMP_RUNTIME_DEFAULT: &str = "RuntimeDefault";
const TERMINATION_GRACE_PERIOD_SECS: i64 = 30;
const PROBE_FAILURE_THRESHOLD: i32 = 3;
const LIVENESS_INITIAL_DELAY_SECS: i32 = 10;
const LIVENESS_PERIOD_SECS: i32 = 30;
const READINESS_INITIAL_DELAY_SECS: i32 = 2;
const READINESS_PERIOD_SECS: i32 = 5;
const DEFAULT_CPU_REQUEST: &str = "50m";
const DEFAULT_MEMORY_REQUEST: &str = "128Mi";
const DEFAULT_CPU_LIMIT: &str = "500m";
const DEFAULT_MEMORY_LIMIT: &str = "256Mi";

/// Embedded ClusterRoles. `include_str!` makes these files build inputs, so a
/// rename fails the build loudly instead of silently drifting.
const CONTROLLER_CLUSTER_ROLE: &str =
    include_str!("../../../deploy/controller/rbac/clusterrole.yaml");
const OPERATOR_CLUSTER_ROLE: &str = include_str!("../../../deploy/operator/rbac/clusterrole.yaml");
const IMAGEBUILDER_CLUSTER_ROLE: &str =
    include_str!("../../../deploy/imagebuilder/rbac/clusterrole.yaml");
const PROVIDER_VSPHERE_CLUSTER_ROLE: &str =
    include_str!("../../../deploy/provider-vsphere/rbac/clusterrole.yaml");
const PROVIDER_LIBVIRT_CLUSTER_ROLE: &str =
    include_str!("../../../deploy/provider-libvirt/rbac/clusterrole.yaml");

/// `banlieue bootstrap <target>`.
#[derive(Debug, Args)]
pub struct Cli {
    #[command(subcommand)]
    pub target: BootstrapTarget,
}

/// What to install.
#[derive(Debug, Subcommand)]
pub enum BootstrapTarget {
    /// Install banlieue: namespace, CRDs, RBAC, the controller and operator
    /// Deployments, and one ProviderClass per backend in this binary.
    ///
    /// After this, registering a backend is `kubectl apply` of a Provider CR.
    Operator {
        #[command(flatten)]
        common: CommonArgs,

        /// Do not create a ProviderClass per compiled-in backend.
        #[arg(long)]
        skip_provider_classes: bool,
    },

    /// Install a standalone provider workload, with no operator involvement.
    ///
    /// For air-gapped or tightly-controlled installs where minting workloads
    /// from a controller is not acceptable. The result is not owned by any
    /// Provider CR, so the operator will neither adopt nor delete it — do not
    /// run both paths for the same backend.
    ///
    /// The standalone provider is scoped to the install namespace
    /// (`--namespace`): it only reconciles Providers there, and its
    /// credential / CA-bundle reads go through a namespaced Role in that
    /// namespace. The shared ClusterRole no longer grants cluster-wide Secret
    /// access (security review 2026-07-31 CHAIN-002), so Providers in other namespaces are not served.
    Provider {
        /// Backend to install (must be compiled into this binary).
        #[arg(value_name = "BACKEND")]
        backend: String,

        #[command(flatten)]
        common: CommonArgs,
    },

    /// Install the VMImage build pipeline.
    Imagebuilder {
        #[command(flatten)]
        common: CommonArgs,
    },
}

/// Flags shared by every bootstrap target.
#[derive(Debug, Args, Clone)]
pub struct CommonArgs {
    /// Namespace to install into.
    #[arg(long, default_value = DEFAULT_NAMESPACE)]
    pub namespace: String,

    /// Image tag to install. Defaults to this binary's own version.
    #[arg(long, default_value = DEFAULT_IMAGE_TAG)]
    pub version: String,

    /// Registry host to pull images from, for air-gapped mirrors. When set the
    /// image becomes `<registry>/banlieue:<version>`.
    #[arg(long)]
    pub registry: Option<String>,

    /// Pin seeded `ProviderClass`es to this image digest, e.g.
    /// `sha256:0f756fa0…`. The tag is kept as documentation of intent; the
    /// digest is what gets pulled.
    ///
    /// Strongly recommended for anything but throwaway clusters: a Deployment
    /// built from a mutable tag has an unchanging spec, so pushing a new image
    /// triggers no rollout and pods keep running old layers while looking
    /// perfectly healthy.
    #[arg(long)]
    pub image_digest: Option<String>,

    /// Restrict image-build and import workloads to nodes carrying these
    /// labels (`key=value`, repeatable).
    ///
    /// A privileged build pod escapes to its node regardless of namespace
    /// (ADR-0016), so pinning builds to dedicated nodes is what bounds an
    /// escape. Passed to the imagebuilder, which sets it on the `OSArtifact`,
    /// and to the operator, which forwards it to every provider workload so
    /// import Jobs can reach the artifacts volume.
    #[arg(long = "build-node-selector", value_name = "KEY=VALUE")]
    pub build_node_selector: Vec<String>,

    /// Tolerate these taints on build and import workloads
    /// (`key[=value]:Effect`, repeatable), so the build node can be tainted to
    /// keep other workloads off it.
    #[arg(long = "build-toleration", value_name = "KEY[=VALUE]:EFFECT")]
    pub build_toleration: Vec<String>,

    /// Print the YAML that would be applied and exit, without contacting a
    /// cluster. Usable with no kubeconfig, and pipeable into `kubectl apply -f -`.
    #[arg(long)]
    pub dry_run: bool,
}

impl CommonArgs {
    /// Split into the install options and the dry-run flag.
    fn split(&self) -> (InstallOptions, bool) {
        (
            InstallOptions {
                namespace: self.namespace.clone(),
                version: self.version.clone(),
                registry: self.registry.clone(),
                build_node_selector: self.build_node_selector.clone(),
                build_toleration: self.build_toleration.clone(),
                image_digest: self.image_digest.clone(),
            },
            self.dry_run,
        )
    }
}

/// Resolved install parameters.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// Namespace to install into.
    pub namespace: String,
    /// Image tag.
    pub version: String,
    /// Optional registry override.
    pub registry: Option<String>,
    /// `--build-node-selector` values, passed to the roles that place build
    /// or import workloads (ADR-0016 follow-up). Empty means no constraint.
    pub build_node_selector: Vec<String>,
    /// `--build-toleration` values, for the same roles.
    pub build_toleration: Vec<String>,
    /// Image digest to pin seeded `ProviderClass`es to, e.g. `sha256:0f75…`.
    ///
    /// Without one the class references a tag, and a Deployment built from a
    /// tag has a spec that never changes when a new image is pushed — so
    /// nothing rolls, and `imagePullPolicy: Always` does not help because it
    /// only applies when a pod is created.
    pub image_digest: Option<String>,
}

/// A banlieue role that can be installed as a workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallRole {
    /// The VirtualMachine scheduler.
    Controller,
    /// The provider lifecycle controller.
    Operator,
    /// The VMImage build pipeline.
    Imagebuilder,
    /// A statically installed backend provider.
    Provider(String),
}

impl InstallRole {
    /// Object name for this role's workload and RBAC.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Controller => "banlieue-controller",
            Self::Operator => "banlieue-operator",
            Self::Imagebuilder => "banlieue-imagebuilder",
            Self::Provider(backend) => match backend.as_str() {
                "vsphere" => "banlieue-provider-vsphere",
                "libvirt" => "banlieue-provider-libvirt",
                _ => "banlieue-provider",
            },
        }
    }

    /// Value of the component label.
    #[must_use]
    pub fn component(&self) -> String {
        match self {
            Self::Controller => "controller".to_string(),
            Self::Operator => "operator".to_string(),
            Self::Imagebuilder => "imagebuilder".to_string(),
            Self::Provider(backend) => format!("provider-{backend}"),
        }
    }

    /// Container args selecting this role's subcommand.
    ///
    /// A statically installed provider carries no `--provider-name`: it serves
    /// every Provider of its class in its watch scope, which is the escape
    /// hatch's whole point. The watch is narrowed to the install namespace in
    /// [`build_container`] (security review 2026-07-31 CHAIN-002), not here — `args()` has no access to the
    /// install options.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        match self {
            Self::Controller => vec!["controller".to_string()],
            Self::Operator => vec!["operator".to_string()],
            Self::Imagebuilder => vec!["imagebuilder".to_string()],
            Self::Provider(backend) => vec!["provider".to_string(), backend.clone()],
        }
    }

    /// Container args including any install-time build-scheduling flags.
    ///
    /// Only the roles that actually place build or import workloads get them:
    /// the imagebuilder sets them on the `OSArtifact`, and the operator
    /// forwards them to each provider workload it creates. The controller
    /// schedules nothing of the sort and does not declare the flags, so
    /// passing them would stop it starting.
    #[must_use]
    pub fn args_with(&self, opts: &InstallOptions) -> Vec<String> {
        let mut args = self.args();
        if !matches!(self, Self::Operator | Self::Imagebuilder) {
            return args;
        }
        // Omitted entirely when unset: clap rejects an empty value.
        for value in &opts.build_node_selector {
            args.push("--build-node-selector".to_string());
            args.push(value.clone());
        }
        for value in &opts.build_toleration {
            args.push("--build-toleration".to_string());
            args.push(value.clone());
        }
        args
    }

    /// Parse this role's embedded ClusterRole.
    ///
    /// # Errors
    /// Returns an error if the embedded YAML is not a valid ClusterRole, which
    /// can only happen if a `deploy/` manifest was edited into invalid YAML.
    pub fn cluster_role(&self) -> Result<ClusterRole> {
        let yaml = match self {
            Self::Controller => CONTROLLER_CLUSTER_ROLE,
            Self::Operator => OPERATOR_CLUSTER_ROLE,
            Self::Imagebuilder => IMAGEBUILDER_CLUSTER_ROLE,
            Self::Provider(backend) => match backend.as_str() {
                "vsphere" => PROVIDER_VSPHERE_CLUSTER_ROLE,
                "libvirt" => PROVIDER_LIBVIRT_CLUSTER_ROLE,
                other => anyhow::bail!("no embedded ClusterRole for backend {other:?}"),
            },
        };
        serde_yaml::from_str(yaml)
            .with_context(|| format!("parsing embedded ClusterRole for {}", self.name()))
    }
}

/// Every object one install applies, in dependency order.
#[derive(Debug, Default)]
pub struct InstallManifests {
    /// Namespace, when this install creates one.
    pub namespace: Namespace,
    /// CRDs. Empty for single-role installs, which never re-apply schemas.
    pub crds: Vec<CustomResourceDefinition>,
    /// One per installed role.
    pub service_accounts: Vec<ServiceAccount>,
    /// Embedded, one per installed role.
    pub cluster_roles: Vec<ClusterRole>,
    /// One per installed role.
    pub cluster_role_bindings: Vec<ClusterRoleBinding>,
    /// Namespaced credential access for standalone providers (security review 2026-07-31).
    /// Empty for every other role.
    pub roles: Vec<Role>,
    /// Binds each entry in [`InstallManifests::roles`].
    pub role_bindings: Vec<RoleBinding>,
    /// One per installed role.
    pub config_maps: Vec<ConfigMap>,
    /// One per installed role.
    pub deployments: Vec<Deployment>,
    /// Seeded classes, so registering a backend needs only a Provider CR.
    pub provider_classes: Vec<ProviderClass>,
    /// The privileged build namespace, for installs that need one (ADR-0016).
    /// `None` for every role except the imagebuilder.
    pub imagebuild_namespace: Option<Namespace>,
    /// Whether [`InstallManifests::namespace`] should be applied.
    creates_namespace: bool,
}

impl InstallManifests {
    /// Render every object as a multi-document YAML stream in apply order.
    ///
    /// # Errors
    /// Returns an error if any object fails to serialize.
    pub fn to_yaml(&self) -> Result<String> {
        let mut out = String::new();
        if self.creates_namespace {
            push_doc(&mut out, &self.namespace)?;
        }
        if let Some(ns) = &self.imagebuild_namespace {
            push_doc(&mut out, ns)?;
        }
        for crd in &self.crds {
            push_doc(&mut out, crd)?;
        }
        for sa in &self.service_accounts {
            push_doc(&mut out, sa)?;
        }
        for cr in &self.cluster_roles {
            push_doc(&mut out, cr)?;
        }
        for crb in &self.cluster_role_bindings {
            push_doc(&mut out, crb)?;
        }
        for role in &self.roles {
            push_doc(&mut out, role)?;
        }
        for rb in &self.role_bindings {
            push_doc(&mut out, rb)?;
        }
        for cm in &self.config_maps {
            push_doc(&mut out, cm)?;
        }
        for deployment in &self.deployments {
            push_doc(&mut out, deployment)?;
        }
        for class in &self.provider_classes {
            push_doc(&mut out, class)?;
        }
        Ok(out)
    }
}

/// Append one YAML document, with its leading separator.
fn push_doc<T: serde::Serialize>(out: &mut String, object: &T) -> Result<()> {
    out.push_str("---\n");
    out.push_str(&serde_yaml::to_string(object)?);
    Ok(())
}

/// Fully qualified image reference for an install.
#[must_use]
pub fn resolve_image(opts: &InstallOptions) -> String {
    match opts.registry.as_deref() {
        Some(registry) => format!("{registry}/{IMAGE_NAME}:{}", opts.version),
        None => format!("{IMAGE_BASE}:{}", opts.version),
    }
}

/// Every CRD this binary implements, post-processed exactly as `crdgen` does.
#[must_use]
pub fn build_crds() -> Vec<CustomResourceDefinition> {
    vec![
        prepared(Provider::crd()),
        prepared(ProviderClass::crd()),
        prepared(VMClass::crd()),
        prepared(VMImage::crd()),
        prepared(VirtualMachine::crd()),
        prepared(VSphereCluster::crd()),
        prepared(VSphereMachine::crd()),
        prepared(VSphereMachineTemplate::crd()),
    ]
}

/// Build the full platform install.
///
/// # Arguments
/// * `opts` - namespace, version, registry.
/// * `backends` - backends compiled into this binary, seeded as ProviderClasses.
/// * `skip_provider_classes` - omit the seeded classes.
/// # Errors
/// Returns an error if a role's embedded ClusterRole is missing or unparseable.
pub fn build_operator_install(
    opts: &InstallOptions,
    backends: &[&str],
    skip_provider_classes: bool,
) -> Result<InstallManifests> {
    let mut manifests = InstallManifests {
        namespace: build_namespace(&opts.namespace),
        crds: build_crds(),
        creates_namespace: true,
        ..Default::default()
    };

    for role in [InstallRole::Controller, InstallRole::Operator] {
        add_role(&mut manifests, &role, opts)?;
    }

    // The shared per-backend ClusterRole. The operator BINDS this to each
    // per-instance ServiceAccount but can never create it: minting the very
    // permissions it hands out is exactly the escalation path ADR-0012 refuses.
    // So bootstrap installs it, or the ClusterRoleBinding the operator writes
    // would reference a role that does not exist and the provider pod would run
    // with no permissions at all.
    //
    // A compiled-in backend with no `deploy/provider-<backend>/rbac/` manifest
    // fails the whole install here, loudly — the alternative is an install that
    // looks fine until that backend's first Provider silently cannot work.
    for backend in backends {
        let role = InstallRole::Provider((*backend).to_string());
        manifests
            .cluster_roles
            .push(role.cluster_role().with_context(|| {
                format!(
                    "backend {backend:?} is compiled in but has no ClusterRole; \
                 add deploy/provider-{backend}/rbac/clusterrole.yaml and embed it in bootstrap.rs"
                )
            })?);
    }

    if !skip_provider_classes {
        manifests.provider_classes = backends
            .iter()
            .map(|backend| build_provider_class(backend, opts))
            .collect();
    }

    Ok(manifests)
}

/// Build a single-role install (the `provider` / `imagebuilder` escape hatches).
///
/// Deliberately applies no CRDs: schemas belong to `bootstrap operator`, and
/// re-applying them from a role install invites version skew between the two.
/// # Errors
/// Returns an error if the role's embedded ClusterRole is missing or
/// unparseable — which is what happens for a backend that has no
/// `deploy/provider-<backend>/rbac/clusterrole.yaml` yet.
pub fn build_role_install(role: &InstallRole, opts: &InstallOptions) -> Result<InstallManifests> {
    let mut manifests = InstallManifests {
        namespace: build_namespace(&opts.namespace),
        creates_namespace: false,
        // The imagebuilder drives kairos, whose build pods need a namespace
        // that admits privileged workloads (ADR-0016). No other role does.
        imagebuild_namespace: matches!(role, InstallRole::Imagebuilder)
            .then(|| build_imagebuild_namespace(DEFAULT_IMAGEBUILD_NAMESPACE)),
        // The identity import Jobs run as. It holds nothing on its own; each
        // Provider's operator-created RoleBinding grants it read access to
        // exactly that Provider and its credentials (ADR-0016 §4).
        service_accounts: if matches!(role, InstallRole::Imagebuilder) {
            vec![ServiceAccount {
                metadata: ObjectMeta {
                    name: Some(crate::workload::IMPORT_SERVICE_ACCOUNT.to_string()),
                    namespace: Some(DEFAULT_IMAGEBUILD_NAMESPACE.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }]
        } else {
            Vec::new()
        },
        ..Default::default()
    };
    add_role(&mut manifests, role, opts)?;
    Ok(manifests)
}

/// Append one role's ServiceAccount, ClusterRole, binding, ConfigMap and
/// Deployment.
///
/// A missing ClusterRole is a **hard error**, never a silent omission: the
/// install would otherwise appear to succeed while producing a workload whose
/// ServiceAccount is bound to a ClusterRole that does not exist — a pod with no
/// permissions at all, failing at runtime with opaque 403s instead of at
/// install time with a clear message.
fn add_role(
    manifests: &mut InstallManifests,
    role: &InstallRole,
    opts: &InstallOptions,
) -> Result<()> {
    manifests.cluster_roles.push(
        role.cluster_role()
            .with_context(|| format!("no ClusterRole available for {}", role.name()))?,
    );
    manifests
        .service_accounts
        .push(build_service_account(role, opts));
    manifests
        .cluster_role_bindings
        .push(build_cluster_role_binding(role, opts));
    // A standalone provider reads credentials and CA bundles through a
    // namespaced Role in the install namespace — the shared
    // ClusterRole no longer grants cluster-wide Secret access, and the
    // per-instance resourceNames Role exists only under the operator.
    if matches!(role, InstallRole::Provider(_)) {
        manifests.roles.push(build_namespaced_role(role, opts));
        manifests
            .role_bindings
            .push(build_namespaced_role_binding(role, opts));
    }
    manifests.config_maps.push(build_config_map(role, opts));
    manifests.deployments.push(build_deployment(role, opts));
    Ok(())
}

/// Labels applied to every bootstrap-installed object.
fn labels(role: &InstallRole) -> BTreeMap<String, String> {
    BTreeMap::from([
        (LABEL_NAME.to_string(), APP_NAME.to_string()),
        (LABEL_COMPONENT.to_string(), role.component()),
        (
            LABEL_MANAGED_BY.to_string(),
            MANAGED_BY_BOOTSTRAP.to_string(),
        ),
    ])
}

/// Build the install Namespace.
#[must_use]
pub fn build_namespace(name: &str) -> Namespace {
    // Pod Security Standards `restricted`, matching
    // `deploy/controller/namespace.yaml`. Without these the CLI install path
    // (ADR-0013) produced a control-plane namespace with no admission floor
    // while the manifest path enforced one — and ADR-0016's reasoning depends
    // on this namespace actually being restricted.
    let mut labels = BTreeMap::from([(LABEL_NAME.to_string(), APP_NAME.to_string())]);
    for mode in ["enforce", "audit", "warn"] {
        labels.insert(
            format!("pod-security.kubernetes.io/{mode}"),
            "restricted".to_string(),
        );
    }
    Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            labels: Some(labels),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The namespace image builds run in (ADR-0016).
///
/// Enforcement is `privileged` because kairos' build pods cannot run under
/// `restricted` or `baseline`. `audit` and `warn` stay at `restricted` on
/// purpose: enforcement is off, so a *new* privileged workload appearing here
/// must still be visible rather than silently indistinguishable from the one
/// we knowingly allowed.
///
/// This bounds admission surface, not escape capability — a privileged pod can
/// escape to its node regardless of namespace.
#[must_use]
pub fn build_imagebuild_namespace(name: &str) -> Namespace {
    let mut labels = BTreeMap::from([(LABEL_NAME.to_string(), APP_NAME.to_string())]);
    labels.insert(
        "pod-security.kubernetes.io/enforce".to_string(),
        "privileged".to_string(),
    );
    for mode in ["audit", "warn"] {
        labels.insert(
            format!("pod-security.kubernetes.io/{mode}"),
            "restricted".to_string(),
        );
    }
    Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            labels: Some(labels),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Build a role's ServiceAccount.
#[must_use]
pub fn build_service_account(role: &InstallRole, opts: &InstallOptions) -> ServiceAccount {
    ServiceAccount {
        metadata: ObjectMeta {
            name: Some(role.name().to_string()),
            namespace: Some(opts.namespace.clone()),
            labels: Some(labels(role)),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Build a role's ClusterRoleBinding.
#[must_use]
pub fn build_cluster_role_binding(role: &InstallRole, opts: &InstallOptions) -> ClusterRoleBinding {
    ClusterRoleBinding {
        metadata: ObjectMeta {
            name: Some(role.name().to_string()),
            labels: Some(labels(role)),
            ..Default::default()
        },
        role_ref: RoleRef {
            api_group: Some("rbac.authorization.k8s.io".to_string()),
            kind: "ClusterRole".to_string(),
            name: role.name().to_string(),
        },
        subjects: Some(vec![Subject {
            kind: "ServiceAccount".to_string(),
            name: role.name().to_string(),
            namespace: Some(opts.namespace.clone()),
            ..Default::default()
        }]),
    }
}

/// Build the namespaced Role a standalone provider reads credentials through.
///
/// `get` on every Secret and ConfigMap **in the install namespace** — no
/// `resourceNames`, because the standalone provider serves every Provider in
/// that namespace and cannot know their `credentialsRef` names at install
/// time. Keeping this namespaced is the whole point (CHAIN-002): the blast
/// radius of a compromised standalone provider is one namespace's Secrets,
/// not the cluster's.
#[must_use]
pub fn build_namespaced_role(role: &InstallRole, opts: &InstallOptions) -> Role {
    Role {
        metadata: ObjectMeta {
            name: Some(role.name().to_string()),
            namespace: Some(opts.namespace.clone()),
            labels: Some(labels(role)),
            ..Default::default()
        },
        rules: Some(vec![
            PolicyRule {
                api_groups: Some(vec![String::new()]),
                resources: Some(vec!["secrets".to_string()]),
                verbs: vec!["get".to_string()],
                ..Default::default()
            },
            PolicyRule {
                api_groups: Some(vec![String::new()]),
                resources: Some(vec!["configmaps".to_string()]),
                verbs: vec!["get".to_string()],
                ..Default::default()
            },
        ]),
    }
}

/// Build the RoleBinding tying [`build_namespaced_role`] to the ServiceAccount.
#[must_use]
pub fn build_namespaced_role_binding(role: &InstallRole, opts: &InstallOptions) -> RoleBinding {
    RoleBinding {
        metadata: ObjectMeta {
            name: Some(role.name().to_string()),
            namespace: Some(opts.namespace.clone()),
            labels: Some(labels(role)),
            ..Default::default()
        },
        role_ref: RoleRef {
            api_group: Some("rbac.authorization.k8s.io".to_string()),
            kind: "Role".to_string(),
            name: role.name().to_string(),
        },
        subjects: Some(vec![Subject {
            kind: "ServiceAccount".to_string(),
            name: role.name().to_string(),
            namespace: Some(opts.namespace.clone()),
            ..Default::default()
        }]),
    }
}

/// Build a role's ConfigMap of log and port settings.
#[must_use]
pub fn build_config_map(role: &InstallRole, opts: &InstallOptions) -> ConfigMap {
    ConfigMap {
        metadata: ObjectMeta {
            name: Some(format!("{}-config", role.name())),
            namespace: Some(opts.namespace.clone()),
            labels: Some(labels(role)),
            ..Default::default()
        },
        data: Some(BTreeMap::from([
            (
                "RUST_LOG".to_string(),
                "info,kube=warn,hyper=warn,tower=warn".to_string(),
            ),
            ("RUST_LOG_FORMAT".to_string(), "json".to_string()),
            (
                "BANLIEUE_METRICS_PORT".to_string(),
                METRICS_PORT.to_string(),
            ),
            ("BANLIEUE_HEALTH_PORT".to_string(), HEALTH_PORT.to_string()),
        ])),
        ..Default::default()
    }
}

/// Build a role's Deployment.
#[must_use]
pub fn build_deployment(role: &InstallRole, opts: &InstallOptions) -> Deployment {
    let role_labels = labels(role);
    let selector = BTreeMap::from([
        (LABEL_NAME.to_string(), APP_NAME.to_string()),
        (LABEL_COMPONENT.to_string(), role.component()),
    ]);

    Deployment {
        metadata: ObjectMeta {
            name: Some(role.name().to_string()),
            namespace: Some(opts.namespace.clone()),
            labels: Some(role_labels.clone()),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(1),
            selector: LabelSelector {
                match_labels: Some(selector),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(role_labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    service_account_name: Some(role.name().to_string()),
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
                    containers: vec![build_container(role, opts)],
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Build a role's container.
fn build_container(role: &InstallRole, opts: &InstallOptions) -> Container {
    // A standalone provider's credential reads stop at the install namespace
    // (CHAIN-002), so its watch must too — otherwise Providers in other
    // namespaces would reconcile and then fail on the credentials read with a
    // 403, instead of simply being out of scope.
    let args = match role {
        InstallRole::Provider(_) => {
            let mut args = role.args();
            args.push("--namespace".to_string());
            args.push(opts.namespace.clone());
            args
        }
        _ => role.args_with(opts),
    };
    Container {
        name: role.component(),
        image: Some(resolve_image(opts)),
        image_pull_policy: Some("IfNotPresent".to_string()),
        args: Some(args),
        env: Some(
            ["metadata.name", "metadata.namespace"]
                .into_iter()
                .zip(["POD_NAME", "POD_NAMESPACE"])
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
                .collect(),
        ),
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
        )),
        readiness_probe: Some(http_probe(
            "/readyz",
            READINESS_INITIAL_DELAY_SECS,
            READINESS_PERIOD_SECS,
        )),
        resources: Some(ResourceRequirements {
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
        }),
        security_context: Some(SecurityContext {
            allow_privilege_escalation: Some(false),
            read_only_root_filesystem: Some(true),
            capabilities: Some(k8s_openapi::api::core::v1::Capabilities {
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

/// Build an HTTP probe against the health port.
fn http_probe(path: &str, initial_delay: i32, period: i32) -> Probe {
    Probe {
        http_get: Some(HTTPGetAction {
            path: Some(path.to_string()),
            port: IntOrString::String(HEALTH_PORT_NAME.to_string()),
            ..Default::default()
        }),
        initial_delay_seconds: Some(initial_delay),
        period_seconds: Some(period),
        failure_threshold: Some(PROBE_FAILURE_THRESHOLD),
        ..Default::default()
    }
}

/// Build the seeded ProviderClass for a backend.
///
/// Named after the backend, matching what `Provider.spec.providerClassRef`
/// conventionally points at.
#[must_use]
pub fn build_provider_class(backend: &str, opts: &InstallOptions) -> ProviderClass {
    let (repository, tag) = split_image(&resolve_image(opts));
    let mut class = ProviderClass::new(
        backend,
        ProviderClassSpec {
            backend: backend.to_string(),
            image: ProviderImage {
                repository,
                tag,
                digest: opts.image_digest.clone(),
                pull_policy: Some(ImagePullPolicy::IfNotPresent),
                pull_secrets: Vec::new(),
            },
            workload_namespace: None,
            replicas: None,
            resources: None,
            node_selector: BTreeMap::new(),
            tolerations: Vec::new(),
            logging: Default::default(),
            additional_rules: backend_additional_rules(backend),
            paused: false,
        },
    );
    class.metadata.labels = Some(BTreeMap::from([
        (LABEL_NAME.to_string(), APP_NAME.to_string()),
        (
            LABEL_MANAGED_BY.to_string(),
            MANAGED_BY_BOOTSTRAP.to_string(),
        ),
    ]));
    class
}

/// Extra per-instance Role rules a backend needs beyond the common set.
///
/// This Role is namespaced to the Provider's own namespace (typically
/// `banlieue-system`) — kept per-backend rather than granted to every
/// provider, since the ability to create Jobs is the ability to run
/// arbitrary pods with the provider's own ServiceAccount.
///
/// libvirt's import Jobs run guest-image transfer (ADR-0011) in the same
/// namespace as the Provider, so this per-instance Role is how it reaches
/// them. **vSphere's per-zone import Jobs (ADR-0020) live in a *different*
/// namespace (`banlieue-imagebuild`, ADR-0016 isolation), so a namespaced
/// Role here could never reach them regardless of what it grants** — its
/// Jobs access comes entirely from the cluster-wide `ClusterRole`
/// (`deploy/provider-vsphere/rbac/clusterrole.yaml`, `list`/`watch` added
/// for the event-driven `VMImage` reconciler's `.watches()` on the Job).
/// vSphere is therefore deliberately excluded here, not because it needs no
/// Jobs access at all.
///
/// libvirt's verbs mirror `deploy/provider-libvirt/rbac/clusterrole.yaml`:
/// reads are by the deterministic name from `import_job_name`, so no
/// list/watch, and finished Jobs are reaped by `ttlSecondsAfterFinished`, so
/// no delete.
#[must_use]
pub fn backend_additional_rules(backend: &str) -> Vec<PolicyRule> {
    match backend {
        "libvirt" => vec![PolicyRule {
            api_groups: Some(vec!["batch".to_string()]),
            resources: Some(vec!["jobs".to_string()]),
            verbs: ["get", "create", "patch"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ..Default::default()
        }],
        _ => Vec::new(),
    }
}

/// Split an image reference into repository and tag.
///
/// Splits on the last `:` that is not part of a registry `host:port`, i.e. the
/// last colon after the final `/`.
fn split_image(image: &str) -> (String, String) {
    let tag_start = image.rfind('/').map_or(0, |slash| slash + 1);
    match image[tag_start..].rfind(':') {
        Some(colon) => (
            image[..tag_start + colon].to_string(),
            image[tag_start + colon + 1..].to_string(),
        ),
        None => (image.to_string(), DEFAULT_IMAGE_TAG.to_string()),
    }
}

/// Run `banlieue bootstrap`.
///
/// # Arguments
/// * `cli` - the parsed subcommand.
/// * `backends` - backends compiled into this binary. A slim build cannot offer
///   to install a backend it does not contain.
///
/// # Errors
/// Returns an error if a requested backend is not compiled in, if manifests
/// fail to serialize, or if the cluster rejects an apply.
pub async fn run(cli: Cli, backends: &[&str]) -> Result<()> {
    let (manifests, dry_run) = match &cli.target {
        BootstrapTarget::Operator {
            common,
            skip_provider_classes,
        } => {
            let (opts, dry_run) = common.split();
            (
                build_operator_install(&opts, backends, *skip_provider_classes)?,
                dry_run,
            )
        }
        BootstrapTarget::Provider { backend, common } => {
            if !backends.contains(&backend.as_str()) {
                anyhow::bail!(
                    "backend {backend:?} is not compiled into this binary (available: {})",
                    backends.join(", ")
                );
            }
            let (opts, dry_run) = common.split();
            (
                build_role_install(&InstallRole::Provider(backend.clone()), &opts)?,
                dry_run,
            )
        }
        BootstrapTarget::Imagebuilder { common } => {
            let (opts, dry_run) = common.split();
            (
                build_role_install(&InstallRole::Imagebuilder, &opts)?,
                dry_run,
            )
        }
    };

    if dry_run {
        // Never contacts a cluster, so this works with no kubeconfig at all.
        print!("{}", manifests.to_yaml()?);
        return Ok(());
    }

    apply(&manifests).await
}

/// Server-side apply every object, in dependency order.
async fn apply(manifests: &InstallManifests) -> Result<()> {
    let client = build_client().await.context("constructing kube client")?;

    if manifests.creates_namespace {
        let api: Api<Namespace> = Api::all(client.clone());
        server_side_apply(&api, FIELD_MANAGER, &manifests.namespace).await?;
        info!(namespace = ?manifests.namespace.metadata.name, "namespace applied");
    }

    // Applied unconditionally when present: the imagebuilder cannot reconcile
    // without it, and kairos' build pods cannot be admitted anywhere else
    // (ADR-0016).
    if let Some(ns) = &manifests.imagebuild_namespace {
        let api: Api<Namespace> = Api::all(client.clone());
        server_side_apply(&api, FIELD_MANAGER, ns).await?;
        info!(namespace = ?ns.metadata.name, "imagebuild namespace applied");
    }

    let crds: Api<CustomResourceDefinition> = Api::all(client.clone());
    for crd in &manifests.crds {
        server_side_apply(&crds, FIELD_MANAGER, crd).await?;
    }
    if !manifests.crds.is_empty() {
        info!(count = manifests.crds.len(), "CRDs applied");
    }

    let cluster_roles: Api<ClusterRole> = Api::all(client.clone());
    for cluster_role in &manifests.cluster_roles {
        server_side_apply(&cluster_roles, FIELD_MANAGER, cluster_role).await?;
    }

    let cluster_role_bindings: Api<ClusterRoleBinding> = Api::all(client.clone());
    for binding in &manifests.cluster_role_bindings {
        server_side_apply(&cluster_role_bindings, FIELD_MANAGER, binding).await?;
    }

    for sa in &manifests.service_accounts {
        let api: Api<ServiceAccount> = namespaced(&client, sa.metadata.namespace.as_deref());
        server_side_apply(&api, FIELD_MANAGER, sa).await?;
    }

    // Namespaced Roles and RoleBindings. These carry the standalone provider's
    // Secret access (security review 2026-07-31, CHAIN-002) — the shared
    // ClusterRole deliberately has none, because it is bound cluster-wide.
    //
    // `to_yaml` already emitted these, so `--dry-run` looked correct while a
    // direct apply silently skipped them and the provider came up with no
    // Secret access at all. Applied before the Deployment, so the pod never
    // starts without its permissions.
    for role in &manifests.roles {
        let api: Api<Role> = namespaced(&client, role.metadata.namespace.as_deref());
        server_side_apply(&api, FIELD_MANAGER, role).await?;
    }

    for binding in &manifests.role_bindings {
        let api: Api<RoleBinding> = namespaced(&client, binding.metadata.namespace.as_deref());
        server_side_apply(&api, FIELD_MANAGER, binding).await?;
    }

    for cm in &manifests.config_maps {
        let api: Api<ConfigMap> = namespaced(&client, cm.metadata.namespace.as_deref());
        server_side_apply(&api, FIELD_MANAGER, cm).await?;
    }

    for deployment in &manifests.deployments {
        let api: Api<Deployment> = namespaced(&client, deployment.metadata.namespace.as_deref());
        server_side_apply(&api, FIELD_MANAGER, deployment).await?;
        info!(name = ?deployment.metadata.name, "deployment applied");
    }

    let provider_classes: Api<ProviderClass> = Api::all(client.clone());
    for class in &manifests.provider_classes {
        server_side_apply(&provider_classes, FIELD_MANAGER, class).await?;
        info!(name = ?class.metadata.name, "provider class applied");
    }

    info!("bootstrap complete");
    Ok(())
}

/// Namespaced API handle, defaulting to [`DEFAULT_NAMESPACE`] when an object
/// somehow carries no namespace.
fn namespaced<K>(client: &kube::Client, namespace: Option<&str>) -> Api<K>
where
    K: kube::Resource<Scope = k8s_openapi::NamespaceResourceScope, DynamicType = ()>,
{
    Api::namespaced(client.clone(), namespace.unwrap_or(DEFAULT_NAMESPACE))
}

#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod bootstrap_tests;
