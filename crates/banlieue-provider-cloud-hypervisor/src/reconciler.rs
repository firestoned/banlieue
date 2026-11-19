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
use crate::plan::{PlanError, UidLedger, plan_machine};
use crate::provider::{HostCaHold, current_host_ca_hold};
use banlieue_api::capi::{
    BOOTSTRAP_DATA_READY, bootstrap_reasons, holds_guest, retain_provider_conditions,
};
use banlieue_api::common::FirstBootSealing;
use banlieue_api::infrastructure::{CloudHypervisorMachine, CloudHypervisorMachineStatus};
use banlieue_provider_sdk::finalizer::{ensure_finalizer, remove_finalizer};
use banlieue_provider_sdk::reconciler::{requeue_default, requeue_long, requeue_on_error};
use banlieue_provider_sdk::scope::{ProviderScope, SharedServing};
use banlieue_provider_sdk::sealing::{SealingGate, sealing_gate};
use banlieue_provider_sdk::ssa::{apply_provider_id, apply_status};
use k8s_openapi::jiff::Timestamp;
use kube::api::{Api, ListParams};
use kube::runtime::controller::Action;
use kube::{Client, Resource, ResourceExt};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Finalizer held while the machine has anything on the host.
pub const MACHINE_FINALIZER: &str = "banlieue.io/cloudhypervisormachine";
/// How soon to look again while the VMM or the guest is changing state.
/// A VMM start is four or five quick steps, one per pass (manufacture,
/// swtpm, VMM unit, API socket), each well under a second; at 3 s a pass
/// they cost a sealed clone about 8 s of waiting (roadmap 18 phase C).
const REQUEUE_TRANSITION: Duration = Duration::from_secs(1);

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
    /// Guest uids handed out by this process, so concurrent reconciles
    /// never share one.
    pub uids: UidLedger,
    /// What this host serves: its home namespace and, with a selector, the
    /// namespaces whose labels match (ADR-0134).
    pub scope: ProviderScope,
    /// The scope plus the gate over the selector; answers "is this namespace
    /// served" for a reconcile.
    pub serving: SharedServing,
}

impl Context {
    /// An [`Api`] over this host's machines: the home namespace without a
    /// selector, every namespace with one (the caller gates the result).
    #[must_use]
    pub fn machines(&self) -> Api<CloudHypervisorMachine> {
        self.scope.api(&self.client)
    }
}

/// Whether `machine` is for this host: its `providerRef` resolves to this
/// host's `Provider` in its home namespace, by name AND namespace. A bare
/// `providerRef` resolves in the machine's own namespace, so a tenant machine
/// that merely names this host by name is not ours (ADR-0133 Decision 5,
/// ADR-0134). A machine with no namespace is nobody's.
#[must_use]
pub fn is_ours(machine: &CloudHypervisorMachine, config: &HostConfig) -> bool {
    let Some(namespace) = machine.namespace() else {
        return false;
    };
    banlieue_provider_sdk::reconciler::serves_machine(
        Some(&config.provider.name),
        &config.provider.namespace,
        &namespace,
        &machine.spec.provider_ref,
    )
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
///
/// `sealing_due` is [`sealing_report_due`] for the status just built: while a
/// first-boot sealing report is still due, a running guest is looked at on
/// the default cadence rather than the long one. The report itself wakes the
/// reconciler (ADR-0112 Decision 1), but nothing wakes it at the deadline,
/// and a clone that cannot seal never reports.
#[must_use]
pub fn requeue_for(o: &Observed, sealing_due: bool) -> Action {
    match o.phase {
        Phase::StartingTpm | Phase::StartingVmm | Phase::Stopping => {
            Action::requeue(REQUEUE_TRANSITION)
        }
        // Still waiting for DHCP: the address is what users look for next.
        Phase::Running if o.addresses.is_empty() => requeue_default(),
        // Still waiting for a sealing report, or for its deadline.
        Phase::Running if sealing_due => requeue_default(),
        Phase::Running | Phase::Stopped => requeue_long(),
    }
}

/// Whether a first-boot sealing report is still due for a machine whose
/// spec declares `required` and whose status is `st`, at `now`: declared,
/// not yet complete, and not yet past its deadline (ADR-0088 Decision 5).
#[must_use]
pub fn sealing_report_due(
    required: Option<&FirstBootSealing>,
    st: &CloudHypervisorMachineStatus,
    now: Timestamp,
) -> bool {
    required.is_some_and(|req| {
        matches!(
            sealing_gate(req, &st.sealed_partitions, &st.conditions, now),
            SealingGate::Pending { .. }
        )
    })
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
    // A namespace the selector does not serve is left alone, unless the
    // machine is being deleted: its finalizer must still run (ADR-0130
    // Decision 6).
    if let Some(action) = ctx
        .serving
        .skip_unserved(&ns, machine.metadata.deletion_timestamp.is_some())
    {
        return Ok(action);
    }
    let name = machine.name_any();
    let uid = machine.uid().ok_or(Error::Missing("metadata.uid"))?;
    let generation = machine.metadata.generation.unwrap_or(0);
    let api: Api<CloudHypervisorMachine> = Api::namespaced(ctx.client.clone(), &ns);
    // This host's own manager (ADR-0087): computed once so every apply below
    // writes under the same identity.
    let field_manager = ctx.config.field_manager();
    let previous = machine.status.as_ref();

    if machine.metadata.deletion_timestamp.is_some() {
        return finalize(&api, &machine, &ctx, &uid).await;
    }
    ensure_finalizer(&api, machine.as_ref(), MACHINE_FINALIZER)
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;

    // A Cluster API machine's guest waits for its bootstrap data (ADR-0129):
    // nothing is created on the host, not even a guest uid, until the
    // machine is owned by a Machine and the controller reports the data
    // delivered. Checked before the uid, so a machine that already has one,
    // or a providerID (a guest created before an upgrade, or a machine moved
    // or restored without its status), is never held.
    if holds_guest(machine.as_ref()) {
        let st = held_status(previous, generation);
        patch_status(&api, &name, &st, &field_manager).await?;
        return Ok(Action::await_change());
    }

    // ADR-0146 Decision 4: an opted-in host with no readable hostCa creates
    // no guest. Checked before the uid is allocated; a host that did not opt
    // in costs no extra API call.
    if holds_new_guest(previous)
        && let Some(hold) = current_host_ca_hold(&ctx).await?
    {
        warn!(machine = %name, reason = hold.reason(), "host CA not asserted; holding the guest");
        let st = host_ca_held_status(previous, &hold, generation);
        patch_status(&api, &name, &st, &field_manager).await?;
        // Nothing wakes this machine when the Provider gains its hostCa, so
        // look again on the default cadence.
        return Ok(requeue_default());
    }

    // Record the guest uid before anything is created on the host for it.
    let Some(host_uid) = previous.and_then(|s| s.host_uid) else {
        // Every namespace this host serves: guest uids are one pool per host.
        let all = ctx.machines().list(&ListParams::default()).await?.items;
        let used = used_host_uids(&all, &ctx.config, &uid);
        let Some(host_uid) = ctx.uids.assign(&uid, &used, ctx.config.guests) else {
            let st = failure_status(
                previous,
                &Error::UidRangeFull.to_string(),
                "GuestUidRangeFull",
                generation,
            );
            patch_status(&api, &name, &st, &field_manager).await?;
            return Ok(requeue_long());
        };
        let st = CloudHypervisorMachineStatus {
            host_uid: Some(host_uid),
            ..previous.cloned().unwrap_or_default()
        };
        patch_status(&api, &name, &st, &field_manager).await?;
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
            patch_status(&api, &name, &st, &field_manager).await?;
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
        patch_provider_id(&api, &name, &plan.provider_id, &field_manager).await?;
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
            patch_status(&api, &name, &st, &field_manager).await?;
            let sealing_due = sealing_report_due(plan.first_boot_sealing.as_ref(), &st, o.now);
            Ok(requeue_for(&o, sealing_due))
        }
        Err(e) => {
            warn!(machine = %name, error = %e, "converge failed");
            let st = failure_status(previous, &e.to_string(), failure_reason(&e), generation);
            patch_status(&api, &name, &st, &field_manager).await?;
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
    // Nothing is left on the host for it, so its uid is free again.
    ctx.uids.release(uid);
    Ok(Action::await_change())
}

/// Whether a machine without a running guest must wait on the host CA hold
/// (ADR-0146 Decision 4): everything that is not already `Ready=True`. A
/// guest that is up is left alone; the hold stops guests being *created*.
#[must_use]
pub fn holds_new_guest(previous: Option<&CloudHypervisorMachineStatus>) -> bool {
    !previous.is_some_and(|s| {
        s.conditions
            .iter()
            .any(|c| c.type_ == "Ready" && c.status == "True")
    })
}

/// The status a machine reports while the host CA hold is in force:
/// `Ready=False` with the hold's reason, nothing provisioned and no
/// attestation subject published.
#[must_use]
pub fn host_ca_held_status(
    previous: Option<&CloudHypervisorMachineStatus>,
    hold: &HostCaHold,
    generation: i64,
) -> CloudHypervisorMachineStatus {
    let mut st = failure_status(previous, &hold.message(), hold.reason(), generation);
    st.attestation_subject = None;
    st
}

/// Held, and banlieue-controller has reported on the machine.
const HELD_MESSAGE: &str =
    "waiting for the Cluster API Machine's bootstrap data; see the BootstrapDataReady condition";

/// Held, and banlieue-controller has never reported on the machine.
const HELD_UNSERVED_MESSAGE: &str = "waiting for the Cluster API Machine's bootstrap data; \
     banlieue-controller has not reported BootstrapDataReady on this machine: check that it \
     is running, that its namespace selector serves this namespace, and that Cluster API \
     serves v1beta2 Machines";

/// The status a held machine reports: not provisioned, `Ready=False` with
/// `WaitingForBootstrapData`. With no `BootstrapDataReady` at all, the
/// controller has never looked at the machine, and the message names what
/// to check.
#[must_use]
pub fn held_status(
    previous: Option<&CloudHypervisorMachineStatus>,
    generation: i64,
) -> CloudHypervisorMachineStatus {
    let reported =
        previous.is_some_and(|s| s.conditions.iter().any(|c| c.type_ == BOOTSTRAP_DATA_READY));
    let message = if reported {
        HELD_MESSAGE
    } else {
        HELD_UNSERVED_MESSAGE
    };
    failure_status(
        previous,
        message,
        bootstrap_reasons::WAITING_FOR_BOOTSTRAP_DATA,
        generation,
    )
}

/// The server-side apply body for this provider's status. The controller's
/// `BootstrapDataReady` condition is dropped: the provider applies its whole
/// status from a snapshot, and must neither own that entry nor overwrite it
/// with a stale copy (ADR-0129 Decision 6).
#[must_use]
pub fn status_patch(status: &CloudHypervisorMachineStatus) -> Value {
    let mut status = status.clone();
    retain_provider_conditions(&mut status.conditions);
    json!({
        "apiVersion": CloudHypervisorMachine::api_version(&()),
        "kind": CloudHypervisorMachine::kind(&()),
        "status": status,
    })
}

async fn patch_status(
    api: &Api<CloudHypervisorMachine>,
    name: &str,
    status: &CloudHypervisorMachineStatus,
    field_manager: &str,
) -> Result<()> {
    // Scoped per ADR-0087. A CloudHypervisorMachine is reconciled by the
    // one host that owns it, so force remains correct. The status applied is
    // the one `status_patch` builds, without the controller's condition.
    let owned = &status_patch(status)["status"];
    apply_status(api, name, field_manager, owned, true).await?;
    Ok(())
}

/// CAPI puts `providerID` on spec and the provider sets it. Applied as its
/// own field under the provider's field manager, so it never contends with
/// the controller's ownership of the rest of spec.
async fn patch_provider_id(
    api: &Api<CloudHypervisorMachine>,
    name: &str,
    id: &str,
    field_manager: &str,
) -> Result<()> {
    apply_provider_id(api, name, field_manager, id).await?;
    Ok(())
}

#[cfg(test)]
#[path = "reconciler_tests.rs"]
mod reconciler_tests;
