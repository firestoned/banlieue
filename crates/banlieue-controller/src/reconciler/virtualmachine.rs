// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VirtualMachine` reconciler — Phase 1A iteration 2.
//!
//! Reconcile loop:
//!
//! 1. If `deletion_timestamp` is set → finalize path (drop finalizer; iter 3
//!    will add cascade-wait on the owned infra CR).
//! 2. Ensure the controller finalizer (`banlieue.io/virtualmachine`).
//! 3. Resolve cluster-scoped refs (`VMClass`, `VMImage`).
//! 4. List `Provider`s and sibling `VirtualMachine`s in the VM's namespace.
//! 5. Call [`schedule`] (pure function) → [`Decision`].
//! 6. SSA the provider-specific infra CR (currently `VSphereMachine`),
//!    owner-referenced to the parent VM.
//! 7. Read back the infra CR and mirror its status onto the VM via
//!    [`mirror_status_from_infra`].
//! 8. Patch the VM's status (conditions + `scheduled` + `infrastructureRef`).
//!
//! Errors set a `Scheduled=False` condition with a stable reason
//! (see `super::scheduler::reasons`) and trigger a short requeue. Real
//! Kubernetes errors propagate up to the error_policy.

use std::sync::Arc;

use banlieue_api::banlieue::{Provider, ScheduledPlacement, VMClass, VMImage, VirtualMachine};
use banlieue_api::common::{
    LocalObjectReference as _PlaceholderLocalRef, TypedObjectReference, condition_types,
};
use banlieue_api::infrastructure::VSphereMachine;
use banlieue_provider_sdk::{
    finalizer::{ensure_finalizer, remove_finalizer},
    reconciler::{requeue_default, requeue_on_error},
    ssa::{FIELD_MANAGER_CONTROLLER, server_side_apply},
    status::{condition_status, set_condition},
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube::{
    Resource, ResourceExt,
    api::{Api, DeleteParams, ListParams, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{debug, info, warn};

use super::infra::build_vsphere_machine;
use super::migration::{MigrationAction, PlacementDriftReason, evaluate};
use super::scheduler::{ScheduleError, reasons, schedule};
use super::status_mirror::mirror_onto_vm;
use crate::context::Context;
use crate::error::{Error, Result};

// (Silence the SDK re-export-induced unused warning on the
// `LocalObjectReference` placeholder import.)
#[allow(dead_code)]
type _Anchor = _PlaceholderLocalRef;

/// Finalizer set on every `VirtualMachine` reconciled by this controller.
pub const VM_FINALIZER: &str = "banlieue.io/virtualmachine";

/// Top-level reconcile entrypoint registered with [`kube::runtime::Controller`].
///
/// # Errors
/// Propagates SDK / kube errors; the controller's `error_policy` decides how
/// long to back off before retrying.
pub async fn reconcile(vm: Arc<VirtualMachine>, ctx: Arc<Context>) -> Result<Action> {
    let namespace = vm.namespace().ok_or(Error::Missing("namespace"))?;
    let name = vm.name_any();
    let generation = vm.metadata.generation.unwrap_or(0);

    let span = tracing::info_span!(
        "reconcile",
        kind = "VirtualMachine",
        namespace = %namespace,
        name = %name,
        generation,
    );
    let _enter = span.enter();
    info!("reconciling VirtualMachine");

    let vm_api: Api<VirtualMachine> = Api::namespaced(ctx.client.clone(), &namespace);
    let vsphere_api: Api<VSphereMachine> = Api::namespaced(ctx.client.clone(), &namespace);

    if vm.metadata.deletion_timestamp.is_some() {
        return finalize_vm(&vm_api, &vsphere_api, &vm).await;
    }

    ensure_finalizer(&vm_api, vm.as_ref(), VM_FINALIZER).await?;

    // ---- Resolve refs ---------------------------------------------------
    let class_api: Api<VMClass> = Api::all(ctx.client.clone());
    let image_api: Api<VMImage> = Api::all(ctx.client.clone());
    let provider_api: Api<Provider> = Api::namespaced(ctx.client.clone(), &namespace);

    let class = class_api.get(&vm.spec.class_ref.name).await?;
    let image = image_api.get(&vm.spec.image_ref.name).await?;
    let providers = provider_api.list(&ListParams::default()).await?.items;
    let sibling_vms = vm_api.list(&ListParams::default()).await?.items;

    // ---- Schedule ------------------------------------------------------
    let decision = match schedule(&vm, &class, &image, &providers, &sibling_vms) {
        Ok(d) => d,
        Err(err) => {
            warn!(?err, "scheduling failed; surfacing condition");
            patch_scheduling_failure(&vm_api, &name, generation, &err).await?;
            return Ok(requeue_default());
        }
    };

    // Look up the chosen provider; threaded into the infra builder so future
    // providers (Proxmox, libvirt) can pull spec-level fields like the API
    // endpoint or SSH transport from it.
    let chosen_provider = providers
        .iter()
        .find(|p| p.name_any() == decision.provider_name)
        .ok_or(Error::Missing("chosen provider not found in listing"))?;

    // ---- Migration sub-loop --------------------------------------------
    // Compare the fresh scheduler decision against the previously-recorded
    // placement and act per VirtualMachine.spec.migrationPolicy.
    let migration_action = evaluate(&vm, &decision);
    match &migration_action {
        MigrationAction::InPlace => {
            // No drift (or first schedule) — fall through to the apply path.
        }
        MigrationAction::StickToOld => {
            // migrationPolicy=Never; leave the existing infra CR untouched
            // and report the drift as a (passive) PlacementValid=True. We
            // still mirror status from whatever is already on the infra CR.
            info!("placement drift but migrationPolicy=Never; sticking to old placement");
            return mirror_only_path(&vm_api, &vsphere_api, &vm, &name, generation).await;
        }
        MigrationAction::SurfaceOnly { reason } => {
            // migrationPolicy=Manual without the annotation. Set
            // PlacementValid=False; do NOT delete the infra CR yet.
            warn!(
                reason = reason.reason(),
                "placement drift; manual migration required (set annotation banlieue.io/migrate=true)"
            );
            patch_placement_invalid(&vm_api, &name, generation, reason).await?;
            return Ok(requeue_default());
        }
        MigrationAction::Recreate { reason } => {
            // migrationPolicy=Automatic (or Manual + annotation). Delete
            // the existing VSphereMachine; the next reconcile pass will
            // create a fresh one with the new placement. This is the
            // recreate-only path; live migration is Phase 2 work.
            info!(
                reason = reason.reason(),
                "placement drift; recreating infra CR for new placement"
            );
            delete_existing_infra(&vsphere_api, &vm.name_any()).await?;
            patch_placement_invalid(&vm_api, &name, generation, reason).await?;
            return Ok(requeue_default());
        }
    }

    // ---- Build + SSA the infra CR --------------------------------------
    let infra = match build_vsphere_machine(&vm, &class, &image, &decision, chosen_provider) {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "infra builder failed; reporting Scheduled=False");
            patch_infra_build_failure(&vm_api, &name, generation, &e.to_string()).await?;
            return Ok(requeue_on_error());
        }
    };
    let applied = server_side_apply(&vsphere_api, FIELD_MANAGER_CONTROLLER, &infra).await?;
    debug!(
        vsphere_machine = %applied.name_any(),
        "applied VSphereMachine via SSA"
    );

    // ---- Status mirror -------------------------------------------------
    let next_status = mirror_onto_vm(&vm, &applied);
    let scheduled_placement =
        decision.to_scheduled_placement(Time(k8s_openapi::jiff::Timestamp::now()));
    let infra_ref = TypedObjectReference {
        api_group: VSphereMachine::group(&()).to_string(),
        kind: "VSphereMachine".to_string(),
        name: applied.name_any(),
        namespace: applied.namespace(),
    };

    patch_status(
        &vm_api,
        &name,
        generation,
        &scheduled_placement,
        &infra_ref,
        &next_status.conditions,
    )
    .await?;

    Ok(requeue_default())
}

/// Drift-but-Never path: don't touch the infra CR, just mirror its status
/// and keep the existing `status.scheduled` intact. The `PlacementValid`
/// condition is intentionally NOT set to False here because the spec says
/// drift is acceptable for this VM.
async fn mirror_only_path(
    vm_api: &Api<VirtualMachine>,
    vsphere_api: &Api<VSphereMachine>,
    vm: &VirtualMachine,
    name: &str,
    generation: i64,
) -> Result<Action> {
    let infra = match vsphere_api.get_opt(name).await? {
        Some(m) => m,
        None => {
            // The infra CR vanished out from under us (manual delete, GC
            // cascade, etc). Drop back to the normal apply path on the
            // next reconcile.
            return Ok(requeue_on_error());
        }
    };
    let next_status = mirror_onto_vm(vm, &infra);
    patch_status_conditions_only(vm_api, name, generation, &next_status.conditions).await?;
    Ok(requeue_default())
}

/// Delete the owned `VSphereMachine` by name. 404 is treated as success so
/// the call is idempotent across retries.
async fn delete_existing_infra(api: &Api<VSphereMachine>, name: &str) -> Result<()> {
    use kube::Error as KubeError;
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => Ok(()),
        Err(KubeError::Api(e)) if e.code == 404 => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Deletion path with cascade-wait on the owned `VSphereMachine`.
///
/// The contract is:
///
/// 1. If an owned infra CR still exists, request its deletion (idempotent —
///    404s are ok) and requeue. Ownership cascade GC will eventually remove
///    it once the provider clears its own finalizer.
/// 2. Only when no infra CR remains do we drop the `banlieue.io/virtualmachine`
///    finalizer, allowing the API server to GC the parent VM.
///
/// This guarantees we never leave the backend with a dangling VM: deletion
/// of the parent VirtualMachine blocks at the K8s API until the provider has
/// confirmed the backend resource is gone.
async fn finalize_vm(
    api: &Api<VirtualMachine>,
    vsphere_api: &Api<VSphereMachine>,
    vm: &VirtualMachine,
) -> Result<Action> {
    info!("finalizing VirtualMachine");
    let owned_name = vm.name_any();
    match vsphere_api.get_opt(&owned_name).await? {
        Some(infra) if infra.metadata.deletion_timestamp.is_none() => {
            // Issue delete; provider's own finalizer will keep it around
            // until the backend VM is gone.
            info!(
                vsphere_machine = %owned_name,
                "requesting VSphereMachine deletion; waiting for cascade"
            );
            delete_existing_infra(vsphere_api, &owned_name).await?;
            Ok(requeue_on_error())
        }
        Some(_) => {
            // Delete already in flight; just wait.
            debug!("VSphereMachine still terminating; will recheck");
            Ok(requeue_on_error())
        }
        None => {
            // Infra is gone. Safe to drop the parent's finalizer.
            info!("VSphereMachine cleared; removing VirtualMachine finalizer");
            remove_finalizer(api, vm, VM_FINALIZER).await?;
            Ok(requeue_default())
        }
    }
}

/// Patch the VirtualMachine status with `Scheduled=False reason=<reason>`
/// for a failed scheduling attempt.
async fn patch_scheduling_failure(
    api: &Api<VirtualMachine>,
    name: &str,
    generation: i64,
    err: &ScheduleError,
) -> Result<()> {
    let mut conditions = Vec::new();
    set_condition(
        &mut conditions,
        condition_types::SCHEDULED,
        condition_status::FALSE,
        err.reason(),
        err.to_string(),
        generation,
    );
    set_condition(
        &mut conditions,
        condition_types::READY,
        condition_status::FALSE,
        "Scheduling",
        "scheduling not yet successful",
        generation,
    );
    patch_status_conditions_only(api, name, generation, &conditions).await
}

async fn patch_infra_build_failure(
    api: &Api<VirtualMachine>,
    name: &str,
    generation: i64,
    detail: &str,
) -> Result<()> {
    let mut conditions = Vec::new();
    set_condition(
        &mut conditions,
        condition_types::SCHEDULED,
        condition_status::FALSE,
        "InfraBuildFailed",
        detail,
        generation,
    );
    set_condition(
        &mut conditions,
        condition_types::READY,
        condition_status::FALSE,
        "InfraBuildFailed",
        "could not construct provider infrastructure CR",
        generation,
    );
    patch_status_conditions_only(api, name, generation, &conditions).await
}

/// Patch `PlacementValid=False` with the drift reason. Also marks `Ready=False`
/// with reason `PlacementInvalid`. Leaves `status.scheduled` untouched — the
/// previously-recorded placement stays visible until the next pass either
/// recreates (Automatic) or the user resolves drift manually (Manual).
async fn patch_placement_invalid(
    api: &Api<VirtualMachine>,
    name: &str,
    generation: i64,
    reason: &PlacementDriftReason,
) -> Result<()> {
    let mut conditions = Vec::new();
    set_condition(
        &mut conditions,
        condition_types::PLACEMENT_VALID,
        condition_status::FALSE,
        reason.reason(),
        reason.message(),
        generation,
    );
    set_condition(
        &mut conditions,
        condition_types::READY,
        condition_status::FALSE,
        "PlacementInvalid",
        "current placement no longer satisfies the spec",
        generation,
    );
    patch_status_conditions_only(api, name, generation, &conditions).await
}

async fn patch_status_conditions_only(
    api: &Api<VirtualMachine>,
    name: &str,
    generation: i64,
    conditions: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition],
) -> Result<()> {
    let patch = json!({
        "apiVersion": format!("{}/{}", VirtualMachine::group(&()), VirtualMachine::version(&())),
        "kind": "VirtualMachine",
        "status": {
            "conditions": conditions,
            "observedGeneration": generation,
        }
    });
    let params = PatchParams::apply(FIELD_MANAGER_CONTROLLER).force();
    api.patch_status(name, &params, &Patch::Apply(&patch))
        .await?;
    Ok(())
}

async fn patch_status(
    api: &Api<VirtualMachine>,
    name: &str,
    generation: i64,
    scheduled: &ScheduledPlacement,
    infra_ref: &TypedObjectReference,
    conditions: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition],
) -> Result<()> {
    let patch = json!({
        "apiVersion": format!("{}/{}", VirtualMachine::group(&()), VirtualMachine::version(&())),
        "kind": "VirtualMachine",
        "status": {
            "scheduled": scheduled,
            "infrastructureRef": infra_ref,
            "conditions": conditions,
            "observedGeneration": generation,
        }
    });
    let params = PatchParams::apply(FIELD_MANAGER_CONTROLLER).force();
    api.patch_status(name, &params, &Patch::Apply(&patch))
        .await?;
    Ok(())
}

/// Error policy: short backoff for transient errors.
pub fn error_policy(_vm: Arc<VirtualMachine>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "reconcile failed; requeuing on short interval");
    requeue_on_error()
}

// Backwards-compatible re-exports for tests written against iteration 1.
pub use reasons::SCHEDULED as REASON_SCHEDULED;

#[cfg(test)]
#[path = "virtualmachine_tests.rs"]
mod virtualmachine_tests;
