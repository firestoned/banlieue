// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Cluster API bootstrap data → InfraMachine `spec.userData` (ADR-0129).
//!
//! One reconciler, generic over the four InfraMachine kinds, run only for the
//! kinds whose provider holds the guest (`app.rs`: Cloud Hypervisor until
//! milestone 2). For a machine a
//! CAPI `Machine` owns (and that Machine, by UID and `spec.infrastructureRef`,
//! confirms), it waits for `spec.bootstrap.dataSecretName`, reads
//! that Secret by name in the Machine's namespace, classifies it
//! ([`bootstrap_value`]), and applies its value byte for byte into
//! `spec.userData`. It reports on [`BOOTSTRAP_DATA_READY`], the one status
//! condition it writes and providers never apply; host providers hold the
//! guest until that condition is `True` ([`banlieue_api::capi::holds_creation`]).
//!
//! Wake-ups: the InfraMachine kinds and CAPI `Machine`s are watched, so
//! adoption and `dataSecretName` arrive as events. The controller holds `get`
//! on Secrets but not `list`/`watch`, so a machine waiting on a Secret that is
//! missing, unreadable or wrong is requeued on a fixed interval instead
//! (ADR-0129 Decision 3).
//!
//! Only machines in namespaces the controller serves are touched (ADR-0130):
//! the watches follow its scope, and selector mode gates each reconcile.
//!
//! Once delivered, `spec.userData` is never rewritten, and nothing is
//! delivered to a machine whose provider has already set `spec.providerID`
//! (Decision 7).

use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use banlieue_api::capi::{
    BOOTSTRAP_DATA_READY, BootstrapTarget, CAPI_GROUP, CapiObjectRef, MACHINE_KIND, MACHINE_PLURAL,
    MACHINE_VERSION, MachineBootstrap, bootstrap_reasons, bootstrap_value, is_bootstrap_data_ready,
    is_capi_intended, machine_confirms_adoption, owning_machine,
};
use banlieue_provider_sdk::reconciler::{
    REQUEUE_DEFAULT_SECS, REQUEUE_LONG_SECS, requeue_on_error,
};
use banlieue_provider_sdk::status::set_condition;
use futures::StreamExt;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::reflector::ObjectRef;
use kube::runtime::{WatchStreamExt, watcher};
use kube::{Resource, ResourceExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::{Error, Result};

/// Field manager for the two things this reconciler writes: `spec.userData`
/// and the [`BOOTSTRAP_DATA_READY`] condition. Its own, so neither contends
/// with the `VirtualMachine` path's manager or a provider's.
pub const FIELD_MANAGER: &str = "banlieue.io/bootstrap-data-controller";

/// Requeue for a Secret that does not exist yet. Bootstrap providers write the
/// Secret before publishing its name, so this is rare and short-lived, and a
/// join token's lifetime is the budget.
pub const SECRET_NOT_FOUND_REQUEUE: Duration = Duration::from_secs(REQUEUE_DEFAULT_SECS);

/// Requeue for a Secret a person must fix or a Role a person must grant.
pub const NEEDS_A_PERSON_REQUEUE: Duration = Duration::from_secs(REQUEUE_LONG_SECS);

/// The InfraMachine API group.
const INFRA_GROUP: &str = "infrastructure.banlieue.io";

/// HTTP status the API server answers a read RBAC does not allow with.
const HTTP_FORBIDDEN: u16 = 403;

/// The CustomResourceDefinition that serves Cluster API `Machine`s. The
/// controller's ClusterRole may read this one CRD and no other.
pub const MACHINE_CRD: &str = "machines.cluster.x-k8s.io";

/// The condition a CRD reports once the API server serves it.
const CRD_ESTABLISHED: &str = "Established";

const STATUS_TRUE: &str = "True";
const STATUS_FALSE: &str = "False";

/// What the InfraMachine alone says to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    /// Not Cluster API's, already delivered, or already past the provider's
    /// hold: nothing to do.
    Skip,
    /// Cluster API's, but no Machine has adopted it yet.
    NotAdopted,
    /// Owned by this Machine; read it next.
    ReadMachine {
        /// The owning Machine's name, in the machine's namespace.
        name: String,
        /// The UID the owner reference names; the Machine read must carry it.
        uid: String,
    },
}

/// Decide from the InfraMachine alone.
#[must_use]
pub fn gate<K: Resource + BootstrapTarget>(machine: &K) -> Gate {
    let meta = machine.meta();
    if !is_capi_intended(meta) {
        return Gate::Skip;
    }
    if is_bootstrap_data_ready(machine.conditions()) {
        return Gate::Skip;
    }
    if machine.creation_started() {
        return Gate::Skip;
    }
    match owning_machine(meta) {
        None => Gate::NotAdopted,
        Some(owner) => Gate::ReadMachine {
            name: owner.name.clone(),
            uid: owner.uid.clone(),
        },
    }
}

/// The result of reading the bootstrap Secret by name.
#[derive(Clone, Debug, PartialEq)]
pub enum SecretLookup {
    /// It exists. Boxed: a Secret is large beside the other variants.
    Found(Box<Secret>),
    /// It does not exist (yet).
    NotFound,
    /// The controller may not read Secrets in this namespace.
    Forbidden,
}

/// What to do once the Machine (and possibly the Secret) has been read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Copy this into `spec.userData` and report `True`.
    Deliver {
        /// The Secret's `value`.
        value: String,
    },
    /// Report `False` with this reason and message, and come back after
    /// `requeue` (`None`: a watch event will).
    Report {
        /// A [`bootstrap_reasons`] value.
        reason: &'static str,
        /// What is being waited for, or what is wrong.
        message: String,
        /// When to look again, for waits no watched object can end.
        requeue: Option<Duration>,
    },
}

fn waiting(message: String, requeue: Option<Duration>) -> Outcome {
    Outcome::Report {
        reason: bootstrap_reasons::WAITING_FOR_BOOTSTRAP_DATA,
        message,
        requeue,
    }
}

/// The InfraMachine as a Machine's `spec.infrastructureRef` names it.
#[must_use]
pub fn infra_ref_of<K: Resource<DynamicType = ()>>(machine: &K) -> CapiObjectRef {
    CapiObjectRef {
        api_group: K::group(&()).into_owned(),
        kind: K::kind(&()).into_owned(),
        name: machine.meta().name.clone().unwrap_or_default(),
    }
}

/// The report for a Machine that does not confirm owning `infra_machine`
/// ([`machine_confirms_adoption`]), or `None` when it does and its Secret may
/// be read. No retry: a change to either object is a watch event.
#[must_use]
pub fn unconfirmed_adoption<K: Resource<DynamicType = ()>>(
    infra_machine: &K,
    owner_uid: &str,
    machine_uid: Option<&str>,
    machine: &MachineBootstrap,
) -> Option<Outcome> {
    if machine_confirms_adoption(
        machine_uid,
        machine,
        owner_uid,
        &infra_ref_of(infra_machine),
    ) {
        return None;
    }
    Some(Outcome::Report {
        reason: bootstrap_reasons::MACHINE_REFERENCE_MISMATCH,
        message: format!(
            "the Machine this {} names as its owner is not that object, or does not name it \
             as its spec.infrastructureRef",
            K::kind(&())
        ),
        requeue: None,
    })
}

/// The report for an owning Machine that exists but has no
/// `spec.clusterName`, which v1beta2 requires: not "not found", which would
/// send whoever reads it looking for the wrong problem. No retry: fixing the
/// Machine is a watch event.
#[must_use]
pub fn unreadable_machine(machine_name: &str) -> Outcome {
    waiting(
        format!("the owning Machine {machine_name} has no spec.clusterName"),
        None,
    )
}

/// Decide from the owning Machine (`None`: it does not exist) and, once it
/// names a Secret, that Secret's lookup.
#[must_use]
pub fn outcome(machine: Option<&MachineBootstrap>, secret: Option<SecretLookup>) -> Outcome {
    let Some(machine) = machine else {
        return waiting("the owning Machine was not found".to_string(), None);
    };
    let Some(name) = machine.data_secret_name.as_deref() else {
        return waiting(
            "the Machine has no spec.bootstrap.dataSecretName yet".to_string(),
            None,
        );
    };
    match secret {
        None | Some(SecretLookup::NotFound) => waiting(
            format!("bootstrap Secret {name} does not exist yet"),
            Some(SECRET_NOT_FOUND_REQUEUE),
        ),
        Some(SecretLookup::Forbidden) => Outcome::Report {
            reason: bootstrap_reasons::BOOTSTRAP_SECRET_FORBIDDEN,
            message: format!(
                "banlieue-controller may not get Secret {name} in this namespace; \
                 grant it a Role with get on secrets here"
            ),
            requeue: Some(NEEDS_A_PERSON_REQUEUE),
        },
        Some(SecretLookup::Found(secret)) => match bootstrap_value(&secret, machine) {
            Ok(value) => Outcome::Deliver { value },
            Err(rejection) => Outcome::Report {
                reason: rejection.reason,
                message: rejection.message,
                requeue: Some(NEEDS_A_PERSON_REQUEUE),
            },
        },
    }
}

/// The InfraMachine of kind `K` a Machine's `spec.infrastructureRef` names:
/// the Machine watch's mapper.
#[must_use]
pub fn infra_machine_for<K>(machine: &DynamicObject) -> Option<ObjectRef<K>>
where
    K: Resource<DynamicType = ()>,
{
    let infra = MachineBootstrap::from_machine(&machine.data)?.infrastructure_ref?;
    if infra.api_group != INFRA_GROUP || infra.kind != K::kind(&()) {
        return None;
    }
    let mut r = ObjectRef::<K>::new(&infra.name);
    if let Some(ns) = machine.namespace() {
        r = r.within(&ns);
    }
    Some(r)
}

/// The server-side apply body that sets `spec.userData` and nothing else.
#[must_use]
pub fn user_data_patch<K: Resource<DynamicType = ()>>(value: &str) -> Value {
    json!({
        "apiVersion": K::api_version(&()),
        "kind": K::kind(&()),
        "spec": { "userData": value },
    })
}

/// The server-side apply body that sets this reconciler's one condition.
#[must_use]
pub fn condition_patch<K: Resource<DynamicType = ()>>(condition: &Condition) -> Value {
    json!({
        "apiVersion": K::api_version(&()),
        "kind": K::kind(&()),
        "status": { "conditions": [condition] },
    })
}

/// The [`BOOTSTRAP_DATA_READY`] condition to write, keeping the current
/// transition time when the status is unchanged.
#[must_use]
pub fn next_condition<K: Resource + BootstrapTarget>(
    machine: &K,
    status: &str,
    reason: &str,
    message: &str,
) -> Condition {
    let mut conditions: Vec<Condition> = machine
        .conditions()
        .iter()
        .filter(|c| c.type_ == BOOTSTRAP_DATA_READY)
        .cloned()
        .collect();
    let generation = machine.meta().generation.unwrap_or_default();
    set_condition(
        &mut conditions,
        BOOTSTRAP_DATA_READY,
        status,
        reason,
        message,
        generation,
    );
    conditions.swap_remove(0)
}

/// Whether writing `condition` would change the machine. An unchanged write
/// is skipped, so a report does not wake its own watch for nothing.
#[must_use]
pub fn condition_changed<K: BootstrapTarget>(machine: &K, condition: &Condition) -> bool {
    machine
        .conditions()
        .iter()
        .find(|c| c.type_ == BOOTSTRAP_DATA_READY)
        .is_none_or(|current| {
            current.status != condition.status
                || current.reason != condition.reason
                || current.message != condition.message
                || current.observed_generation != condition.observed_generation
        })
}

/// The `cluster.x-k8s.io/v1beta2` `Machine` resource.
#[must_use]
pub fn machine_resource() -> ApiResource {
    let gvk = GroupVersionKind::gvk(CAPI_GROUP, MACHINE_VERSION, MACHINE_KIND);
    ApiResource::from_gvk_with_plural(&gvk, MACHINE_PLURAL)
}

/// Whether this CRD makes the API server serve `Machine`s at
/// [`MACHINE_VERSION`]: it is [`MACHINE_CRD`], `Established`, and serves that
/// version.
#[must_use]
pub fn crd_serves_machines(crd: &CustomResourceDefinition) -> bool {
    if crd.metadata.name.as_deref() != Some(MACHINE_CRD) {
        return false;
    }
    let established = crd.status.as_ref().is_some_and(|s| {
        s.conditions
            .iter()
            .flatten()
            .any(|c| c.type_ == CRD_ESTABLISHED && c.status == STATUS_TRUE)
    });
    established
        && crd
            .spec
            .versions
            .iter()
            .any(|v| v.name == MACHINE_VERSION && v.served)
}

/// Return once the API server serves `machines.cluster.x-k8s.io` at
/// [`MACHINE_VERSION`] (ADR-0129 Decision 2): at once when it already does,
/// otherwise when Cluster API is installed, however long after the
/// controller started.
///
/// A watch on the one CRD ([`MACHINE_CRD`]), not discovery at startup, so a
/// later `clusterctl init` turns the reconciler on without a restart, and an
/// API error (a 503 while the API server restarts, a missing grant) is
/// logged and retried with backoff instead of stopping every other
/// controller in the process.
pub async fn wait_for_capi_machines(client: &kube::Client) {
    let crds: Api<CustomResourceDefinition> = Api::all(client.clone());
    let config = watcher::Config::default().fields(&format!("metadata.name={MACHINE_CRD}"));
    let mut events = std::pin::pin!(watcher(crds, config).default_backoff().applied_objects());
    while let Some(event) = events.next().await {
        match event {
            Ok(crd) if crd_serves_machines(&crd) => return,
            Ok(_) => {}
            Err(e) => {
                warn!(crd = MACHINE_CRD, error = %e, "watching the Cluster API Machine CRD failed; retrying");
            }
        }
    }
    // A backed-off watcher stream does not end; if it ever does, the
    // reconciler stays off rather than starting against an unserved API.
    warn!(
        crd = MACHINE_CRD,
        "the Cluster API Machine CRD watch ended; CAPI bootstrap-data controllers stay off"
    );
    futures::future::pending::<()>().await;
}

/// Reconcile one InfraMachine of kind `K`.
///
/// # Errors
/// Kubernetes API errors other than a missing Machine or Secret, or a
/// forbidden Secret read, which are reported on the condition instead.
pub async fn reconcile<K>(obj: Arc<K>, ctx: Arc<Context>) -> Result<Action>
where
    K: Resource<DynamicType = ()> + BootstrapTarget + Clone + Debug + DeserializeOwned + Serialize,
    K: Resource<Scope = k8s_openapi::NamespaceResourceScope>,
{
    let gate = gate(obj.as_ref());
    if gate == Gate::Skip {
        return Ok(Action::await_change());
    }
    let ns = obj
        .namespace()
        .ok_or(Error::Missing("metadata.namespace"))?;
    // Not served (ADR-0130): read nothing, write nothing. This reconciler
    // holds no finalizer, so a deleting machine needs nothing either.
    if let Some(action) = ctx.skip_unserved(&ns, false) {
        return Ok(action);
    }
    let api: Api<K> = Api::namespaced(ctx.client.clone(), &ns);

    let Gate::ReadMachine {
        name: machine_name,
        uid: owner_uid,
    } = gate
    else {
        let not_adopted = waiting("not yet adopted by a Cluster API Machine".to_string(), None);
        return act(&api, obj.as_ref(), not_adopted).await;
    };

    let machines: Api<DynamicObject> =
        Api::namespaced_with(ctx.client.clone(), &ns, &machine_resource());
    let found = machines.get_opt(&machine_name).await?;
    let machine_uid = found.as_ref().and_then(|m| m.metadata.uid.clone());
    let machine = match found {
        None => None,
        Some(m) => match MachineBootstrap::from_machine(&m.data) {
            Some(parsed) => Some(parsed),
            None => return act(&api, obj.as_ref(), unreadable_machine(&machine_name)).await,
        },
    };
    // Before any Secret is read: the owner reference must name this very
    // Machine, and the Machine must name this InfraMachine back.
    let mismatch = machine
        .as_ref()
        .and_then(|m| unconfirmed_adoption(obj.as_ref(), &owner_uid, machine_uid.as_deref(), m));
    if let Some(mismatch) = mismatch {
        warn!(kind = K::kind(&()).as_ref(), machine = %obj.name_any(), namespace = %ns, owner = %machine_name, "owner Machine does not confirm adoption; bootstrap Secret not read");
        return act(&api, obj.as_ref(), mismatch).await;
    }
    let lookup = match machine.as_ref().and_then(|m| m.data_secret_name.as_deref()) {
        None => None,
        Some(secret_name) => Some(lookup_secret(&ctx, &ns, secret_name).await?),
    };
    act(&api, obj.as_ref(), outcome(machine.as_ref(), lookup)).await
}

/// Carry out an [`Outcome`] on `machine`: deliver and report `True`, or
/// report `False` and come back when it asks.
async fn act<K>(api: &Api<K>, machine: &K, outcome: Outcome) -> Result<Action>
where
    K: Resource<DynamicType = ()> + BootstrapTarget + Clone + Debug + DeserializeOwned + Serialize,
{
    let name = machine.name_any();
    match outcome {
        Outcome::Deliver { value } => {
            // The condition is written against the object the patch returned,
            // so its observedGeneration is the generation that carries the
            // data, not the one before it.
            let delivered = if machine.user_data() == Some(value.as_str()) {
                machine.clone()
            } else {
                api.patch(
                    &name,
                    &PatchParams::apply(FIELD_MANAGER).force(),
                    &Patch::Apply(&user_data_patch::<K>(&value)),
                )
                .await?
            };
            report_status(
                api,
                &delivered,
                STATUS_TRUE,
                bootstrap_reasons::BOOTSTRAP_DATA_DELIVERED,
                "spec.userData holds the Machine's bootstrap data",
            )
            .await?;
            info!(kind = K::kind(&()).as_ref(), machine = %name, namespace = %machine.namespace().unwrap_or_default(), "bootstrap data delivered");
            Ok(Action::await_change())
        }
        Outcome::Report {
            reason,
            message,
            requeue,
        } => {
            report_status(api, machine, STATUS_FALSE, reason, &message).await?;
            Ok(requeue.map_or_else(Action::await_change, Action::requeue))
        }
    }
}

async fn lookup_secret(ctx: &Context, ns: &str, name: &str) -> Result<SecretLookup> {
    let secrets: Api<Secret> = Api::namespaced(ctx.client.clone(), ns);
    match secrets.get_opt(name).await {
        Ok(Some(secret)) => Ok(SecretLookup::Found(Box::new(secret))),
        Ok(None) => Ok(SecretLookup::NotFound),
        Err(kube::Error::Api(e)) if e.code == HTTP_FORBIDDEN => Ok(SecretLookup::Forbidden),
        Err(e) => Err(e.into()),
    }
}

async fn report_status<K>(
    api: &Api<K>,
    machine: &K,
    status: &str,
    reason: &str,
    message: &str,
) -> Result<()>
where
    K: Resource<DynamicType = ()> + BootstrapTarget + Clone + Debug + DeserializeOwned,
{
    let condition = next_condition(machine, status, reason, message);
    if !condition_changed(machine, &condition) {
        return Ok(());
    }
    api.patch_status(
        &machine.name_any(),
        // This reconciler is the only writer of this condition entry; `force`
        // takes it back from a provider build that predates ADR-0129.
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&condition_patch::<K>(&condition)),
    )
    .await?;
    Ok(())
}

/// Requeue on errors after the SDK's short interval.
pub fn error_policy<K>(obj: Arc<K>, err: &Error, _ctx: Arc<Context>) -> Action
where
    K: Resource<DynamicType = ()>,
{
    warn!(kind = K::kind(&()).as_ref(), machine = %obj.meta().name.as_deref().unwrap_or_default(), error = %err, "bootstrap data reconcile failed");
    requeue_on_error()
}

#[cfg(test)]
#[path = "bootstrap_data_tests.rs"]
mod bootstrap_data_tests;
