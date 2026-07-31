// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The `ProviderClass` status reconciler.
//!
//! ADR-0012 specifies `ProviderClass.status` as holding `conditions`,
//! `observedGeneration`, and `providers` (the count of Providers referencing
//! the class). The CRD shipped those fields — and a `Providers` print column —
//! with nothing populating them, so `kubectl get providerclasses` showed a
//! permanently blank column. This closes that gap.
//!
//! The `Ready` condition answers a question worth asking: *can this class
//! actually produce a working workload?* Its most valuable check is that the
//! shared per-backend ClusterRole exists. The operator binds that role but
//! cannot create it (minting the permissions it hands out is the escalation
//! path ADR-0012 refuses), so if an install omitted it every workload of this
//! class runs with no permissions. That was bug-110, and it was invisible until
//! a provider pod started emitting 403s. As a class condition it is visible
//! immediately, before any Provider is even created.

use std::sync::Arc;

use banlieue_api::banlieue::{Provider, ProviderClass, ProviderClassSpec};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_on_error};
use k8s_openapi::api::rbac::v1::ClusterRole;
use kube::api::{ListParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::{Api, Resource, ResourceExt};
use serde_json::json;
use tracing::{debug, warn};

use crate::context::Context;
use crate::error::Result;
use crate::reconciler::provider::FIELD_MANAGER;
use crate::workload::shared_cluster_role_name;

/// Condition type published on a `ProviderClass`.
pub const CONDITION_READY: &str = "Ready";

/// Whether a class can produce a working workload, and if not, why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassReadiness {
    /// Everything a workload needs is present.
    Ready,
    /// The shared per-backend ClusterRole the operator binds does not exist, so
    /// every workload of this class would run with no permissions.
    MissingClusterRole,
    /// The image reference is not usable.
    InvalidImage,
}

impl ClassReadiness {
    /// Whether the class is usable.
    #[must_use]
    pub fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Stable identifier for the condition's `reason`.
    ///
    /// Dashboards and alert rules match on these, so they are a closed set of
    /// single identifiers rather than free-form prose.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::MissingClusterRole => "MissingClusterRole",
            Self::InvalidImage => "InvalidImage",
        }
    }

    /// Human-readable explanation, naming the exact thing to fix.
    ///
    /// Takes the backend so the ClusterRole is named outright — a message
    /// containing a `<backend>` placeholder makes the reader do the
    /// substitution, which is the difference between an actionable condition
    /// and a decorative one.
    #[must_use]
    pub fn message(self, backend: &str) -> String {
        match self {
            Self::Ready => "class is usable".to_string(),
            Self::MissingClusterRole => format!(
                "ClusterRole {} does not exist; every workload of this class would run with no \
                 permissions. Install it with `banlieue bootstrap operator`, or apply \
                 deploy/provider-{backend}/rbac/clusterrole.yaml",
                shared_cluster_role_name(backend)
            ),
            Self::InvalidImage => {
                "spec.image is incomplete — repository and tag are both required".to_string()
            }
        }
    }
}

/// Assess whether a class can produce a working workload.
///
/// A missing ClusterRole is reported ahead of a bad image: it is the more
/// actionable failure, and fixing the image alone would leave the class
/// unusable anyway.
#[must_use]
pub fn assess(spec: &ProviderClassSpec, cluster_role_present: bool) -> ClassReadiness {
    if !cluster_role_present {
        return ClassReadiness::MissingClusterRole;
    }
    if spec.image.repository.trim().is_empty() || spec.image.tag.trim().is_empty() {
        return ClassReadiness::InvalidImage;
    }
    ClassReadiness::Ready
}

/// Number of `Provider`s referencing `class_name`, across every namespace.
#[must_use]
pub fn count_providers(providers: &[Provider], class_name: &str) -> i32 {
    i32::try_from(
        providers
            .iter()
            .filter(|p| p.spec.provider_class_ref.name == class_name)
            .count(),
    )
    .unwrap_or(i32::MAX)
}

/// Reconcile one `ProviderClass` into its status.
///
/// # Errors
/// Returns an error if the API server rejects a read or the status patch.
pub async fn reconcile(class: Arc<ProviderClass>, ctx: Arc<Context>) -> Result<Action> {
    let name = class.name_any();

    let cluster_roles: Api<ClusterRole> = Api::all(ctx.client.clone());
    let role_name = shared_cluster_role_name(&class.spec.backend);
    let cluster_role_present = cluster_roles.get_opt(&role_name).await?.is_some();

    let providers: Api<Provider> = Api::all(ctx.client.clone());
    let all = providers.list(&ListParams::default()).await?;
    let referencing = count_providers(&all.items, &name);

    let assessment = assess(&class.spec, cluster_role_present);
    if !assessment.is_ready() {
        warn!(
            class = %name,
            reason = assessment.reason(),
            "ProviderClass is not usable"
        );
    }

    let classes: Api<ProviderClass> = Api::all(ctx.client.clone());
    let patch = json!({
        "apiVersion": ProviderClass::api_version(&()),
        "kind": ProviderClass::kind(&()),
        "status": {
            "providers": referencing,
            "observedGeneration": class.meta().generation,
            "conditions": [{
                "type": CONDITION_READY,
                "status": if assessment.is_ready() { "True" } else { "False" },
                "reason": assessment.reason(),
                "message": assessment.message(&class.spec.backend),
                "lastTransitionTime": k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                    k8s_openapi::jiff::Timestamp::now(),
                ),
                "observedGeneration": class.meta().generation,
            }],
        },
    });

    classes
        .patch_status(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&patch),
        )
        .await?;

    debug!(class = %name, providers = referencing, ready = assessment.is_ready(), "class status published");
    Ok(requeue_default())
}

/// Requeue policy for a failed reconcile.
#[must_use]
pub fn error_policy(
    _class: Arc<ProviderClass>,
    error: &crate::Error,
    _ctx: Arc<Context>,
) -> Action {
    warn!(error = %error, "provider class reconcile failed");
    requeue_on_error()
}

#[cfg(test)]
#[path = "providerclass_tests.rs"]
mod providerclass_tests;
