// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Converging one `CloudHypervisorMachine`, and tearing it down.
//!
//! [`converge`] advances a machine by at most one step per call and returns
//! what it observed; the reconciler calls it again. Steps, in order:
//!
//! 1. host files: directories, OS disk (cloned and grown, or empty for
//!    `Deferred`), the machine's own copy of its installer while it is
//!    attached (deleted once ejected), NoCloud seed;
//! 2. taps, owned by the guest's uid and joined to their bridges;
//!    with a vTPM (ADR-0065): manufacture it once in its own unit, hand the
//!    state to the guest, and start its swtpm before the VMM;
//! 3. the listener for the guest's vsock report (ADR-0065 Decision 5), then
//!    the VMM's unit (an instance of `banlieue-ch@.service`); a failed one
//!    is cleared and restarted;
//! 4. wait for the API socket;
//! 5. version gate (ADR-0061 Decision 5);
//! 6. `vm.create` then `vm.boot`, each only when the VM needs it;
//! 7. addresses from the neighbour table;
//! 8. once the installed system reports, hot-unplug the installer
//!    (ADR-0065 Decision 4).
//!
//! Every step is idempotent and re-checks the host rather than trusting the
//! previous pass (ADR-0063 Decision 2). Power-off is graceful: power button,
//! then stop the unit once the guest has shut down.

use crate::error::{Error, Result};
use crate::host::HostOps;
use crate::plan::{
    DISK_ID_INSTALL, MachinePlan, VMM_TEMPLATE, swtpm_setup_unit, swtpm_unit, vmm_unit,
};
use crate::systemd::UnitState;
use banlieue_api::common::{
    InitializationStatus, MachineAddress, MachineAddressType, PowerState, condition_types,
};
use banlieue_api::infrastructure::{ChAddressSource, CloudHypervisorMachineStatus};
use banlieue_cloud_hypervisor::{VmState, check_version};
use banlieue_provider_sdk::cloudinit::build_seed_iso;
use banlieue_provider_sdk::status::{condition_status, set_condition};
use std::net::Ipv4Addr;
use tracing::{info, warn};

/// Where a machine is after one converge pass.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    /// The vTPM is being manufactured, or its swtpm is starting.
    StartingTpm,
    /// The VMM unit was just (re)started; its API is not up yet.
    StartingVmm,
    /// The VM is created and booted.
    Running,
    /// The power button was pressed; waiting for the guest to shut down.
    Stopping,
    /// The VMM is not running, as desired.
    #[default]
    Stopped,
}

/// What one converge pass observed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observed {
    /// Where the machine is.
    pub phase: Phase,
    /// Guest IPv4 addresses seen so far.
    pub addresses: Vec<Ipv4Addr>,
    /// The vTPM's EK certificates, as the host minted them (PEM).
    pub ek_certificates: Vec<String>,
    /// The guest reported `phase=installed` over vsock.
    pub guest_installed: bool,
    /// For a plan with an installer: whether it is now detached. `None`
    /// when the plan has none (never had one, or already detached).
    pub install_media_detached: Option<bool>,
}

/// Advance the machine one step toward `desired`.
///
/// `hostname` is the guest's `local-hostname` in its seed; `user_data` the
/// already-resolved payload (ADR-0025, ADR-0038), `None` for no seed.
///
/// # Errors
/// An [`Error`] from the first step that failed. Nothing is retried here;
/// the reconciler's backoff does that.
pub async fn converge(
    host: &dyn HostOps,
    plan: &MachinePlan,
    hostname: &str,
    user_data: Option<&str>,
    desired: &PowerState,
) -> Result<Observed> {
    if *desired == PowerState::PoweredOff {
        return power_off(host, plan).await;
    }

    host.prepare_dirs(plan).await?;
    host.ensure_os_disk(plan).await?;
    host.ensure_install_media(plan).await?;
    if let Some(ud) = user_data {
        let iso = build_seed_iso(hostname, &plan.uid, Some(ud))
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
        host.write_seed(plan, iso).await?;
    }
    host.ensure_taps(plan).await?;
    if plan.tpm.is_some() && !ensure_tpm(host, plan).await? {
        return Ok(observed(Phase::StartingTpm, vec![]));
    }
    // Before the VMM: a report sent on the guest's first boot is not lost.
    host.ensure_report_listener(plan).await?;

    match host.unit_state(&plan.unit).await? {
        Some(state) if state.is_running() => {}
        // Report before restarting: a VMM that cannot even start (bad user,
        // bad sandbox) must show up in status, not loop silently. The
        // reconciler's error backoff paces the retry.
        Some(UnitState::Failed) => {
            let why = host
                .unit_failure(&plan.unit)
                .await?
                .unwrap_or_else(|| "failed, reason unknown".to_string());
            warn!(unit = %plan.unit, %why, "VMM unit failed");
            host.stop_unit(&plan.unit).await?;
            return Err(Error::VmmExited(why));
        }
        other => {
            if other.is_some() {
                info!(unit = %plan.unit, state = ?other, "VMM unit is not running; restarting it");
                host.stop_unit(&plan.unit).await?;
            }
            info!(unit = %plan.unit, "starting VMM unit");
            host.start_unit(&vmm_unit(plan)).await?;
            return Ok(observed(Phase::StartingVmm, vec![]));
        }
    }
    if !host.api_socket_ready(plan).await? {
        return Ok(observed(Phase::StartingVmm, vec![]));
    }

    check_version(&host.vmm_ping(plan).await?)?;
    match host.vm_info(plan).await?.map(|i| i.state) {
        None => {
            info!(unit = %plan.unit, "creating VM");
            host.vm_create(plan).await?;
            host.vm_boot(plan).await?;
        }
        Some(VmState::Created | VmState::Shutdown) => {
            info!(unit = %plan.unit, "booting VM");
            host.vm_boot(plan).await?;
        }
        Some(_) => {}
    }

    let mut o = observed(Phase::Running, host.guest_addresses(plan).await?);
    o.ek_certificates = host.ek_certificates(plan).await?;
    o.guest_installed = host.guest_reported_installed(plan).await;
    if plan.has_install_media() {
        // The live half of the eject (ADR-0065 Decision 4): gone from the
        // running VMM, so a guest-initiated reboot cannot bring it back.
        // The reconciler then plans every later start without it.
        if o.guest_installed {
            info!(unit = %plan.unit, "installed system reported; ejecting the installer");
            host.vm_remove_device(plan, DISK_ID_INSTALL).await?;
        }
        o.install_media_detached = Some(o.guest_installed);
    }
    Ok(o)
}

/// Bring the vTPM up, one step per call; `true` once swtpm is serving.
///
/// Manufacture runs once, in its own unit as the provider's user
/// ([`swtpm_setup_unit`]); whether it ran is the host-minted EK files, not
/// the guest-owned state, so a guest cannot get a second TPM manufactured
/// by deleting its own. A failed unit is reported, as the VMM's is.
async fn ensure_tpm(host: &dyn HostOps, plan: &MachinePlan) -> Result<bool> {
    let Some(t) = &plan.tpm else {
        return Ok(true);
    };
    host.prepare_tpm(plan).await?;
    if !host.tpm_manufactured(plan).await {
        match host.unit_state(&t.setup_unit).await? {
            // Kept failed, not cleared: manufacture fails for host reasons
            // (permissions, the CA), and clearing it would retry every few
            // seconds, each retry overwriting the reported failure.
            Some(UnitState::Failed) => {
                let why = host
                    .unit_failure(&t.setup_unit)
                    .await?
                    .unwrap_or_else(|| "failed, reason unknown".to_string());
                return Err(Error::VmmExited(format!(
                    "{}: {why}; fix the host, then `systemctl reset-failed {}` to retry",
                    t.setup_unit, t.setup_unit
                )));
            }
            Some(_) => {}
            None => {
                let unit = swtpm_setup_unit(plan).ok_or_else(|| {
                    Error::Plan(crate::plan::PlanError::Unsupported(
                        "vTPM: no [tpm] on this host".into(),
                    ))
                })?;
                info!(unit = %t.setup_unit, vmid = %t.vmid, "manufacturing the vTPM");
                host.reset_tpm_state(plan).await?;
                host.start_unit(&unit).await?;
            }
        }
        return Ok(false);
    }
    match host.unit_state(&t.unit).await? {
        Some(state) if state.is_running() => Ok(host.tpm_socket_ready(plan).await),
        Some(UnitState::Failed) => Err(report_failed(host, &t.unit).await?),
        other => {
            if other.is_some() {
                host.stop_unit(&t.unit).await?;
            }
            host.adopt_tpm_state(plan).await?;
            let unit = swtpm_unit(plan).ok_or_else(|| {
                Error::Plan(crate::plan::PlanError::Unsupported(
                    "vTPM: no [tpm] on this host".into(),
                ))
            })?;
            info!(unit = %t.unit, "starting swtpm");
            host.start_unit(&unit).await?;
            Ok(false)
        }
    }
}

/// Read why `unit` failed, clear it so the next pass can retry, and return
/// the error to report.
async fn report_failed(host: &dyn HostOps, unit: &str) -> Result<Error> {
    let why = host
        .unit_failure(unit)
        .await?
        .unwrap_or_else(|| "failed, reason unknown".to_string());
    warn!(%unit, %why, "unit failed");
    host.stop_unit(unit).await?;
    Ok(Error::VmmExited(format!("{unit}: {why}")))
}

async fn power_off(host: &dyn HostOps, plan: &MachinePlan) -> Result<Observed> {
    let running = host
        .unit_state(&plan.unit)
        .await?
        .is_some_and(|s| s.is_running());
    if !running {
        host.stop_unit(&plan.unit).await?;
        stop_tpm(host, plan).await?;
        host.stop_report_listener(plan).await;
        return Ok(observed(Phase::Stopped, vec![]));
    }
    // A socket that cannot be trusted does not stop a power-off: the unit
    // stop below still ends the guest.
    let vm = if host.api_socket_ready(plan).await.unwrap_or(false) {
        host.vm_info(plan).await?.map(|i| i.state)
    } else {
        None
    };
    if vm == Some(VmState::Running) {
        info!(unit = %plan.unit, "pressing the power button");
        host.vm_power_button(plan).await?;
        return Ok(observed(Phase::Stopping, vec![]));
    }
    info!(unit = %plan.unit, "guest is down; stopping VMM unit");
    host.stop_unit(&plan.unit).await?;
    stop_tpm(host, plan).await?;
    host.stop_report_listener(plan).await;
    Ok(observed(Phase::Stopped, vec![]))
}

/// Stop the vTPM's units, after the VMM that uses them.
async fn stop_tpm(host: &dyn HostOps, plan: &MachinePlan) -> Result<()> {
    if let Some(t) = &plan.tpm {
        host.stop_unit(&t.unit).await?;
        host.stop_unit(&t.setup_unit).await?;
    }
    Ok(())
}

/// Remove everything the machine has on the host: unit first, so no file is
/// pulled out from under a running VMM, then taps, then files. Each is
/// verified; absent is success.
///
/// # Errors
/// The first step that failed, including one that reported success but left
/// something behind.
pub async fn teardown(host: &dyn HostOps, plan: &MachinePlan) -> Result<()> {
    host.stop_unit(&plan.unit).await?;
    stop_tpm(host, plan).await?;
    host.stop_report_listener(plan).await;
    if host.unit_state(&plan.unit).await?.is_some() {
        return Err(Error::Systemd(format!(
            "{} is still loaded after stop",
            plan.unit
        )));
    }
    host.remove_taps(plan).await?;
    host.remove_files(plan).await?;
    if host.files_exist(plan).await {
        return Err(Error::Io(std::io::Error::other(format!(
            "{} still has files after removal",
            plan.uid
        ))));
    }
    Ok(())
}

/// The glob that matches every unit this provider owns: VMM units, and
/// image import units (`banlieue-ch-import-*`) while they run.
#[must_use]
pub fn vmm_unit_glob() -> String {
    format!("{VMM_TEMPLATE}@*.service")
}

fn observed(phase: Phase, addresses: Vec<Ipv4Addr>) -> Observed {
    Observed {
        phase,
        addresses,
        ..Observed::default()
    }
}

/// The status for a pass that observed `o`.
#[must_use]
pub fn build_status(
    previous: Option<&CloudHypervisorMachineStatus>,
    o: &Observed,
    plan: &MachinePlan,
    failure_domain: &str,
    generation: i64,
) -> CloudHypervisorMachineStatus {
    let mut st = previous.cloned().unwrap_or_default();
    let running = o.phase == Phase::Running;
    st.initialization = InitializationStatus {
        provisioned: Some(running),
    };
    st.host_uid = Some(plan.host_uid);
    st.failure_domain = Some(failure_domain.to_string());
    st.observed_power_state = Some(match o.phase {
        Phase::Running | Phase::Stopping => PowerState::PoweredOn,
        Phase::StartingTpm | Phase::StartingVmm | Phase::Stopped => PowerState::PoweredOff,
    });
    st.tpm_attached = Some(plan.tpm.is_some());
    if !o.ek_certificates.is_empty() {
        st.tpm_endorsement_certificates = o.ek_certificates.clone();
    }
    if !o.addresses.is_empty() {
        st.addresses = o
            .addresses
            .iter()
            .map(|ip| MachineAddress {
                address_type: MachineAddressType::InternalIP,
                address: ip.to_string(),
            })
            .collect();
        st.address_source = Some(ChAddressSource::Neighbour);
    } else if !running {
        // A guest that is not running has no address. One that is running
        // keeps what a previous pass saw: neighbour entries expire.
        st.addresses.clear();
        st.address_source = None;
    }
    st.observed_generation = Some(generation);

    let (status, reason, message) = match o.phase {
        Phase::Running => (
            condition_status::TRUE,
            "GuestRunning",
            "VM is created and booted",
        ),
        Phase::StartingTpm => (
            condition_status::FALSE,
            "StartingTpm",
            "manufacturing the vTPM or starting its swtpm",
        ),
        Phase::StartingVmm => (
            condition_status::FALSE,
            "StartingVmm",
            "VMM unit started; waiting for its API",
        ),
        Phase::Stopping => (
            condition_status::FALSE,
            "Stopping",
            "power button pressed; waiting for the guest to shut down",
        ),
        Phase::Stopped => (
            condition_status::FALSE,
            "Stopped",
            "VMM is not running, as desired",
        ),
    };
    set_condition(
        &mut st.conditions,
        condition_types::READY,
        status,
        reason,
        message.to_string(),
        generation,
    );

    // ADR-0043/0044/0045 on this backend (ADR-0065 Decisions 4–5). Sticky:
    // the report is re-sent every boot, but a stopped guest has not become
    // uninstalled, and an ejected installer does not come back.
    if o.guest_installed || st.guest_installed == Some(true) {
        st.guest_installed = Some(true);
    } else if running {
        st.guest_installed = Some(false);
    }
    st.install_media_detached = if plan.empty_os_disk {
        match (st.install_media_detached, o.install_media_detached) {
            (Some(true), _) => Some(true),
            (_, Some(d)) => Some(d),
            (previous, None) => previous,
        }
    } else {
        None
    };
    // Published only where it can be evaluated: a Deferred install, which
    // is expected to report, or any guest that has. An Immediate image with
    // no sender leaves it absent, so a pool reports the signal absent
    // rather than waiting forever (ADR-0046 Decision 3).
    let installed = st.guest_installed == Some(true);
    if plan.empty_os_disk || installed {
        let (status, reason, message) = if !installed {
            (
                condition_status::FALSE,
                "GuestNotAnnounced",
                format!(
                    "waiting for the installed system's phase=installed on vsock port {}",
                    crate::report::REPORT_PORT
                ),
            )
        } else if st.install_media_detached == Some(false) {
            (
                condition_status::FALSE,
                "InstallMediaAttached",
                "the installed guest reported, but its installer is still attached (ADR-0044)"
                    .to_string(),
            )
        } else if plan.tpm.is_some() && st.tpm_endorsement_certificates.is_empty() {
            (
                condition_status::FALSE,
                "TpmEndorsementPending",
                "the installed guest reported, but its EK certificate is not published yet \
                 (ADR-0045)"
                    .to_string(),
            )
        } else {
            (
                condition_status::TRUE,
                "GuestAnnounced",
                "the installed guest reported".to_string(),
            )
        };
        set_condition(
            &mut st.conditions,
            condition_types::GUEST_READY,
            status,
            reason,
            message,
            generation,
        );
    }
    st
}

/// The status for a pass that failed with `detail`.
#[must_use]
pub fn failure_status(
    previous: Option<&CloudHypervisorMachineStatus>,
    detail: &str,
    reason: &str,
    generation: i64,
) -> CloudHypervisorMachineStatus {
    let mut st = previous.cloned().unwrap_or_default();
    st.initialization = InitializationStatus {
        provisioned: Some(false),
    };
    st.observed_generation = Some(generation);
    set_condition(
        &mut st.conditions,
        condition_types::READY,
        condition_status::FALSE,
        reason,
        detail.to_string(),
        generation,
    );
    st
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod machine_tests;
