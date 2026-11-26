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
//!   `crdgen_support::all_crds()` list `crdgen` uses.
//! - **ClusterRoles** are `include_str!`-embedded from `deploy/*/rbac/` (and
//!   the Cluster API aggregate role from `deploy/capi/`, ADR-0096), so the
//!   shipped manifests stay the single source of truth and a GitOps install
//!   grants exactly the same permissions as a bootstrap install. Moving one of
//!   those files breaks the build — which is the point.
//! - Everything else (Namespace, ServiceAccount, bindings, ConfigMap,
//!   Deployment, ProviderClass) is built here, because it is parameterised by
//!   namespace, image tag, and registry.

use anyhow::{Context as _, Result};
use banlieue_api::banlieue::{
    ImagePullPolicy, ProviderClass, ProviderClassSpec, ProviderDeployment, ProviderImage,
};
use banlieue_api::crdgen_support::all_crds;
use banlieue_provider_sdk::client::build_client;
use banlieue_provider_sdk::ssa::server_side_apply;
use clap::{Args, Subcommand};
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    ConfigMap, Container, EnvVar, Namespace, PodSpec, PodTemplateSpec, ServiceAccount,
};
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding, PolicyRule, Role, RoleBinding};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use kube::Api;
use std::collections::BTreeMap;
use tracing::{info, warn};

use crate::naming::{APP_NAME, LABEL_COMPONENT, LABEL_MANAGED_BY, LABEL_NAME};
use crate::pod::{
    DEFAULT_CPU_REQUEST, DEFAULT_MEMORY_REQUEST, HEALTH_PORT, LIVENESS_INITIAL_DELAY_SECS,
    LIVENESS_PATH, LIVENESS_PERIOD_SECS, METRICS_PORT, READINESS_INITIAL_DELAY_SECS,
    READINESS_PATH, READINESS_PERIOD_SECS, TERMINATION_GRACE_PERIOD_SECS, container_ports,
    container_security_context, downward_api_env, http_probe, pod_security_context, resources,
};
use crate::rbac::{named_rule, namespace_with_psa, role_ref, rule, sa_subject};

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
pub const DEFAULT_IMAGEBUILD_NAMESPACE: &str =
    banlieue_provider_sdk::defaults::DEFAULT_BUILD_NAMESPACE;

/// Image repository, without a tag.
pub const IMAGE_BASE: &str = "ghcr.io/firestoned/banlieue";

/// Image name appended to a `--registry` override.
const IMAGE_NAME: &str = "banlieue";

/// The pre-lay image's repository, without a tag (ADR-0113, amendment
/// 2026-11-04): what `banlieue-imagebuilder`'s pre-lay Job runs.
pub const PRELAY_IMAGE_BASE: &str = "ghcr.io/firestoned/banlieue-prelay";

/// Pre-lay image name appended to a `--registry` override.
const PRELAY_IMAGE_NAME: &str = "banlieue-prelay";

/// Default image tag: the binary's own version, so bootstrap installs the image
/// matching the binary that ran it.
pub const DEFAULT_IMAGE_TAG: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// Field manager for everything bootstrap writes. Distinct from the operator's,
/// so an object's bootstrap-set fields stay distinguishable from the ones the
/// operator manages later.
pub const FIELD_MANAGER: &str = "banlieue.io/bootstrap";

/// Value of the managed-by label on bootstrap-installed objects.
const MANAGED_BY_BOOTSTRAP: &str = "banlieue-bootstrap";

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
const PROVIDER_PROXMOX_CLUSTER_ROLE: &str =
    include_str!("../../../deploy/provider-proxmox/rbac/clusterrole.yaml");
/// The Azure provider (ADR-0164). Grants no Secret access: its only cloud
/// credential is a projected ServiceAccount token (ADR-0159 Decision 1).
const PROVIDER_AZURE_CLUSTER_ROLE: &str =
    include_str!("../../../deploy/provider-azure/rbac/clusterrole.yaml");
const PROVIDER_CLOUD_HYPERVISOR_CLUSTER_ROLE: &str =
    include_str!("../../../deploy/provider-cloud-hypervisor/rbac/clusterrole.yaml");
/// The opt-in shared-serving ClusterRole of the Cloud Hypervisor provider
/// (ADR-0134). The operator binds it only for a Provider with a selector.
const PROVIDER_CLOUD_HYPERVISOR_SHARED_CLUSTER_ROLE: &str =
    include_str!("../../../deploy/provider-cloud-hypervisor/rbac/clusterrole-shared.yaml");
const CAPI_AGGREGATE_CLUSTER_ROLE_YAML: &str =
    include_str!("../../../deploy/capi/clusterrole-aggregate.yaml");
/// The controller's namespaced user-data Role and its binding (ADR-0025,
/// ADR-0038). Embedded like the ClusterRoles, so a bootstrap install grants
/// exactly what the GitOps manifests grant. ADR-0042's admission policy is
/// what makes this un-scoped `get` safe.
const CONTROLLER_USERDATA_ROLE: &str = include_str!("../../../deploy/controller/rbac/role.yaml");
const CONTROLLER_USERDATA_ROLE_BINDING: &str =
    include_str!("../../../deploy/controller/rbac/rolebinding.yaml");

/// Name of the ClusterRole aggregated into Cluster API's manager (ADR-0096).
pub const CAPI_AGGREGATE_CLUSTER_ROLE: &str = "banlieue-capi-infrastructure";

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

        /// Pin the pre-lay image (`banlieue-prelay`, ADR-0113) to this
        /// digest, e.g. `sha256:0f756fa0…`. Its own flag because it is its
        /// own image: `--image-digest` pins the banlieue image. Recommended
        /// for the same reason (ADR-0103), and more so for a privileged Job.
        #[arg(long, value_parser = parse_image_digest)]
        prelay_image_digest: Option<String>,

        /// The Azure guest helper bundle (`banlieue-azure-guest`, ADR-0171)
        /// every image with an `azure` source carries, as
        /// `<registry>/<repository>@sha256:<digest>`. Only a digest-pinned
        /// reference is accepted: the bundle is unpacked into the image's
        /// root filesystem. Without it the imagebuilder refuses Azure builds
        /// (`AzureGuestImageUnset`).
        #[arg(long, value_parser = parse_pinned_image)]
        azure_guest_image: Option<String>,
    },

    /// Issue a Cloud Hypervisor host its first credential: a kubeconfig and
    /// a bound token for the ServiceAccount the operator created for that
    /// host's External Provider (ADR-0060 Decision 5). The provider renews
    /// the token itself from then on.
    CloudHypervisorHost(crate::host_credential::HostCredentialArgs),
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

    /// Pin every banlieue image this install writes to this digest, e.g.
    /// `sha256:0f756fa0…`: the controller, operator and imagebuilder
    /// Deployments and the seeded `ProviderClass`es (ADR-0103). The tag is
    /// kept as documentation of intent; the digest is what gets pulled.
    ///
    /// Strongly recommended for anything but throwaway clusters: a Deployment
    /// built from a mutable tag has an unchanging spec, so pushing a new image
    /// triggers no rollout and pods keep running old layers while looking
    /// perfectly healthy.
    #[arg(long, value_parser = parse_image_digest)]
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

    /// Serve the `Provider`s in every namespace whose labels match this label
    /// selector (for example `banlieue.io/tenant=true`). Passed to the
    /// operator as `--watch-namespace-selector` (ADR-0131); without it the
    /// operator serves only the install namespace. Ignored for other roles.
    #[arg(long, value_name = "SELECTOR")]
    pub watch_namespace_selector: Option<String>,

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
                prelay_image_digest: None,
                azure_guest_image: None,
                watch_namespace_selector: self.watch_namespace_selector.clone(),
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
    /// `--watch-namespace-selector` for the operator (ADR-0131). `None`
    /// leaves the operator on its default scope, its own namespace.
    pub watch_namespace_selector: Option<String>,
    /// Image digest to pin every installed banlieue image to, e.g.
    /// `sha256:0f75…` (ADR-0103).
    ///
    /// Without one the class references a tag, and a Deployment built from a
    /// tag has a spec that never changes when a new image is pushed — so
    /// nothing rolls, and `imagePullPolicy: Always` does not help because it
    /// only applies when a pod is created.
    pub image_digest: Option<String>,
    /// Digest to pin the pre-lay image to (`bootstrap imagebuilder
    /// --prelay-image-digest`, ADR-0113). Never taken from
    /// [`InstallOptions::image_digest`]: different image, different bytes.
    pub prelay_image_digest: Option<String>,
    /// Digest-pinned Azure guest helper bundle for the imagebuilder
    /// (`bootstrap imagebuilder --azure-guest-image`, ADR-0171). `None`
    /// omits the flag, and Azure builds are refused.
    pub azure_guest_image: Option<String>,
}

impl InstallOptions {
    /// These options with the Azure guest helper bundle set to `image` (or
    /// unset for `None`).
    #[must_use]
    pub fn with_azure_guest_image(self, image: Option<String>) -> Self {
        Self {
            azure_guest_image: image,
            ..self
        }
    }

    /// These options with the pre-lay image pinned to `digest` (or unpinned
    /// for `None`).
    #[must_use]
    pub fn with_prelay_digest(self, digest: Option<String>) -> Self {
        Self {
            prelay_image_digest: digest,
            ..self
        }
    }
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
                "proxmox" => "banlieue-provider-proxmox",
                "azure" => "banlieue-provider-azure",
                "cloud-hypervisor" => "banlieue-provider-cloud-hypervisor",
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
        if let (Self::Operator, Some(selector)) = (self, &opts.watch_namespace_selector) {
            args.push("--watch-namespace-selector".to_string());
            args.push(selector.clone());
        }
        // The version being installed, not the binary's own default, and the
        // mirror under `--registry` (ADR-0113, amendment 2026-11-04).
        if !matches!(self, Self::Imagebuilder) {
            return args;
        }
        args.push("--prelay-image".to_string());
        args.push(prelay_install_image(opts));
        // The measure, push and publish Jobs run the image being installed,
        // never the binary's release-tag default, which can predate the Job
        // subcommands this imagebuilder asks for.
        args.push("--push-image".to_string());
        args.push(install_image(opts));
        if let Some(image) = &opts.azure_guest_image {
            args.push("--azure-guest-image".to_string());
            args.push(image.clone());
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
                "proxmox" => PROVIDER_PROXMOX_CLUSTER_ROLE,
                "azure" => PROVIDER_AZURE_CLUSTER_ROLE,
                "cloud-hypervisor" => PROVIDER_CLOUD_HYPERVISOR_CLUSTER_ROLE,
                other => anyhow::bail!("no embedded ClusterRole for backend {other:?}"),
            },
        };
        serde_saphyr::from_str(yaml)
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
    out.push_str(&banlieue_api::yaml::to_string(object)?);
    Ok(())
}

/// Tagged image reference for an install, without any digest.
///
/// The seeded `ProviderClass`es split this into repository and tag and carry
/// the digest in their own field; workload containers use [`install_image`].
#[must_use]
pub fn resolve_image(opts: &InstallOptions) -> String {
    match opts.registry.as_deref() {
        Some(registry) => format!("{registry}/{IMAGE_NAME}:{}", opts.version),
        None => format!("{IMAGE_BASE}:{}", opts.version),
    }
}

/// Image reference a bootstrap-written container pulls:
/// `<repository>:<tag>@<digest>` when `--image-digest` is set, otherwise
/// [`resolve_image`]'s tag (ADR-0103).
///
/// The same shape [`ProviderImage::reference`] gives a pinned class, so a
/// digest-pinned install runs every banlieue role from the same bytes.
#[must_use]
pub fn install_image(opts: &InstallOptions) -> String {
    let tagged = resolve_image(opts);
    match opts.image_digest.as_deref() {
        Some(digest) => format!("{tagged}@{digest}"),
        None => tagged,
    }
}

/// The pre-lay image an imagebuilder install runs:
/// `<repository>:<tag>[@<digest>]`, with the repository under `--registry`
/// when one is set and the digest from `--prelay-image-digest` only
/// (ADR-0113, amendment 2026-11-04).
#[must_use]
pub fn prelay_install_image(opts: &InstallOptions) -> String {
    let tagged = match opts.registry.as_deref() {
        Some(registry) => format!("{registry}/{PRELAY_IMAGE_NAME}:{}", opts.version),
        None => format!("{PRELAY_IMAGE_BASE}:{}", opts.version),
    };
    match opts.prelay_image_digest.as_deref() {
        Some(digest) => format!("{tagged}@{digest}"),
        None => tagged,
    }
}

/// `--image-digest` value parser: `sha256:` and 64 lowercase hex digits.
///
/// A malformed digest would otherwise pass the install and surface later as
/// `ErrImagePull` on every pod.
///
/// # Errors
/// A message naming the expected form when `value` is not a sha256 digest.
pub fn parse_image_digest(value: &str) -> Result<String, String> {
    banlieue_oci::reference::require_digest(value)
        .map(|()| value.to_string())
        .map_err(|_| format!("{value:?} is not a digest: expected sha256:<64 lowercase hex>"))
}

/// `--azure-guest-image` value parser: a registry-qualified reference
/// pinned by a sha256 digest (`<registry>/<repository>@sha256:<hex>`).
///
/// # Errors
/// A message naming the expected form when `value` is not such a reference.
pub fn parse_pinned_image(value: &str) -> Result<String, String> {
    let expected = "expected <registry>/<repository>@sha256:<64 lowercase hex>";
    let reference =
        banlieue_oci::reference::Reference::parse(value).map_err(|e| format!("{e}: {expected}"))?;
    if reference.digest.is_none() {
        return Err(format!("{value:?} is not pinned by digest: {expected}"));
    }
    Ok(value.to_string())
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
        // The one list `crdgen` and the API reference also read: a second
        // hand-written list here once missed a CRD until CI caught it.
        crds: all_crds(),
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

    // The opt-in shared-serving ClusterRole (ADR-0134). Installed whenever the
    // backend is, and bound by the operator only for a Provider with a selector.
    if backends.contains(&"cloud-hypervisor") {
        manifests.cluster_roles.push(
            serde_saphyr::from_str(PROVIDER_CLOUD_HYPERVISOR_SHARED_CLUSTER_ROLE)
                .context("parsing embedded shared ClusterRole for cloud-hypervisor")?,
        );
    }

    // What Cluster API's manager may do on the infrastructure kinds
    // (ADR-0096). No workload is bound to it: CAPI's own manager role
    // aggregates it by label. This install is also the clusterctl components
    // file, so `clusterctl init --infrastructure banlieue` gets it from here.
    manifests
        .cluster_roles
        .push(build_capi_aggregate_cluster_role()?);

    if !skip_provider_classes {
        manifests.provider_classes = backends
            .iter()
            .map(|backend| build_provider_class(backend, opts))
            .collect();
    }

    Ok(manifests)
}

/// Parse the embedded ClusterRole that Cluster API's manager aggregates
/// (`deploy/capi/clusterrole-aggregate.yaml`, ADR-0096 Decision 4).
///
/// It grants get, list, watch, create, update, patch and delete on the
/// `infrastructure.banlieue.io` machine, machine-template and cluster kinds
/// and their status, and nothing else.
///
/// # Errors
/// Returns an error if the embedded YAML is not a valid ClusterRole, which can
/// only happen if the `deploy/capi/` manifest was edited into invalid YAML.
pub fn build_capi_aggregate_cluster_role() -> Result<ClusterRole> {
    serde_saphyr::from_str(CAPI_AGGREGATE_CLUSTER_ROLE_YAML)
        .with_context(|| format!("parsing embedded ClusterRole {CAPI_AGGREGATE_CLUSTER_ROLE}"))
}

/// The controller's user-data `Role` and `RoleBinding`, from
/// `deploy/controller/rbac/`, moved into the install namespace.
///
/// The controller resolves `VirtualMachine.spec.userData` Secrets and
/// ConfigMaps itself (ADR-0025, ADR-0038); without this Role every
/// `VirtualMachine` carrying user-data fails to schedule with a 403.
///
/// # Errors
/// Returns an error if either embedded manifest is not valid YAML for its
/// kind, which can only happen if `deploy/controller/rbac/` was edited into
/// invalid YAML.
pub fn build_controller_userdata_role(opts: &InstallOptions) -> Result<(Role, RoleBinding)> {
    let mut role: Role = serde_saphyr::from_str(CONTROLLER_USERDATA_ROLE)
        .context("parsing embedded deploy/controller/rbac/role.yaml")?;
    let mut binding: RoleBinding = serde_saphyr::from_str(CONTROLLER_USERDATA_ROLE_BINDING)
        .context("parsing embedded deploy/controller/rbac/rolebinding.yaml")?;
    role.metadata.namespace = Some(opts.namespace.clone());
    binding.metadata.namespace = Some(opts.namespace.clone());
    for subject in binding.subjects.iter_mut().flatten() {
        subject.namespace = Some(opts.namespace.clone());
    }
    Ok((role, binding))
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
        // exactly that Provider and its credentials (ADR-0016 §4). Beside it,
        // the Azure image publish Job's identity (ADR-0162 Decision 1): no
        // RBAC and no API server token, only the projected token its
        // federated credential trusts.
        service_accounts: if matches!(role, InstallRole::Imagebuilder) {
            build_job_service_accounts()
        } else {
            Vec::new()
        },
        ..Default::default()
    };
    add_role(&mut manifests, role, opts)?;
    Ok(manifests)
}

/// The ServiceAccount the Azure image publish Job runs as, in the build
/// namespace (ADR-0162 Decision 1). Mirrors
/// `deploy/imagebuilder/rbac/serviceaccount-azure-publisher.yaml`.
pub const AZURE_PUBLISHER_SERVICE_ACCOUNT: &str = "banlieue-azure-publisher";

/// The identities Jobs in the build namespace run as: the import identity
/// (ADR-0016 §4) and the Azure publisher (ADR-0162 Decision 1). The publisher
/// mounts no API server token: it never calls the API server.
#[must_use]
pub fn build_job_service_accounts() -> Vec<ServiceAccount> {
    let account = |name: &str, automount: Option<bool>| ServiceAccount {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(DEFAULT_IMAGEBUILD_NAMESPACE.to_string()),
            ..Default::default()
        },
        automount_service_account_token: automount,
        ..Default::default()
    };
    vec![
        account(crate::workload::IMPORT_SERVICE_ACCOUNT, None),
        account(AZURE_PUBLISHER_SERVICE_ACCOUNT, Some(false)),
    ]
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
    // The operator grants External providers `serviceaccounts/token` on
    // their own ServiceAccount (ADR-0060 Decision 5), so it must hold it —
    // but only in the install namespace, never cluster-wide.
    if matches!(role, InstallRole::Operator) {
        manifests.roles.push(build_operator_token_role(opts));
        manifests
            .role_bindings
            .push(build_operator_token_role_binding(opts));
    }
    if matches!(role, InstallRole::Controller) {
        let (userdata_role, userdata_binding) = build_controller_userdata_role(opts)?;
        manifests.roles.push(userdata_role);
        manifests.role_bindings.push(userdata_binding);
    }
    if matches!(role, InstallRole::Provider(_)) {
        manifests.roles.push(build_namespaced_role(role, opts));
        manifests
            .role_bindings
            .push(build_namespaced_role_binding(role, opts));
        // A backend that runs Jobs holds its Job verbs in the build namespace
        // only, the same grant the operator renders per Provider: the shared
        // ClusterRole no longer carries `jobs`, which bound cluster-wide was
        // pod create in every namespace.
        if let Some((jobs_role, jobs_binding)) = build_provider_jobs_role(role, opts) {
            manifests.roles.push(jobs_role);
            manifests.role_bindings.push(jobs_binding);
        }
    }
    // The imagebuilder merges VMImage.spec.cloudConfigs Secrets (ADR-0037),
    // which is Secret *content* — but only ever in the build namespace. That
    // grant is a namespaced Role there, never a rule in the shared ClusterRole,
    // which has no namespace scope at all (ADR-0041). Without this the CLI
    // install path would diverge from `deploy/imagebuilder/rbac/role.yaml` and
    // 403 on the first VMImage carrying cloudConfigs — the same GitOps/CLI
    // parity gap that hid the missing banlieue-import ServiceAccount until it
    // failed live.
    if matches!(role, InstallRole::Imagebuilder) {
        manifests.roles.push(build_cloud_config_role(role));
        manifests
            .role_bindings
            .push(build_cloud_config_role_binding(role, opts));
    }
    // Last, so each role's own namespaced Role stays first. ADR-0102: each role's leader election reaches its own Lease only.
    if elects_through_own_lease(role) {
        manifests.roles.push(build_leader_election_role(role, opts));
        manifests
            .role_bindings
            .push(build_leader_election_role_binding(role, opts));
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
    namespace_with_psa(name, "restricted")
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
    namespace_with_psa(name, "privileged")
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
        role_ref: role_ref("ClusterRole", role.name()),
        subjects: Some(vec![sa_subject(role.name(), &opts.namespace)]),
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
            rule("", "secrets", &["get"]),
            rule("", "configmaps", &["get"]),
        ]),
    }
}

/// Suffix of a standalone provider's Job Role and RoleBinding.
const PROVIDER_JOBS_ROLE_SUFFIX: &str = "-jobs";

/// A standalone provider's Job Role and its RoleBinding, in the build
/// namespace, for a backend that runs Jobs (vSphere, libvirt, Azure); `None`
/// for any other role or backend.
///
/// The rules are [`crate::workload::job_rules`], the ones the operator renders
/// for a Provider of the same backend, so the two install paths grant the same
/// verbs. The binding sits in the build namespace and names the provider's
/// ServiceAccount in the install namespace. `banlieue bootstrap imagebuilder`
/// creates the build namespace; until it exists, `apply` warns and skips
/// these two objects.
#[must_use]
pub fn build_provider_jobs_role(
    role: &InstallRole,
    opts: &InstallOptions,
) -> Option<(Role, RoleBinding)> {
    let InstallRole::Provider(backend) = role else {
        return None;
    };
    let rules = crate::workload::job_rules(backend)?;
    let name = format!("{}{PROVIDER_JOBS_ROLE_SUFFIX}", role.name());
    let metadata = ObjectMeta {
        name: Some(name.clone()),
        namespace: Some(DEFAULT_IMAGEBUILD_NAMESPACE.to_string()),
        labels: Some(labels(role)),
        ..Default::default()
    };
    Some((
        Role {
            metadata: metadata.clone(),
            rules: Some(rules),
        },
        RoleBinding {
            metadata,
            role_ref: role_ref("Role", name),
            subjects: Some(vec![sa_subject(role.name(), &opts.namespace)]),
        },
    ))
}

/// Name of the operator's namespaced token Role and its binding.
const OPERATOR_TOKEN_ROLE_NAME: &str = "banlieue-operator-external-tokens";

/// API group of `Lease`.
const COORDINATION_API_GROUP: &str = "coordination.k8s.io";

/// Suffix of each role's leader-election Role and RoleBinding.
const LEADER_ELECTION_ROLE_SUFFIX: &str = "-leader-election";

/// Whether `role` gets a namespaced Role for its own Lease (ADR-0102). Every
/// role but the operator, which must hold Lease rights cluster-wide to grant
/// them to the providers it runs, and keeps them in its ClusterRole.
#[must_use]
pub fn elects_through_own_lease(role: &InstallRole) -> bool {
    !matches!(role, InstallRole::Operator)
}

/// The leader-election Role for `role`, in the install namespace: `create`
/// on Leases (which `resourceNames` cannot narrow) as its own rule, and
/// `get`/`update`/`patch` on the role's own Lease only (ADR-0102). The Lease
/// is named after the role, each binary's default `--leader-election-id`.
#[must_use]
pub fn build_leader_election_role(role: &InstallRole, opts: &InstallOptions) -> Role {
    Role {
        metadata: ObjectMeta {
            name: Some(format!("{}{LEADER_ELECTION_ROLE_SUFFIX}", role.name())),
            namespace: Some(opts.namespace.clone()),
            labels: Some(labels(role)),
            ..Default::default()
        },
        rules: Some(vec![
            rule(COORDINATION_API_GROUP, "leases", &["create"]),
            named_rule(
                COORDINATION_API_GROUP,
                "leases",
                &["get", "update", "patch"],
                &[role.name().to_string()],
            ),
        ]),
    }
}

/// Bind [`build_leader_election_role`] to `role`'s ServiceAccount.
#[must_use]
pub fn build_leader_election_role_binding(
    role: &InstallRole,
    opts: &InstallOptions,
) -> RoleBinding {
    let name = format!("{}{LEADER_ELECTION_ROLE_SUFFIX}", role.name());
    RoleBinding {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(opts.namespace.clone()),
            labels: Some(labels(role)),
            ..Default::default()
        },
        role_ref: role_ref("Role", name),
        subjects: Some(vec![sa_subject(role.name(), &opts.namespace)]),
    }
}

/// The operator's namespaced Role holding `serviceaccounts/token` `create`,
/// which it grants each External provider on its own ServiceAccount
/// (ADR-0060 Decision 5). Namespaced on purpose: cluster-wide, it would let
/// the operator mint a token for any ServiceAccount. Mirrors
/// `deploy/operator/rbac/role.yaml`; a test keeps the two equal.
#[must_use]
pub fn build_operator_token_role(opts: &InstallOptions) -> Role {
    Role {
        metadata: ObjectMeta {
            name: Some(OPERATOR_TOKEN_ROLE_NAME.to_string()),
            namespace: Some(opts.namespace.clone()),
            labels: Some(labels(&InstallRole::Operator)),
            ..Default::default()
        },
        rules: Some(vec![rule("", "serviceaccounts/token", &["create"])]),
    }
}

/// Bind [`build_operator_token_role`] to the operator's ServiceAccount.
#[must_use]
pub fn build_operator_token_role_binding(opts: &InstallOptions) -> RoleBinding {
    RoleBinding {
        metadata: ObjectMeta {
            name: Some(OPERATOR_TOKEN_ROLE_NAME.to_string()),
            namespace: Some(opts.namespace.clone()),
            labels: Some(labels(&InstallRole::Operator)),
            ..Default::default()
        },
        role_ref: role_ref("Role", OPERATOR_TOKEN_ROLE_NAME),
        subjects: Some(vec![sa_subject(
            InstallRole::Operator.name(),
            &opts.namespace,
        )]),
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
        role_ref: role_ref("Role", role.name()),
        subjects: Some(vec![sa_subject(role.name(), &opts.namespace)]),
    }
}

/// Name shared by the imagebuilder's cloud-config `Role` and `RoleBinding`.
const CLOUD_CONFIG_ROLE_NAME: &str = "banlieue-imagebuilder-cloudconfig";

/// Build the imagebuilder's namespaced `Role` in the build namespace:
/// cloud-config Secrets (ADR-0041) and the registry push Job (ADR-0064).
/// Keeps its original name so existing installs update in place.
///
/// Lives in the build namespace — where the Secrets it grants actually are —
/// not in the install namespace where the workload runs. No `resourceNames`:
/// the names come from arbitrary `VMImage` authors and are unknowable here.
#[must_use]
pub fn build_cloud_config_role(role: &InstallRole) -> Role {
    Role {
        metadata: ObjectMeta {
            name: Some(CLOUD_CONFIG_ROLE_NAME.to_string()),
            namespace: Some(DEFAULT_IMAGEBUILD_NAMESPACE.to_string()),
            labels: Some(labels(role)),
            ..Default::default()
        },
        // get/list/watch to read the referenced cloud-configs; create+patch
        // because a server-side apply is a CREATE when the merged Secret is
        // absent and a PATCH when it exists. Never delete.
        rules: Some(vec![
            rule("", "secrets", &["get", "list", "watch", "create", "patch"]),
            // The registry push Job (ADR-0064): created by server-side
            // apply, deleted when a previous build's Job or a Job whose
            // result was lost must be replaced.
            rule("batch", "jobs", &["get", "create", "patch", "delete"]),
            // The push Job's only output is its pod's termination message.
            rule("", "pods", &["list"]),
            // The rebuild gate reads the artifacts PVC's deletionTimestamp so
            // a stale OSArtifact is never recreated over a volume that is
            // still being torn down. Read-only.
            rule("", "persistentvolumeclaims", &["get"]),
        ]),
    }
}

/// Bind [`build_cloud_config_role`] to the imagebuilder ServiceAccount.
///
/// The `RoleBinding` sits in the build namespace with the `Role` it references,
/// while its subject names the ServiceAccount in the install namespace. Placing
/// it the other way round produces a binding that silently grants nothing.
#[must_use]
pub fn build_cloud_config_role_binding(role: &InstallRole, opts: &InstallOptions) -> RoleBinding {
    RoleBinding {
        metadata: ObjectMeta {
            name: Some(CLOUD_CONFIG_ROLE_NAME.to_string()),
            namespace: Some(DEFAULT_IMAGEBUILD_NAMESPACE.to_string()),
            labels: Some(labels(role)),
            ..Default::default()
        },
        role_ref: role_ref("Role", CLOUD_CONFIG_ROLE_NAME),
        subjects: Some(vec![sa_subject(role.name(), &opts.namespace)]),
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
        spec: DeploymentSpec {
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
                    security_context: Some(pod_security_context()),
                    termination_grace_period_seconds: Some(TERMINATION_GRACE_PERIOD_SECS),
                    containers: vec![build_container(role, opts)],
                    // The projected Entra ID token, for a static Azure
                    // provider only (ADR-0159 Decision 1), as the operator
                    // mounts it for one it spawns.
                    volumes: uses_azure_workload_identity(role)
                        .then(|| vec![crate::workload::azure_token_volume()]),
                    ..Default::default()
                }),
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Whether a role is a statically installed Azure provider, which
/// authenticates with a projected ServiceAccount token (ADR-0159 Decision 1).
fn uses_azure_workload_identity(role: &InstallRole) -> bool {
    matches!(role, InstallRole::Provider(backend) if backend == crate::workload::AZURE_BACKEND)
}

/// A role's container environment: the downward API identity, plus the
/// federated token file for a static Azure provider.
fn role_env(role: &InstallRole) -> Vec<EnvVar> {
    let mut env = downward_api_env(&[
        ("POD_NAME", "metadata.name"),
        ("POD_NAMESPACE", "metadata.namespace"),
    ]);
    if uses_azure_workload_identity(role) {
        env.push(crate::workload::azure_token_env());
    }
    env
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
    // The role's narrowed Lease Role is in the install namespace, so its
    // Lease must be too; each binary defaults to `banlieue-system`
    // (ADR-0102). The operator keeps its cluster-wide Lease rights and its
    // own default.
    let mut args = args;
    if elects_through_own_lease(role) {
        args.push("--leader-election-namespace".to_string());
        args.push(opts.namespace.clone());
    }
    Container {
        name: role.component(),
        image: Some(install_image(opts)),
        image_pull_policy: Some("IfNotPresent".to_string()),
        args: Some(args),
        env: Some(role_env(role)),
        volume_mounts: uses_azure_workload_identity(role)
            .then(|| vec![crate::workload::azure_token_mount()]),
        ports: Some(container_ports()),
        liveness_probe: Some(http_probe(
            LIVENESS_PATH,
            LIVENESS_INITIAL_DELAY_SECS,
            LIVENESS_PERIOD_SECS,
            None,
        )),
        readiness_probe: Some(http_probe(
            READINESS_PATH,
            READINESS_INITIAL_DELAY_SECS,
            READINESS_PERIOD_SECS,
            None,
        )),
        resources: Some(resources(
            DEFAULT_CPU_REQUEST,
            DEFAULT_MEMORY_REQUEST,
            DEFAULT_CPU_LIMIT,
            DEFAULT_MEMORY_LIMIT,
        )),
        security_context: Some(container_security_context()),
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
    // A host-resident backend runs on the hypervisor, not in the cluster: the
    // operator gives it an identity and nothing else (ADR-0060 Decision 3).
    let external = backend == "cloud-hypervisor";
    let mut class = ProviderClass::new(
        backend,
        ProviderClassSpec {
            backend: backend.to_string(),
            image: (!external).then(|| ProviderImage {
                repository,
                tag,
                digest: opts.image_digest.clone(),
                pull_policy: Some(ImagePullPolicy::IfNotPresent),
                pull_secrets: Vec::new(),
            }),
            deployment: external.then_some(ProviderDeployment::External),
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
/// `banlieue-system`).
///
/// **No backend gets `jobs` here.** Every provider's Jobs (libvirt's import,
/// ADR-0011; vSphere's per-zone import, ADR-0020; Azure's publish, ADR-0162)
/// run in the build namespace (ADR-0016), which a Role here cannot reach. A
/// `jobs` rule here once granted libvirt Job create in the Provider's own
/// namespace, where the operator's ServiceAccount lives: a pod there could run
/// as the operator. The Job verbs are rendered as a Role in the build
/// namespace instead ([`crate::workload::job_rules`]).
#[must_use]
pub fn backend_additional_rules(backend: &str) -> Vec<PolicyRule> {
    match backend {
        // Its machines, namespaced like the Role (ADR-0062): no create, no
        // delete — the controller owns their lifecycle.
        "cloud-hypervisor" => {
            let machines = |resource: &str, verbs: &[&str]| {
                rule("infrastructure.banlieue.io", resource, verbs)
            };
            vec![
                machines(
                    "cloudhypervisormachines",
                    &["get", "list", "watch", "update", "patch"],
                ),
                machines(
                    "cloudhypervisormachines/status",
                    &["get", "update", "patch"],
                ),
                machines("cloudhypervisormachines/finalizers", &["update"]),
            ]
        }
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
    // Not an install: it issues a credential and writes files.
    if let BootstrapTarget::CloudHypervisorHost(args) = &cli.target {
        return crate::host_credential::run(args).await;
    }
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
        BootstrapTarget::Imagebuilder {
            common,
            prelay_image_digest,
            azure_guest_image,
        } => {
            let (opts, dry_run) = common.split();
            let opts = opts
                .with_prelay_digest(prelay_image_digest.clone())
                .with_azure_guest_image(azure_guest_image.clone());
            (
                build_role_install(&InstallRole::Imagebuilder, &opts)?,
                dry_run,
            )
        }
        BootstrapTarget::CloudHypervisorHost(_) => unreachable!("handled above"),
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
        tolerate_missing_build_namespace(
            server_side_apply(&api, FIELD_MANAGER, role).await,
            role.metadata.namespace.as_deref(),
            manifests,
        )?;
    }

    for binding in &manifests.role_bindings {
        let api: Api<RoleBinding> = namespaced(&client, binding.metadata.namespace.as_deref());
        tolerate_missing_build_namespace(
            server_side_apply(&api, FIELD_MANAGER, binding).await,
            binding.metadata.namespace.as_deref(),
            manifests,
        )?;
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

/// Pass an apply result through, except a 404 for an object in the build
/// namespace when this install does not create that namespace: a standalone
/// provider's Job Role ([`build_provider_jobs_role`]) installed before
/// `banlieue bootstrap imagebuilder`. Warned and skipped, so the provider
/// still installs; re-running the install after the imagebuilder's applies it.
fn tolerate_missing_build_namespace<K>(
    result: banlieue_provider_sdk::Result<K>,
    namespace: Option<&str>,
    manifests: &InstallManifests,
) -> Result<()> {
    match result {
        Ok(_) => Ok(()),
        Err(banlieue_provider_sdk::Error::Kube(e))
            if banlieue_provider_sdk::ssa::is_not_found(&e)
                && namespace == Some(DEFAULT_IMAGEBUILD_NAMESPACE)
                && manifests.imagebuild_namespace.is_none() =>
        {
            warn!(
                namespace = DEFAULT_IMAGEBUILD_NAMESPACE,
                "build namespace not found: install the imagebuilder, then re-run this \
                 install to grant the provider its Job Role"
            );
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
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
