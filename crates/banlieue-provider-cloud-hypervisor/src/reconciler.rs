// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The `CloudHypervisorMachine` reconciler: Kubernetes glue around
//! [`machine::converge`](crate::machine::converge).
//!
//! Per pass:
//!
//! 1. skip machines that belong to another host (ADR-0060: one Provider, one
//!    host);
//! 2. on deletion, tear down on the host, verified, then drop the finalizer;
//! 3. make sure the machine has a guest uid **recorded in status before**
//!    anything is created for it, so a crash cannot leave host objects owned
//!    by a uid nobody remembers;
//! 4. plan, converge one step, publish status and `spec.providerID`.
//!
//! The decisions are pure functions below, unit-tested; the loop is covered
//! by the end-to-end run.

use crate::error::{Error, Result};
use crate::host::HostOps;
use crate::host_config::HostConfig;
use crate::machine::{Observed, Phase, build_status, converge, failure_status, teardown};
use crate::plan::{PlanError, allocate_host_uid, plan_machine};
use banlieue_api::infrastructure::{CloudHypervisorMachine, CloudHypervisorMachineStatus};
use banlieue_provider_sdk::finalizer::{ensure_finalizer, remove_finalizer};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use banlieue_provider_sdk::ssa::FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR;
use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::{Client, ResourceExt};
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Finalizer held while the machine has anything on the host.
pub const MACHINE_FINALIZER: &str = "banlieue.io/cloudhypervisormachine";
/// How soon to look again while the VMM or the guest is changing state.
const REQUEUE_TRANSITION: Duration = Duration::from_secs(3);

/// What every reconcile needs.
pub struct Context {
    /// Kubernetes client.
    pub client: Client,
    /// This host's config.
    pub config: Arc<HostConfig>,
    /// The host.
    pub host: Arc<dyn HostOps>,
    /// The provider's gid, shared with guests (ADR-0063 Decision 5).
    pub group: u32,
}

/// Whether `machine` is for this host: its Provider is ours and it lives in
/// our Provider's namespace.
#[must_use]
pub fn is_ours(machine: &CloudHypervisorMachine, config: &HostConfig) -> bool {
    machine.spec.provider_ref.name == config.provider.name
        && machine.namespace().as_deref() == Some(config.provider.namespace.as_str())
}

/// Guest uids already recorded by this host's machines, other than `except`.
#[must_use]
pub fn used_host_uids(
    machines: &[CloudHypervisorMachine],
    config: &HostConfig,
    except: &str,
) -> BTreeSet<u32> {
    machines
        .iter()
        .filter(|m| is_ours(m, config))
        .filter(|m| m.uid().as_deref() != Some(except))
        .filter_map(|m| m.status.as_ref().and_then(|s| s.host_uid))
        .collect()
}

/// How soon to reconcile again after a pass that observed `o`.
#[must_use]
pub fn requeue_for(o: &Observed) -> Action {
    match o.phase {
        Phase::StartingTpm | Phase::StartingVmm | Phase::Stopping => {
            Action::requeue(REQUEUE_TRANSITION)
        }
        // Still waiting for DHCP: the address is what users look for next.
        Phase::Running if o.addresses.is_empty() => requeue_default(),
        Phase::Running | Phase::Stopped => requeue_long(),
    }
}

/// The `Ready=False` reason for an error.
#[must_use]
pub fn failure_reason(e: &Error) -> &'static str {
    match e {
        Error::Plan(PlanError::Unsupported(_)) => "Unsupported",
        Error::Plan(_) => "InvalidSpec",
        Error::Vmm(banlieue_cloud_hypervisor::Error::VersionUnsupported { .. }) => {
            "VmmVersionUnsupported"
        }
        Error::Vmm(_) => "VmmError",
        Error::VmmExited(_) => "VmmExited",
        Error::Systemd(_) => "SystemdError",
        Error::UidRangeFull => "GuestUidRangeFull",
        _ => "HostError",
    }
}

/// Whether an error will not go away by retrying soon.
#[must_use]
pub fn is_permanent(e: &Error) -> bool {
    matches!(e, Error::Plan(_) | Error::UidRangeFull)
}

/// Reconcile one machine.
///
/// # Errors
/// Kubernetes API errors; host failures are reported on status instead.
pub async fn reconcile(machine: Arc<CloudHypervisorMachine>, ctx: Arc<Context>) -> Result<Action> {
    if !is_ours(&machine, &ctx.config) {
        return Ok(Action::await_change());
    }
    let ns = machine
        .namespace()
        .ok_or(Error::Missing("metadata.namespace"))?;
    let name = machine.name_any();
    let uid = machine.uid().ok_or(Error::Missing("metadata.uid"))?;
    let generation = machine.metadata.generation.unwrap_or(0);
    let api: Api<CloudHypervisorMachine> = Api::namespaced(ctx.client.clone(), &ns);
    let previous = machine.status.as_ref();

    if machine.metadata.deletion_timestamp.is_some() {
        return finalize(&api, &machine, &ctx, &uid).await;
    }
    ensure_finalizer(&api, machine.as_ref(), MACHINE_FINALIZER)
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;

    // Record the guest uid before anything is created on the host for it.
    let Some(host_uid) = previous.and_then(|s| s.host_uid) else {
        let all = api.list(&ListParams::default()).await?.items;
        let used = used_host_uids(&all, &ctx.config, &uid);
        let Some(host_uid) = allocate_host_uid(&used, ctx.config.guests) else {
            let st = failure_status(
                previous,
                &Error::UidRangeFull.to_string(),
                "GuestUidRangeFull",
                generation,
            );
            patch_status(&api, &name, &st).await?;
            return Ok(requeue_long());
        };
        let st = CloudHypervisorMachineStatus {
            host_uid: Some(host_uid),
            ..previous.cloned().unwrap_or_default()
        };
        patch_status(&api, &name, &st).await?;
        info!(machine = %name, host_uid, "guest uid allocated");
        return Ok(Action::requeue(Duration::ZERO));
    };

    let plan = match plan_machine(
        &uid,
        &name,
        &ctx.config.provider.name,
        &machine.spec,
        &ctx.config,
        host_uid,
    ) {
        Ok(p) => p,
        Err(e) => {
            let e = Error::Plan(e);
            warn!(machine = %name, error = %e, "cannot plan machine");
            let st = failure_status(previous, &e.to_string(), failure_reason(&e), generation);
            patch_status(&api, &name, &st).await?;
            return Ok(requeue_long());
        }
    };

    // Once ejected, the installer stays out of every later start
    // (ADR-0065 Decision 4).
    let plan = if previous.and_then(|p| p.install_media_detached) == Some(true) {
        plan.without_install_media()
    } else {
        plan
    };

    if machine.spec.provider_id.as_deref() != Some(plan.provider_id.as_str()) {
        patch_provider_id(&api, &name, &plan.provider_id).await?;
    }

    match converge(
        ctx.host.as_ref(),
        &plan,
        &name,
        machine.spec.user_data.as_deref(),
        &machine.spec.desired_power_state,
    )
    .await
    {
        Ok(o) => {
            let st = build_status(previous, &o, &plan, &ctx.config.provider.name, generation);
            patch_status(&api, &name, &st).await?;
            Ok(requeue_for(&o))
        }
        Err(e) => {
            warn!(machine = %name, error = %e, "converge failed");
            let st = failure_status(previous, &e.to_string(), failure_reason(&e), generation);
            patch_status(&api, &name, &st).await?;
            Ok(if is_permanent(&e) {
                requeue_long()
            } else {
                requeue_on_error()
            })
        }
    }
}

async fn finalize(
    api: &Api<CloudHypervisorMachine>,
    machine: &CloudHypervisorMachine,
    ctx: &Context,
    uid: &str,
) -> Result<Action> {
    // No guest uid recorded means nothing was ever created on the host.
    if let Some(host_uid) = machine.status.as_ref().and_then(|s| s.host_uid) {
        match plan_machine(
            uid,
            &machine.name_any(),
            &ctx.config.provider.name,
            &machine.spec,
            &ctx.config,
            host_uid,
        ) {
            Ok(plan) => {
                info!(machine = %machine.name_any(), "tearing down on the host");
                teardown(ctx.host.as_ref(), &plan).await?;
            }
            // An unplannable spec never got past planning, so the only
            // thing that can exist is a unit started before the spec went
            // bad; its name depends only on the recorded guest uid.
            Err(e) => {
                warn!(error = %e, "spec no longer plans; stopping the unit by name only");
                ctx.host
                    .stop_unit(&crate::plan::instance_unit(
                        crate::plan::VMM_TEMPLATE,
                        &host_uid.to_string(),
                    ))
                    .await?;
            }
        }
    }
    remove_finalizer(api, machine, MACHINE_FINALIZER)
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
    Ok(Action::await_change())
}

async fn patch_status(
    api: &Api<CloudHypervisorMachine>,
    name: &str,
    status: &CloudHypervisorMachineStatus,
) -> Result<()> {
    let patch = json!({
        "apiVersion": "infrastructure.banlieue.io/v1alpha1",
        "kind": "CloudHypervisorMachine",
        "status": status,
    });
    api.patch_status(
        name,
        &PatchParams::apply(FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR).force(),
        &Patch::Apply(&patch),
    )
    .await?;
    Ok(())
}

/// CAPI puts `providerID` on spec and the provider sets it. Applied as its
/// own field under the provider's field manager, so it never contends with
/// the controller's ownership of the rest of spec.
async fn patch_provider_id(api: &Api<CloudHypervisorMachine>, name: &str, id: &str) -> Result<()> {
    let patch = json!({
        "apiVersion": "infrastructure.banlieue.io/v1alpha1",
        "kind": "CloudHypervisorMachine",
        "spec": { "providerID": id },
    });
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR),
        &Patch::Apply(&patch),
    )
    .await?;
    Ok(())
}

/// Requeue after a reconcile error.
pub fn error_policy(_m: Arc<CloudHypervisorMachine>, e: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %e, "CloudHypervisorMachine reconcile error");
    requeue_on_error()
}

#[cfg(test)]
#[path = "reconciler_tests.rs"]
mod reconciler_tests;
