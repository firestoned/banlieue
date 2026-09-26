// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The seam the machine reconciler is tested through.
//!
//! [`HostOps`] is everything the reconciler does to the host: files, taps,
//! systemd units, and the VMM API. [`RealHost`] implements it over
//! [`hostfs`](crate::hostfs), [`sys`](crate::sys),
//! [`systemd`](crate::systemd) and `banlieue-cloud-hypervisor`.
//! [`FakeHost`](crate::fake::FakeHost) implements it in memory and refuses
//! what the real host refuses, so a unit test cannot pass on behaviour the
//! real system would reject (testing rules, "a fake that is more permissive
//! than the real thing hides bugs").

use crate::error::Result;
use crate::hostfs::{self, DiskOutcome};
use crate::neigh;
use crate::plan::MachinePlan;
use crate::sys;
use crate::systemd::{Bus, Systemd, UnitStart, UnitState};
use async_trait::async_trait;
use banlieue_cloud_hypervisor::{Client, ExpectedSocket, VmInfo, VmmPing};
use std::net::Ipv4Addr;
use std::time::Duration;

/// Timeout for one VMM API call.
const VMM_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Everything the machine reconciler does to the host.
#[async_trait]
pub trait HostOps: Send + Sync {
    /// Create the machine and run directories (idempotent).
    async fn prepare_dirs(&self, plan: &MachinePlan) -> Result<()>;
    /// Create and grow the OS disk unless it exists.
    async fn ensure_os_disk(&self, plan: &MachinePlan) -> Result<DiskOutcome>;
    /// Stage the machine's installer copy while it is attached; delete it
    /// once ejected (ADR-0065 Decisions 3 and 4).
    async fn ensure_install_media(&self, plan: &MachinePlan) -> Result<()>;
    /// Write the NoCloud seed unless an identical one exists.
    async fn write_seed(&self, plan: &MachinePlan, iso: Vec<u8>) -> Result<()>;
    /// Remove the machine's files and verify they are gone.
    async fn remove_files(&self, plan: &MachinePlan) -> Result<()>;
    /// Whether any of the machine's files remain.
    async fn files_exist(&self, plan: &MachinePlan) -> bool;

    /// Get the vTPM's directories ready (ADR-0065); a no-op without one.
    async fn prepare_tpm(&self, plan: &MachinePlan) -> Result<()>;
    /// A fresh state directory for manufacture (only just before it).
    async fn reset_tpm_state(&self, plan: &MachinePlan) -> Result<()>;
    /// Whether the machine's TPM was manufactured.
    async fn tpm_manufactured(&self, plan: &MachinePlan) -> bool;
    /// Hand the manufactured TPM state to the guest's uid.
    async fn adopt_tpm_state(&self, plan: &MachinePlan) -> Result<()>;
    /// The EK certificates the host minted, as PEM.
    async fn ek_certificates(&self, plan: &MachinePlan) -> Result<Vec<String>>;
    /// Whether swtpm's control socket is up.
    async fn tpm_socket_ready(&self, plan: &MachinePlan) -> bool;

    /// Create every NIC's tap, owned by the guest, joined to its bridge, up.
    async fn ensure_taps(&self, plan: &MachinePlan) -> Result<()>;
    /// Delete every NIC's tap and verify they are gone.
    async fn remove_taps(&self, plan: &MachinePlan) -> Result<()>;

    /// The unit's state, or `None` when it is not loaded.
    async fn unit_state(&self, name: &str) -> Result<Option<UnitState>>;
    /// Why a unit failed (systemd's result and exit status), when it is
    /// loaded and failed; `None` otherwise.
    async fn unit_failure(&self, name: &str) -> Result<Option<String>>;
    /// Start an instance of a template unit.
    /// Write the unit's environment file, if any, then start it.
    async fn start_unit(&self, spec: &UnitStart) -> Result<()>;
    /// Stop a unit and clear its failed state. Not loaded is success.
    async fn stop_unit(&self, name: &str) -> Result<()>;
    /// Loaded units matching a glob, with state.
    async fn list_units(&self, pattern: &str) -> Result<Vec<(String, UnitState)>>;

    /// Whether the VMM's API socket exists yet, opening it to the
    /// provider's group once it does ([`hostfs::grant_api_socket`]). An
    /// error means something other than the guest's socket is there.
    async fn api_socket_ready(&self, plan: &MachinePlan) -> Result<bool>;
    /// `vmm.ping`.
    async fn vmm_ping(&self, plan: &MachinePlan) -> Result<VmmPing>;
    /// `vm.info`; `None` when no VM is created.
    async fn vm_info(&self, plan: &MachinePlan) -> Result<Option<VmInfo>>;
    /// `vm.create` from the plan.
    async fn vm_create(&self, plan: &MachinePlan) -> Result<()>;
    /// `vm.boot`.
    async fn vm_boot(&self, plan: &MachinePlan) -> Result<()>;
    /// `vm.power-button`.
    async fn vm_power_button(&self, plan: &MachinePlan) -> Result<()>;
    /// `vm.remove-device` (ADR-0065 Decision 4).
    async fn vm_remove_device(&self, plan: &MachinePlan, id: &str) -> Result<()>;

    /// Listen for the guest's report on its vsock (ADR-0065 Decision 5).
    async fn ensure_report_listener(&self, plan: &MachinePlan) -> Result<()>;
    /// Whether the guest reported `phase=installed`.
    async fn guest_reported_installed(&self, plan: &MachinePlan) -> bool;
    /// Stop listening for the guest.
    async fn stop_report_listener(&self, plan: &MachinePlan);

    /// Guest IPv4 addresses per NIC, from the neighbour table.
    async fn guest_addresses(&self, plan: &MachinePlan) -> Result<Vec<Ipv4Addr>>;
}

/// The real host.
pub struct RealHost {
    systemd: Systemd,
    group: u32,
    arp_table: std::path::PathBuf,
    reports: crate::report::Listeners,
}

impl RealHost {
    /// Connect to systemd on `bus`. `group` is the provider's gid, shared
    /// with every guest's files and sockets (ADR-0063 Decision 5).
    ///
    /// # Errors
    /// The D-Bus error from connecting.
    pub async fn connect(bus: Bus, group: u32) -> Result<Self> {
        Ok(Self {
            systemd: Systemd::connect(bus).await?,
            group,
            arp_table: neigh::PROC_NET_ARP.into(),
            reports: crate::report::Listeners::default(),
        })
    }

    fn client(&self, plan: &MachinePlan) -> Client {
        Client::new(&plan.api_socket, VMM_CALL_TIMEOUT).with_expected_owner(ExpectedSocket {
            uid: plan.host_uid,
            gid: self.group,
        })
    }
}

/// Run blocking file or syscall work off the async executor.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T> {
    Ok(tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))??)
}

#[async_trait]
impl HostOps for RealHost {
    async fn prepare_dirs(&self, plan: &MachinePlan) -> Result<()> {
        let (plan, group) = (plan.clone(), self.group);
        blocking(move || hostfs::prepare_dirs(&plan, group)).await
    }

    async fn ensure_os_disk(&self, plan: &MachinePlan) -> Result<DiskOutcome> {
        let (plan, group) = (plan.clone(), self.group);
        blocking(move || hostfs::ensure_os_disk(&plan, group)).await
    }

    async fn ensure_install_media(&self, plan: &MachinePlan) -> Result<()> {
        let (plan, group) = (plan.clone(), self.group);
        blocking(move || hostfs::ensure_install_media(&plan, group)).await
    }

    async fn write_seed(&self, plan: &MachinePlan, iso: Vec<u8>) -> Result<()> {
        let (plan, group) = (plan.clone(), self.group);
        blocking(move || hostfs::write_seed(&plan, &iso, group)).await
    }

    async fn remove_files(&self, plan: &MachinePlan) -> Result<()> {
        let plan = plan.clone();
        blocking(move || hostfs::remove_machine(&plan)).await
    }

    async fn files_exist(&self, plan: &MachinePlan) -> bool {
        hostfs::machine_files_exist(plan)
    }

    async fn prepare_tpm(&self, plan: &MachinePlan) -> Result<()> {
        let (plan, group) = (plan.clone(), self.group);
        blocking(move || hostfs::prepare_tpm(&plan, crate::sys::effective_uid(), group)).await
    }

    async fn reset_tpm_state(&self, plan: &MachinePlan) -> Result<()> {
        let (plan, group) = (plan.clone(), self.group);
        blocking(move || hostfs::reset_tpm_state(&plan, crate::sys::effective_uid(), group)).await
    }

    async fn tpm_manufactured(&self, plan: &MachinePlan) -> bool {
        hostfs::tpm_manufactured(plan)
    }

    async fn adopt_tpm_state(&self, plan: &MachinePlan) -> Result<()> {
        let (plan, group) = (plan.clone(), self.group);
        blocking(move || hostfs::adopt_tpm_state(&plan, group)).await
    }

    async fn ek_certificates(&self, plan: &MachinePlan) -> Result<Vec<String>> {
        let plan = plan.clone();
        blocking(move || hostfs::ek_certificates(&plan)).await
    }

    async fn tpm_socket_ready(&self, plan: &MachinePlan) -> bool {
        hostfs::tpm_socket_ready(plan)
    }

    async fn ensure_taps(&self, plan: &MachinePlan) -> Result<()> {
        let plan = plan.clone();
        blocking(move || {
            for nic in &plan.nics {
                sys::ensure_tap(&nic.tap, Some(plan.host_uid), &nic.bridge)?;
            }
            Ok(())
        })
        .await
    }

    async fn remove_taps(&self, plan: &MachinePlan) -> Result<()> {
        let plan = plan.clone();
        blocking(move || {
            for nic in &plan.nics {
                sys::delete_persistent_tap(&nic.tap)?;
                if sys::interface_exists(&nic.tap) {
                    return Err(std::io::Error::other(format!(
                        "tap {} still exists after delete",
                        nic.tap
                    )));
                }
            }
            Ok(())
        })
        .await
    }

    async fn unit_state(&self, name: &str) -> Result<Option<UnitState>> {
        Ok(self.systemd.state(name).await?)
    }

    async fn unit_failure(&self, name: &str) -> Result<Option<String>> {
        Ok(self.systemd.failure(name).await?)
    }

    async fn start_unit(&self, spec: &UnitStart) -> Result<()> {
        if let Some(env) = &spec.environment {
            let text = env.render().map_err(crate::error::Error::Systemd)?;
            let path = env.path.clone();
            blocking(move || hostfs::write_env_file(&path, &text)).await?;
        }
        Ok(self.systemd.start(spec).await?)
    }

    async fn stop_unit(&self, name: &str) -> Result<()> {
        Ok(self.systemd.stop(name).await?)
    }

    async fn list_units(&self, pattern: &str) -> Result<Vec<(String, UnitState)>> {
        Ok(self.systemd.list(pattern).await?)
    }

    async fn api_socket_ready(&self, plan: &MachinePlan) -> Result<bool> {
        let (path, uid, group) = (plan.api_socket.clone(), plan.host_uid, self.group);
        blocking(move || hostfs::grant_api_socket(&path, uid, group)).await
    }

    async fn vmm_ping(&self, plan: &MachinePlan) -> Result<VmmPing> {
        Ok(self.client(plan).ping().await?)
    }

    async fn vm_info(&self, plan: &MachinePlan) -> Result<Option<VmInfo>> {
        Ok(self.client(plan).info().await?)
    }

    async fn vm_create(&self, plan: &MachinePlan) -> Result<()> {
        Ok(self.client(plan).create(&plan.guest).await?)
    }

    async fn vm_boot(&self, plan: &MachinePlan) -> Result<()> {
        Ok(self.client(plan).boot().await?)
    }

    async fn vm_power_button(&self, plan: &MachinePlan) -> Result<()> {
        Ok(self.client(plan).power_button().await?)
    }

    async fn vm_remove_device(&self, plan: &MachinePlan, id: &str) -> Result<()> {
        Ok(self.client(plan).remove_device(id).await?)
    }

    async fn ensure_report_listener(&self, plan: &MachinePlan) -> Result<()> {
        Ok(self
            .reports
            .ensure(&plan.uid, &plan.report_socket, plan.host_uid, self.group)
            .await?)
    }

    async fn guest_reported_installed(&self, plan: &MachinePlan) -> bool {
        self.reports.installed(&plan.uid)
    }

    async fn stop_report_listener(&self, plan: &MachinePlan) {
        self.reports.stop(&plan.uid, &plan.report_socket);
    }

    async fn guest_addresses(&self, plan: &MachinePlan) -> Result<Vec<Ipv4Addr>> {
        let table = neigh::read_arp(&self.arp_table)?;
        let mut out = Vec::new();
        for nic in &plan.nics {
            for ip in neigh::addresses_for(&table, &nic.mac, &nic.bridge) {
                if !out.contains(&ip) {
                    out.push(ip);
                }
            }
        }
        Ok(out)
    }
}
