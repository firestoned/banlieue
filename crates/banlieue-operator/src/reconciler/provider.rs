// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The `Provider` → workload reconciler.
//!
//! For every `Provider`, resolve its `ProviderClass` and server-side-apply the
//! per-instance workload (ADR-0003). Deletion is handled by a finalizer rather
//! than owner references alone, because the ClusterRoleBinding is cluster-scoped
//! and a namespaced owner cannot garbage-collect it.
//!
//! This controller writes **only** `Provider.status.workload`. It never touches
//! `status.conditions` — that list is owned by the provider's own field manager,
//! and a plain list without `x-kubernetes-list-type: map` cannot be merged
//! per-entry by two managers (ADR-0012).

use std::sync::Arc;

use banlieue_api::banlieue::{
    Provider, ProviderClass, ProviderClassSpec, ProviderConnection, ProviderSpec,
    ProviderWorkloadStatus,
};
use banlieue_provider_sdk::finalizer::{ensure_finalizer, remove_finalizer};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_on_error};
use banlieue_provider_sdk::ssa::server_side_apply;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::ServiceAccount;
use k8s_openapi::api::rbac::v1::{ClusterRoleBinding, Role, RoleBinding};
use kube::api::{DeleteParams, ListParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::{Api, Resource, ResourceExt};
use serde_json::json;
use tracing::{debug, info, warn};

use crate::context::Context;
use crate::error::{Error, Result};
use crate::events;
use crate::naming::{cluster_scoped_name, owned_by_selector, workload_name};
use crate::workload::{WorkloadInputs, build_workload, owner_reference};

/// Finalizer held on every `Provider` this operator manages.
///
/// Present solely so the cluster-scoped ClusterRoleBinding (and, when a class
/// pins `workloadNamespace` away from the Provider, the other unowned objects)
/// can be removed before the Provider disappears.
pub const FINALIZER: &str = "banlieue.io/provider-workload";

/// Field manager for everything this operator writes.
pub const FIELD_MANAGER: &str = "banlieue.io/operator";

/// Why a `Provider` is intentionally not being reconciled into a workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `Provider.spec.paused` is set.
    ProviderPaused,
    /// The referenced `ProviderClass.spec.paused` is set.
    ClassPaused,
}

impl SkipReason {
    /// Human-readable explanation for logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderPaused => "Provider.spec.paused is set",
            Self::ClassPaused => "ProviderClass.spec.paused is set",
        }
    }
}

/// Whether reconciliation is suspended, and by which object.
///
/// A paused `Provider` wins over a paused class: it is the more specific
/// signal, and naming it points the reader at the object actually edited.
#[must_use]
pub fn skip_reason(provider: &ProviderSpec, class: &ProviderClassSpec) -> Option<SkipReason> {
    if provider.paused {
        return Some(SkipReason::ProviderPaused);
    }
    if class.paused {
        return Some(SkipReason::ClassPaused);
    }
    None
}

/// Names of the ConfigMap and Secret (in that order) a Provider's CA bundle is
/// read from, if any.
///
/// Drives the `resourceNames` on the generated Role. An inline PEM needs no
/// grant at all — nothing is read from the API server.
#[must_use]
pub fn ca_bundle_refs(connection: &ProviderConnection) -> (Option<String>, Option<String>) {
    let Some(ca_bundle) = connection.ca_bundle.as_ref() else {
        return (None, None);
    };
    (
        ca_bundle.config_map_ref.as_ref().map(|s| s.name.clone()),
        ca_bundle.secret_ref.as_ref().map(|s| s.name.clone()),
    )
}

/// Build the status stanza describing a Provider's workload.
///
/// A missing Deployment, or one whose status the apiserver has not populated,
/// reads as zero ready replicas rather than being reported healthy.
#[must_use]
pub fn workload_status(
    deployment: Option<&Deployment>,
    namespace: &str,
    name: &str,
    observed_generation: Option<i64>,
) -> ProviderWorkloadStatus {
    let ready_replicas = deployment
        .and_then(|d| d.status.as_ref())
        .and_then(|s| s.ready_replicas)
        .unwrap_or(0);

    ProviderWorkloadStatus {
        deployment_name: name.to_string(),
        namespace: namespace.to_string(),
        ready_replicas,
        observed_generation,
    }
}

/// Reconcile one `Provider` into its workload.
///
/// # Errors
/// Returns an error if the API server rejects a read, apply, or delete.
pub async fn reconcile(provider: Arc<Provider>, ctx: Arc<Context>) -> Result<Action> {
    let name = provider.name_any();
    let namespace = provider
        .namespace()
        .ok_or(Error::Missing("Provider.metadata.namespace"))?;
    let providers: Api<Provider> = Api::namespaced(ctx.client.clone(), &namespace);

    if provider.meta().deletion_timestamp.is_some() {
        return cleanup(&provider, &providers, &ctx).await;
    }

    ensure_finalizer(&providers, provider.as_ref(), FINALIZER).await?;

    let class_name = &provider.spec.provider_class_ref.name;
    let classes: Api<ProviderClass> = Api::all(ctx.client.clone());
    let Some(class) = classes.get_opt(class_name).await? else {
        warn!(
            provider = %name,
            class = %class_name,
            "ProviderClass not found — cannot provision a workload"
        );
        emit(&ctx, &provider, events::class_not_found(class_name)).await;
        return Ok(requeue_default());
    };

    if let Some(reason) = skip_reason(&provider.spec, &class.spec) {
        info!(provider = %name, reason = reason.as_str(), "skipping workload reconcile");
        emit(&ctx, &provider, events::reconcile_skipped(reason)).await;
        // Deliberately the NORMAL requeue, not the long one. Nothing watches
        // `ProviderClass`, so a paused resource is only re-examined when this
        // timer fires — and with the 300s long requeue, un-pausing took up to
        // five minutes to take effect. A paused check is a cheap no-op, so the
        // slower interval bought nothing and cost responsiveness. Caught by the
        // kind e2e, which timed out waiting for an unpaused workload.
        return Ok(requeue_default());
    }

    apply_workload(&provider, &class, &namespace, &ctx).await?;
    let workload_namespace = class.spec.workload_namespace_or(&namespace).to_string();
    emit(
        &ctx,
        &provider,
        events::workload_applied(
            &workload_name(&class.name_any(), &name),
            &workload_namespace,
        ),
    )
    .await;

    for pruned in prune_orphans(&provider, &class, &namespace, &ctx).await? {
        emit(&ctx, &provider, events::workload_pruned(&pruned)).await;
    }

    publish_status(&provider, &class, &namespace, &providers, &ctx).await?;

    Ok(requeue_default())
}

/// Publish an Event against `provider`.
///
/// Deliberately swallows failures: an Event is diagnostic output, and losing
/// one must never fail a reconcile that otherwise succeeded.
async fn emit(ctx: &Context, provider: &Provider, event: kube::runtime::events::Event) {
    let reason = event.reason.clone();
    if let Err(e) = ctx
        .recorder
        .publish(&event, &provider.object_ref(&()))
        .await
    {
        warn!(error = %e, %reason, "failed to publish event");
    }
}

/// Delete objects that belong to this Provider but are no longer part of its
/// current workload.
///
/// ADR-0007 deliberately ships the `providerClassRef` immutability policy as
/// *optional* hardening and states the controller must not depend on it,
/// "falling back to the controller's delete-and-recreate semantics". This is
/// those semantics.
///
/// Without it, editing `spec.providerClassRef` on a cluster that never applied
/// `deploy/admission/` changes the derived name, so a second workload appears
/// while the first keeps running — two provider pods for one backend, both
/// holding credentials. The stale ClusterRoleBinding is worse: nothing owns it,
/// and a name-based cleanup computed from the *current* class could never find
/// it again, so it would leak permanently.
///
/// Selection is by label, pinned to both provider name and namespace, and
/// searches **every** namespace — a class change can move the workload
/// namespace too, so scoping the search to the current one would miss the
/// orphan entirely.
async fn prune_orphans(
    provider: &Provider,
    class: &ProviderClass,
    provider_namespace: &str,
    ctx: &Context,
) -> Result<Vec<String>> {
    let name = provider.name_any();
    let selector = owned_by_selector(provider_namespace, &name);

    let keep_namespaced = vec![workload_name(&class.name_any(), &name)];
    // Roles and RoleBindings come in pairs: the controller's, and the
    // read-only import identity's (ADR-0016 §4).
    let keep_rbac = vec![
        workload_name(&class.name_any(), &name),
        format!("{}-import", workload_name(&class.name_any(), &name)),
    ];
    let keep_cluster_scoped = cluster_scoped_name(&class.name_any(), provider_namespace, &name);

    let mut pruned = Vec::new();
    pruned.extend(prune_namespaced::<Deployment>(ctx, &selector, &keep_namespaced).await?);
    pruned.extend(prune_namespaced::<ServiceAccount>(ctx, &selector, &keep_namespaced).await?);
    pruned.extend(prune_namespaced::<Role>(ctx, &selector, &keep_rbac).await?);
    pruned.extend(prune_namespaced::<RoleBinding>(ctx, &selector, &keep_rbac).await?);
    pruned.extend(prune_cluster_role_bindings(ctx, &selector, Some(&keep_cluster_scoped)).await?);

    // One name may appear for several kinds; the caller emits one Event per
    // distinct workload, not per object.
    pruned.sort_unstable();
    pruned.dedup();
    Ok(pruned)
}

/// Delete every namespaced object of kind `K` matching `selector`, except those
/// named in `keep`. An empty `keep` deletes all of them.
///
/// A **set**, not a single name: one Provider legitimately owns more than one
/// Role — the controller's and the import identity's (ADR-0016 §4). When this
/// took a single name the pruner deleted the import Role and RoleBinding
/// moments after the reconciler created them, on every pass, forever.
async fn prune_namespaced<K>(ctx: &Context, selector: &str, keep: &[String]) -> Result<Vec<String>>
where
    K: kube::Resource<Scope = k8s_openapi::NamespaceResourceScope, DynamicType = ()>
        + Clone
        + std::fmt::Debug
        + serde::de::DeserializeOwned,
{
    let all: Api<K> = Api::all(ctx.client.clone());
    let found = all.list(&ListParams::default().labels(selector)).await?;
    let mut removed = Vec::new();

    for object in found.items {
        let object_name = object.name_any();
        if keep.iter().any(|k| k == &object_name) {
            continue;
        }
        let Some(object_namespace) = object.namespace() else {
            continue;
        };
        let api: Api<K> = Api::namespaced(ctx.client.clone(), &object_namespace);
        delete_ignoring_missing(&api, &object_name).await?;
        info!(
            kind = std::any::type_name::<K>(),
            namespace = %object_namespace,
            name = %object_name,
            "pruned workload object no longer part of this Provider"
        );
        removed.push(object_name);
    }
    Ok(removed)
}

/// The cluster-scoped half of [`prune_namespaced`].
async fn prune_cluster_role_bindings(
    ctx: &Context,
    selector: &str,
    keep: Option<&str>,
) -> Result<Vec<String>> {
    let api: Api<ClusterRoleBinding> = Api::all(ctx.client.clone());
    let found = api.list(&ListParams::default().labels(selector)).await?;
    let mut removed = Vec::new();

    for binding in found.items {
        let binding_name = binding.name_any();
        if keep == Some(binding_name.as_str()) {
            continue;
        }
        delete_ignoring_missing(&api, &binding_name).await?;
        info!(name = %binding_name, "pruned orphaned ClusterRoleBinding");
        removed.push(binding_name);
    }
    Ok(removed)
}

/// Server-side apply every object backing this Provider.
async fn apply_workload(
    provider: &Provider,
    class: &ProviderClass,
    provider_namespace: &str,
    ctx: &Context,
) -> Result<()> {
    let workload_namespace = class
        .spec
        .workload_namespace_or(provider_namespace)
        .to_string();
    let (ca_config_map, ca_secret) = ca_bundle_refs(&provider.spec.connection);
    let owner = provider
        .meta()
        .uid
        .as_ref()
        .map(|uid| owner_reference(&provider.name_any(), uid));

    let inputs = WorkloadInputs {
        class_name: &class.name_any(),
        class: &class.spec,
        provider_name: &provider.name_any(),
        provider_namespace,
        workload_namespace: &workload_namespace,
        credentials_secret: &provider.spec.connection.credentials_ref.name,
        build_toleration: &ctx.build_toleration,
        ca_bundle_config_map: ca_config_map.as_deref(),
        ca_bundle_secret: ca_secret.as_deref(),
        owner,
    };
    let set = build_workload(&inputs, &ctx.imagebuild_namespace);

    let service_accounts: Api<ServiceAccount> =
        Api::namespaced(ctx.client.clone(), &workload_namespace);
    let deployments: Api<Deployment> = Api::namespaced(ctx.client.clone(), &workload_namespace);
    let roles: Api<Role> = Api::namespaced(ctx.client.clone(), provider_namespace);
    let role_bindings: Api<RoleBinding> = Api::namespaced(ctx.client.clone(), provider_namespace);
    let cluster_role_bindings: Api<ClusterRoleBinding> = Api::all(ctx.client.clone());

    // Order matters: the identity and its permissions must exist before the
    // pod that uses them, or the provider starts up unable to read anything.
    server_side_apply(&service_accounts, FIELD_MANAGER, &set.service_account).await?;
    server_side_apply(&roles, FIELD_MANAGER, &set.role).await?;
    server_side_apply(&role_bindings, FIELD_MANAGER, &set.role_binding).await?;
    // The import identity: read-only, cross-namespace, so the import Job can
    // reach this Provider and its credentials from the build namespace
    // (ADR-0016 §4).
    server_side_apply(&roles, FIELD_MANAGER, &set.import_role).await?;
    server_side_apply(&role_bindings, FIELD_MANAGER, &set.import_role_binding).await?;
    server_side_apply(
        &cluster_role_bindings,
        FIELD_MANAGER,
        &set.cluster_role_binding,
    )
    .await?;
    server_side_apply(&deployments, FIELD_MANAGER, &set.deployment).await?;

    debug!(
        provider = %provider.name_any(),
        namespace = %workload_namespace,
        workload = %set.deployment.name_any(),
        "workload applied"
    );
    Ok(())
}

/// Mirror the workload's readiness into `Provider.status.workload`.
async fn publish_status(
    provider: &Provider,
    class: &ProviderClass,
    provider_namespace: &str,
    providers: &Api<Provider>,
    ctx: &Context,
) -> Result<()> {
    let workload_namespace = class.spec.workload_namespace_or(provider_namespace);
    let name = workload_name(&class.name_any(), &provider.name_any());

    let deployments: Api<Deployment> = Api::namespaced(ctx.client.clone(), workload_namespace);
    let deployment = deployments.get_opt(&name).await?;

    let status = workload_status(
        deployment.as_ref(),
        workload_namespace,
        &name,
        provider.meta().generation,
    );

    // Patch only `status.workload`; `status.conditions` stays owned by the
    // provider's own field manager (ADR-0012).
    let patch = json!({
        "apiVersion": Provider::api_version(&()),
        "kind": Provider::kind(&()),
        "status": { "workload": status },
    });
    providers
        .patch_status(
            &provider.name_any(),
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&patch),
        )
        .await?;

    Ok(())
}

/// Delete what owner references cannot, then release the finalizer.
async fn cleanup(provider: &Provider, providers: &Api<Provider>, ctx: &Context) -> Result<Action> {
    let name = provider.name_any();
    let namespace = provider
        .namespace()
        .ok_or(Error::Missing("Provider.metadata.namespace"))?;
    // Delete by LABEL, not by a recomputed name. A name derived from the
    // Provider's current `providerClassRef` cannot find objects created under a
    // previous class, so a name-based cleanup would leak exactly the objects
    // that owner references also cannot reach — the cluster-scoped
    // ClusterRoleBinding above all. Searching by label across all namespaces
    // also covers a workload namespace that moved.
    //
    // Owner references still garbage-collect the same-namespace objects; this
    // is belt-and-braces for them and the only mechanism for the rest.
    let selector = owned_by_selector(&namespace, &name);

    prune_cluster_role_bindings(ctx, &selector, None).await?;
    // Deleting the Provider: keep nothing, including the import identity's
    // Role and RoleBinding.
    prune_namespaced::<Deployment>(ctx, &selector, &[]).await?;
    prune_namespaced::<ServiceAccount>(ctx, &selector, &[]).await?;
    prune_namespaced::<Role>(ctx, &selector, &[]).await?;
    prune_namespaced::<RoleBinding>(ctx, &selector, &[]).await?;

    info!(provider = %name, "workload cleaned up");
    emit(
        ctx,
        provider,
        events::workload_deleted(&workload_name(
            &provider.spec.provider_class_ref.name,
            &name,
        )),
    )
    .await;
    remove_finalizer(providers, provider, FINALIZER).await?;
    Ok(Action::await_change())
}

/// Delete `name`, treating "already gone" as success so cleanup is idempotent.
async fn delete_ignoring_missing<K>(api: &Api<K>, name: &str) -> Result<()>
where
    K: Clone + std::fmt::Debug + serde::de::DeserializeOwned,
{
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(e)) if e.code == HTTP_NOT_FOUND => Ok(()),
        Err(e) => Err(Error::Kube(e)),
    }
}

/// HTTP status the apiserver returns for an object that no longer exists.
const HTTP_NOT_FOUND: u16 = 404;

/// Requeue policy for a failed reconcile.
#[must_use]
pub fn error_policy(_provider: Arc<Provider>, error: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %error, "provider workload reconcile failed");
    requeue_on_error()
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;
