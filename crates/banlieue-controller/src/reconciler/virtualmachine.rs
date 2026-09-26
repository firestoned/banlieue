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

use banlieue_api::banlieue::DEFAULT_USER_DATA_KEY;
use banlieue_api::banlieue::{
    InstallMode, Provider, VMClass, VMImage, VirtualMachine, VirtualMachineStatus,
};
use banlieue_api::common::{
    LocalObjectReference as _PlaceholderLocalRef, TypedObjectReference, condition_types,
};
use banlieue_api::infrastructure::{CloudHypervisorMachine, LibvirtMachine, VSphereMachine};
use banlieue_provider_sdk::{
    finalizer::{ensure_finalizer, remove_finalizer},
    guestdata::{GuestDataContext, render_placeholders},
    reconciler::{requeue_default, requeue_long, requeue_on_error},
    ssa::{FIELD_MANAGER_CONTROLLER, server_side_apply},
    status::{condition_status, set_condition},
};
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube::{
    Resource, ResourceExt,
    api::{Api, DeleteParams, ListParams, Patch, PatchParams},
    runtime::controller::Action,
};
use serde_json::json;
use tracing::{debug, info, warn};

use super::infra::{
    InfraKind, build_cloud_hypervisor_machine, build_libvirt_machine, build_vsphere_machine,
};
use super::migration::{MigrationAction, PlacementDriftReason, evaluate};
use super::scheduler::{ScheduleError, reasons, schedule};
use super::status_mirror::{InfraMachineRead, mirror_onto_vm, mirror_status_from_infra};
use crate::context::Context;
use crate::error::{Error, Result};

// (Silence the SDK re-export-induced unused warning on the
// `LocalObjectReference` placeholder import.)
#[allow(dead_code)]
type _Anchor = _PlaceholderLocalRef;

/// Finalizer set on every `VirtualMachine` reconciled by this controller.
pub const VM_FINALIZER: &str = "banlieue.io/virtualmachine";

/// Condition reason for a `VMClass`/`VMImage` pairing that cannot work
/// (ADR-0048).
pub const REASON_IMAGE_CLASS_MISMATCH: &str = "ImageClassMismatch";

/// `Some(message)` when this class/image pairing would attach a vTPM that
/// nothing ever seals to (ADR-0048).
///
/// Kairos partition encryption is install-phase-only: `kcrypt` seals LUKS
/// keys to the TPM during `kairos-agent install`, so the sealing can only
/// happen on a disk written *after* a per-VM vTPM exists (ADR-0040). An
/// [`InstallMode::Immediate`] template is installed once, centrally, and then
/// cloned — there is no per-clone install, so `install.encrypted_partitions`
/// has nothing to seal against. The combination clones fine, attaches a vTPM,
/// and silently encrypts nothing.
///
/// [`InstallMode::Manual`] passes deliberately: ADR-0040 defines it as
/// identical mechanics to [`InstallMode::Deferred`] for a build that is not
/// Kairos-driven, so a per-VM vTPM *does* exist when the disk is written.
/// banlieue cannot verify what such an image actually does with it, and
/// rejecting it would leave non-Kairos encrypted images no path at all.
///
/// Pure and backend-neutral — it reads only provider-agnostic fields, so
/// every backend is constrained identically without participating.
#[must_use]
pub fn image_class_mismatch(tpm_enabled: bool, mode: InstallMode) -> Option<&'static str> {
    (tpm_enabled && mode == InstallMode::Immediate).then_some(
        "VMClass.tpmEnabled requires a VMImage with installMode: Deferred; an \
         Immediate (pre-installed) template cannot be encrypted per VM, because \
         its disk was installed before any per-VM vTPM existed (ADR-0040, ADR-0048)",
    )
}

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
    let infra_apis = InfraApis::namespaced(&ctx.client, &namespace);

    if vm.metadata.deletion_timestamp.is_some() {
        return finalize_vm(&vm_api, &infra_apis, &vm).await;
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

    // ---- Validate the class/image pairing (ADR-0048) --------------------
    //
    // Checked here because this is the first point at which BOTH objects are
    // in hand — `tpmEnabled` lives on the VMClass and `installMode` on the
    // VMImage, so no admission policy can see the combination. Checked
    // *before* scheduling so a VM that can never be what it claims to be
    // never reaches a provider: a provider handed this machine would
    // cheerfully build the unencrypted thing.
    // No `template` means a `Template`/`BackingFile` source — a pre-built
    // disk, with no build step at all. That is a pre-laid disk in exactly
    // the sense ADR-0040 rules out, so it takes `InstallMode`'s own default
    // (`Immediate`) and is rejected on the same grounds. Fail closed: the
    // error of rejecting a sealable image is visible and fixable, while the
    // error of accepting an unsealable one is silent (ADR-0048 Decision 6).
    let install_mode = image
        .spec
        .template
        .as_ref()
        .map_or_else(InstallMode::default, |t| t.install_mode);

    if let Some(detail) = image_class_mismatch(class.spec.tpm_enabled, install_mode) {
        warn!(
            class = %vm.spec.class_ref.name,
            image = %vm.spec.image_ref.name,
            "class/image pairing would attach a vTPM that nothing seals to"
        );
        patch_image_class_mismatch(&vm_api, &vm, &name, generation, detail).await?;
        return Ok(requeue_long());
    }

    // ---- Schedule ------------------------------------------------------
    let decision = match schedule(&vm, &class, &image, &providers, &sibling_vms) {
        Ok(d) => d,
        Err(err) => {
            warn!(?err, "scheduling failed; surfacing condition");
            patch_scheduling_failure(&vm_api, &vm, &name, generation, &err).await?;
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

    // Which infra CR this VM becomes. A provider class with no builder here
    // is reported rather than ignored: the alternative is a VirtualMachine
    // that schedules successfully and then silently never gets an infra CR.
    let Some(infra_kind) = InfraKind::from_provider_class(&decision.provider_class) else {
        warn!(
            provider_class = %decision.provider_class,
            "no infrastructure builder for this provider class"
        );
        patch_infra_build_failure(
            &vm_api,
            &vm,
            &name,
            generation,
            &format!(
                "provider class '{}' has no infrastructure builder in banlieue-controller",
                decision.provider_class
            ),
        )
        .await?;
        return Ok(requeue_long());
    };

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
            return mirror_only_path(&vm_api, &infra_apis, infra_kind, &vm, &name).await;
        }
        MigrationAction::SurfaceOnly { reason } => {
            // migrationPolicy=Manual without the annotation. Set
            // PlacementValid=False; do NOT delete the infra CR yet.
            warn!(
                reason = reason.reason(),
                "placement drift; manual migration required (set annotation banlieue.io/migrate=true)"
            );
            patch_placement_invalid(&vm_api, &vm, &name, generation, reason).await?;
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
            delete_existing_infra(&infra_apis, infra_kind, &vm.name_any()).await?;
            patch_placement_invalid(&vm_api, &vm, &name, generation, reason).await?;
            return Ok(requeue_default());
        }
    }

    // ---- Resolve + render userData (ADR-0025) ---------------------------
    // Read here, not in the provider: banlieue-controller already has a
    // namespace-scoped Secret Role for this (deploy/controller/rbac/role.
    // yaml); the provider deliberately has none.
    let rendered_user_data = match resolve_rendered_user_data(&ctx, &namespace, &name, &vm).await {
        Ok(u) => u,
        Err(e) => {
            warn!(error = %e, "userData resolution failed");
            patch_infra_build_failure(&vm_api, &vm, &name, generation, &e.to_string()).await?;
            return Ok(requeue_on_error());
        }
    };

    // ---- Build + SSA the infra CR --------------------------------------
    // Both arms end at the same place: an applied object behind
    // `InfraMachineRead`, so everything below this match is backend-blind.
    let applied: Box<dyn InfraMachineRead + Send> = match infra_kind {
        InfraKind::VSphere => {
            let infra = match build_vsphere_machine(
                &vm,
                &class,
                &image,
                &decision,
                chosen_provider,
                rendered_user_data.as_deref(),
            ) {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "infra builder failed; reporting Scheduled=False");
                    patch_infra_build_failure(&vm_api, &vm, &name, generation, &e.to_string())
                        .await?;
                    return Ok(requeue_on_error());
                }
            };
            let m =
                server_side_apply(&infra_apis.vsphere, FIELD_MANAGER_CONTROLLER, &infra).await?;
            debug!(vsphere_machine = %m.name_any(), "applied VSphereMachine via SSA");
            Box::new(m)
        }
        InfraKind::Libvirt => {
            let infra = match build_libvirt_machine(
                &vm,
                &class,
                &image,
                &decision,
                chosen_provider,
                rendered_user_data.as_deref(),
            ) {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "infra builder failed; reporting Scheduled=False");
                    patch_infra_build_failure(&vm_api, &vm, &name, generation, &e.to_string())
                        .await?;
                    return Ok(requeue_on_error());
                }
            };
            let m =
                server_side_apply(&infra_apis.libvirt, FIELD_MANAGER_CONTROLLER, &infra).await?;
            debug!(libvirt_machine = %m.name_any(), "applied LibvirtMachine via SSA");
            Box::new(m)
        }
        InfraKind::CloudHypervisor => {
            let infra = match build_cloud_hypervisor_machine(
                &vm,
                &class,
                &image,
                &decision,
                chosen_provider,
                rendered_user_data.as_deref(),
            ) {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "infra builder failed; reporting Scheduled=False");
                    patch_infra_build_failure(&vm_api, &vm, &name, generation, &e.to_string())
                        .await?;
                    return Ok(requeue_on_error());
                }
            };
            let m = server_side_apply(
                &infra_apis.cloud_hypervisor,
                FIELD_MANAGER_CONTROLLER,
                &infra,
            )
            .await?;
            debug!(cloud_hypervisor_machine = %m.name_any(), "applied CloudHypervisorMachine via SSA");
            Box::new(m)
        }
    };

    // ---- Status mirror -------------------------------------------------
    // `mirror_status_from_infra`'s aggregate Ready computation reads the
    // `Scheduled` *condition* off `current.conditions` — not
    // `status.scheduled` (the struct patched below) — but the only two
    // places that ever set that condition are the failure paths
    // (`patch_scheduling_failure`, `patch_infra_build_failure`), both
    // `False`. Nothing on the success path ever set it `True`, so `Ready`
    // stayed stuck at `Scheduling` forever even once `status.scheduled` was
    // populated and `InfrastructureReady` was `True` (found live: two VMs
    // fully up and reachable in vCenter, `VirtualMachine.status` still
    // reporting `Ready=False reason=Scheduling`). Set it here, on the
    // pre-mirror `current` snapshot — not after calling `mirror_onto_vm` —
    // so the aggregate computation inside `mirror_status_from_infra`
    // actually sees it this same pass, rather than one reconcile late.
    // Reaching this line only happens via `schedule()` returning
    // `Ok(decision)` above, so it's always correct to set this here.
    let mut current_status = vm.status.clone().unwrap_or_default();
    set_condition(
        &mut current_status.conditions,
        condition_types::SCHEDULED,
        condition_status::TRUE,
        "Scheduled",
        "VirtualMachine scheduled successfully",
        generation,
    );
    let mut next_status = mirror_status_from_infra(&current_status, applied.as_ref(), generation);
    next_status.scheduled =
        Some(decision.to_scheduled_placement(Time(k8s_openapi::jiff::Timestamp::now())));
    // Both infra kinds live in the same API group and are named after their
    // parent VM, so only `kind` actually varies here.
    next_status.infrastructure_ref = Some(TypedObjectReference {
        api_group: VSphereMachine::group(&()).to_string(),
        kind: infra_kind.kind_name().to_string(),
        name: name.clone(),
        namespace: Some(namespace.clone()),
    });

    patch_status(&vm_api, &name, &next_status).await?;

    Ok(requeue_default())
}

/// Drift-but-Never path: don't touch the infra CR, just mirror its status
/// and keep the existing `status.scheduled` intact. The `PlacementValid`
/// condition is intentionally NOT set to False here because the spec says
/// drift is acceptable for this VM.
async fn mirror_only_path(
    vm_api: &Api<VirtualMachine>,
    apis: &InfraApis,
    infra_kind: InfraKind,
    vm: &VirtualMachine,
    name: &str,
) -> Result<Action> {
    let infra: Option<Box<dyn InfraMachineRead + Send>> = match infra_kind {
        InfraKind::VSphere => apis
            .vsphere
            .get_opt(name)
            .await?
            .map(|m| Box::new(m) as Box<dyn InfraMachineRead + Send>),
        InfraKind::Libvirt => apis
            .libvirt
            .get_opt(name)
            .await?
            .map(|m| Box::new(m) as Box<dyn InfraMachineRead + Send>),
        InfraKind::CloudHypervisor => apis
            .cloud_hypervisor
            .get_opt(name)
            .await?
            .map(|m| Box::new(m) as Box<dyn InfraMachineRead + Send>),
    };
    let Some(infra) = infra else {
        // The infra CR vanished out from under us (manual delete, GC
        // cascade, etc). Drop back to the normal apply path on the
        // next reconcile.
        return Ok(requeue_on_error());
    };
    // mirror_status_from_infra leaves `scheduled`/`infrastructureRef`
    // untouched from `current` — exactly the "keep the existing placement"
    // contract this path wants — so `next_status` already carries them
    // forward unchanged; nothing extra to set here.
    let next_status = mirror_onto_vm(vm, infra.as_ref());
    patch_status(vm_api, name, &next_status).await?;
    Ok(requeue_default())
}

/// Delete the owned infra CR of `infra_kind` by name. 404 is treated as
/// success so the call is idempotent across retries.
async fn delete_existing_infra(apis: &InfraApis, infra_kind: InfraKind, name: &str) -> Result<()> {
    match infra_kind {
        InfraKind::VSphere => delete_ignoring_404(&apis.vsphere, name).await,
        InfraKind::Libvirt => delete_ignoring_404(&apis.libvirt, name).await,
        InfraKind::CloudHypervisor => delete_ignoring_404(&apis.cloud_hypervisor, name).await,
    }
}

/// The typed API for every infra kind, in one namespace.
///
/// One value to pass around instead of one parameter per backend, so a new
/// backend is a field here rather than a new argument on every function
/// that might touch an infra CR.
struct InfraApis {
    vsphere: Api<VSphereMachine>,
    libvirt: Api<LibvirtMachine>,
    cloud_hypervisor: Api<CloudHypervisorMachine>,
}

impl InfraApis {
    fn namespaced(client: &kube::Client, namespace: &str) -> Self {
        Self {
            vsphere: Api::namespaced(client.clone(), namespace),
            libvirt: Api::namespaced(client.clone(), namespace),
            cloud_hypervisor: Api::namespaced(client.clone(), namespace),
        }
    }
}

/// Delete `name` through `api`, treating "already gone" as success.
async fn delete_ignoring_404<K>(api: &Api<K>, name: &str) -> Result<()>
where
    K: kube::Resource + Clone + serde::de::DeserializeOwned + std::fmt::Debug,
{
    use kube::Error as KubeError;
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => Ok(()),
        Err(KubeError::Api(e)) if e.code == 404 => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Deletion path with cascade-wait on the owned infra CR.
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
///
/// # Why every kind is checked rather than the scheduled one
///
/// This runs before scheduling, and deliberately does not consult
/// `status.infrastructureRef` or the `Provider`. At deletion time the
/// Provider CR may already be gone, the status may never have been written
/// (a VM deleted mid-first-reconcile), or the VM may have been migrated
/// between backends. Every one of those makes a "which kind was it?" answer
/// wrong, and being wrong here means dropping the finalizer while a real VM
/// is still running on a hypervisor. One `get_opt` per kind is the cheap price
/// of never leaking one.
async fn finalize_vm(
    api: &Api<VirtualMachine>,
    apis: &InfraApis,
    vm: &VirtualMachine,
) -> Result<Action> {
    info!("finalizing VirtualMachine");
    let owned_name = vm.name_any();

    // (exists, already terminating) for each backend.
    let vsphere = apis
        .vsphere
        .get_opt(&owned_name)
        .await?
        .map(|m| m.metadata.deletion_timestamp.is_some());
    let libvirt = apis
        .libvirt
        .get_opt(&owned_name)
        .await?
        .map(|m| m.metadata.deletion_timestamp.is_some());
    let cloud_hypervisor = apis
        .cloud_hypervisor
        .get_opt(&owned_name)
        .await?
        .map(|m| m.metadata.deletion_timestamp.is_some());

    if vsphere.is_none() && libvirt.is_none() && cloud_hypervisor.is_none() {
        info!("infra CRs cleared; removing VirtualMachine finalizer");
        remove_finalizer(api, vm, VM_FINALIZER).await?;
        return Ok(requeue_default());
    }

    // Issue a delete for anything present that is not already terminating.
    // The provider's own finalizer keeps it around until the backend VM is
    // really gone; this is never `|| true`-ed, so a failure surfaces instead
    // of leaving a guest running while teardown claims success.
    if vsphere == Some(false) {
        info!(vsphere_machine = %owned_name, "requesting VSphereMachine deletion; waiting for cascade");
        delete_ignoring_404(&apis.vsphere, &owned_name).await?;
    }
    if libvirt == Some(false) {
        info!(libvirt_machine = %owned_name, "requesting LibvirtMachine deletion; waiting for cascade");
        delete_ignoring_404(&apis.libvirt, &owned_name).await?;
    }
    if cloud_hypervisor == Some(false) {
        info!(cloud_hypervisor_machine = %owned_name, "requesting CloudHypervisorMachine deletion; waiting for cascade");
        delete_ignoring_404(&apis.cloud_hypervisor, &owned_name).await?;
    }
    if vsphere == Some(true) || libvirt == Some(true) || cloud_hypervisor == Some(true) {
        debug!("infra CR still terminating; will recheck");
    }
    Ok(requeue_on_error())
}

/// Patch the VirtualMachine status with `Scheduled=False reason=<reason>`
/// for a failed scheduling attempt. Starts from `vm.status` (not
/// `Vec::new()`) and patches the whole status object — a conditions-only
/// narrower apply from the same field manager as the full-status success
/// path would otherwise make the apiserver retract, and SSA then wipe,
/// every field this failure patch doesn't mention (`initialization`,
/// `scheduled`, `observedPowerState`, ...) — the same bug class ADR-0034
/// found live in the vsphere provider's own status patching.
async fn patch_scheduling_failure(
    api: &Api<VirtualMachine>,
    vm: &VirtualMachine,
    name: &str,
    generation: i64,
    err: &ScheduleError,
) -> Result<()> {
    let mut status = vm.status.clone().unwrap_or_default();
    set_condition(
        &mut status.conditions,
        condition_types::SCHEDULED,
        condition_status::FALSE,
        err.reason(),
        err.to_string(),
        generation,
    );
    set_condition(
        &mut status.conditions,
        condition_types::READY,
        condition_status::FALSE,
        "Scheduling",
        "scheduling not yet successful",
        generation,
    );
    status.observed_generation = Some(generation);
    patch_status(api, name, &status).await
}

/// Resolve `vm.spec.userData`'s Secret or ConfigMap and render the fixed
/// ADR-0024 placeholder set into it (ADR-0025 — done here, not by the
/// provider, which has no RBAC for arbitrary Secrets / ConfigMaps).
/// `None` when `spec.userData` is unset. The substitution context's static
/// network values come from the first entry in `spec.networkOverrides`, if
/// any — the same "first static interface wins" rule `build_guestinfo`
/// (vsphere provider) uses, since `guestinfo.network.*` is a flat,
/// non-indexed convention.
async fn resolve_rendered_user_data(
    ctx: &Context,
    namespace: &str,
    vm_name: &str,
    vm: &VirtualMachine,
) -> Result<Option<String>> {
    let Some(user_data) = &vm.spec.user_data else {
        return Ok(None);
    };
    if let Err(msg) = user_data.validate() {
        return Err(Error::Missing(msg));
    }

    let raw = if let Some(ref sel) = user_data.secret_ref {
        resolve_secret_data(ctx, namespace, sel).await?
    } else if let Some(ref sel) = user_data.config_map_ref {
        resolve_configmap_data(ctx, namespace, sel).await?
    } else {
        // Unreachable after validate(), but explicit for safety.
        return Err(Error::Missing(
            "userData: exactly one of secretRef, configMapRef must be set",
        ));
    };

    let static_cfg = vm.spec.network_overrides.first().map(|o| &o.static_);
    let gd_ctx = GuestDataContext::from_static(vm_name, static_cfg);
    Ok(Some(render_placeholders(&raw, &gd_ctx)))
}

/// Read a single key from a Secret.
async fn resolve_secret_data(
    ctx: &Context,
    namespace: &str,
    sel: &banlieue_api::common::KeySelector,
) -> Result<String> {
    let api: Api<Secret> = Api::namespaced(ctx.client.clone(), namespace);
    let secret = api.get(&sel.name).await.map_err(|e| {
        if let kube::Error::Api(api_err) = &e
            && api_err.code == 404
        {
            return Error::Missing("VirtualMachine.spec.userData.secretRef");
        }
        Error::Kube(e)
    })?;
    let key = sel.key_or(DEFAULT_USER_DATA_KEY);
    let data = secret.data.unwrap_or_default();
    let raw = data
        .get(key)
        .ok_or(Error::Missing("userData secret.data[key]"))?;
    String::from_utf8(raw.0.clone())
        .map_err(|_| Error::Missing("userData secret.data[key] (not utf-8)"))
}

/// Read a single key from a ConfigMap.
async fn resolve_configmap_data(
    ctx: &Context,
    namespace: &str,
    sel: &banlieue_api::common::KeySelector,
) -> Result<String> {
    let api: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), namespace);
    let cm = api.get(&sel.name).await.map_err(|e| {
        if let kube::Error::Api(api_err) = &e
            && api_err.code == 404
        {
            return Error::Missing("VirtualMachine.spec.userData.configMapRef");
        }
        Error::Kube(e)
    })?;
    let key = sel.key_or(DEFAULT_USER_DATA_KEY);
    let data = cm.data.unwrap_or_default();
    data.get(key)
        .cloned()
        .ok_or(Error::Missing("userData configMap.data[key]"))
}

/// Starts from `vm.status`, not `Vec::new()` — see `patch_scheduling_failure`'s
/// doc comment for why a conditions-only narrower patch from the same field
/// manager as the full-status success path is unsafe under SSA.
/// Patch `Ready=False` for a class/image pairing that cannot work (ADR-0048).
///
/// Deliberately does **not** touch `Scheduled`: this VM never reached the
/// scheduler, so claiming anything about its placement would be a second
/// falsehood on top of the one being reported.
async fn patch_image_class_mismatch(
    api: &Api<VirtualMachine>,
    vm: &VirtualMachine,
    name: &str,
    generation: i64,
    detail: &str,
) -> Result<()> {
    let mut status = vm.status.clone().unwrap_or_default();
    set_condition(
        &mut status.conditions,
        condition_types::READY,
        condition_status::FALSE,
        REASON_IMAGE_CLASS_MISMATCH,
        detail,
        generation,
    );
    status.observed_generation = Some(generation);
    patch_status(api, name, &status).await
}

async fn patch_infra_build_failure(
    api: &Api<VirtualMachine>,
    vm: &VirtualMachine,
    name: &str,
    generation: i64,
    detail: &str,
) -> Result<()> {
    let mut status = vm.status.clone().unwrap_or_default();
    set_condition(
        &mut status.conditions,
        condition_types::SCHEDULED,
        condition_status::FALSE,
        "InfraBuildFailed",
        detail,
        generation,
    );
    set_condition(
        &mut status.conditions,
        condition_types::READY,
        condition_status::FALSE,
        "InfraBuildFailed",
        "could not construct provider infrastructure CR",
        generation,
    );
    status.observed_generation = Some(generation);
    patch_status(api, name, &status).await
}

/// Patch `PlacementValid=False` with the drift reason. Also marks `Ready=False`
/// with reason `PlacementInvalid`. Leaves `status.scheduled` untouched — the
/// previously-recorded placement stays visible until the next pass either
/// recreates (Automatic) or the user resolves drift manually (Manual).
/// Starts from `vm.status`, not `Vec::new()` — see `patch_scheduling_failure`'s
/// doc comment for why a conditions-only narrower patch from the same field
/// manager as the full-status success path is unsafe under SSA.
async fn patch_placement_invalid(
    api: &Api<VirtualMachine>,
    vm: &VirtualMachine,
    name: &str,
    generation: i64,
    reason: &PlacementDriftReason,
) -> Result<()> {
    let mut status = vm.status.clone().unwrap_or_default();
    set_condition(
        &mut status.conditions,
        condition_types::PLACEMENT_VALID,
        condition_status::FALSE,
        reason.reason(),
        reason.message(),
        generation,
    );
    set_condition(
        &mut status.conditions,
        condition_types::READY,
        condition_status::FALSE,
        "PlacementInvalid",
        "current placement no longer satisfies the spec",
        generation,
    );
    status.observed_generation = Some(generation);
    patch_status(api, name, &status).await
}

/// Apply the *entire* status object, serialized as-is — never a
/// hand-picked subset of fields. Found live: an earlier version of this
/// function sent only `{scheduled, infrastructureRef, conditions,
/// observedGeneration}`, silently dropping `initialization`/`addresses`
/// (`VirtualMachine.status.initialization` stayed `{}` forever, even fully
/// provisioned) and, once ADR-0034 added it, `observedPowerState` too.
/// Beyond just dropping fields on write, the SAME field manager
/// (`FIELD_MANAGER_CONTROLLER`) re-applying a *narrower* field set on a
/// later pass makes the apiserver retract this manager's ownership of every
/// field the narrower payload omits — the identical class of bug found in
/// the vsphere provider's own `refresh_power_state` (ADR-0034). Every
/// caller must build its full intended `VirtualMachineStatus` (including
/// `scheduled`/`infrastructureRef`, which `mirror_status_from_infra`
/// otherwise leaves untouched from `current`) and pass it here whole.
async fn patch_status(
    api: &Api<VirtualMachine>,
    name: &str,
    status: &VirtualMachineStatus,
) -> Result<()> {
    let patch = json!({
        "apiVersion": format!("{}/{}", VirtualMachine::group(&()), VirtualMachine::version(&())),
        "kind": "VirtualMachine",
        "status": status,
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
