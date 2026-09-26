// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! An in-memory [`HostOps`] for unit tests.
//!
//! Strict on purpose: it refuses what the real host refuses, with the same
//! error kinds, because a permissive fake is how a reconciler passes every
//! unit test and fails its second real reconcile (the ADR-0050 redefine
//! lesson). The rules below were each observed on a real host or VMM:
//!
//! - a second start of a loaded unit fails (live systemd test);
//! - the VMM API is unreachable until the unit is running and its socket
//!   exists (connect fails with an I/O error);
//! - `vm.create` twice, and `vm.power-button` on a VM that is not running,
//!   are HTTP 500 (captured from v53.0);
//! - `vm.boot` on a VM that is not created fails;
//! - a tap cannot join a bridge the host does not have;
//! - the OS disk needs the image in the cache.

use crate::error::{Error, Result};
use crate::host::HostOps;
use crate::hostfs::DiskOutcome;
use crate::plan::MachinePlan;
use crate::sys::CloneKind;
use crate::systemd::{UnitStart, UnitState};
use async_trait::async_trait;
use banlieue_cloud_hypervisor::{VmInfo, VmState, VmmPing};
use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::Mutex;

/// HTTP status the VMM uses for a refused operation.
const HTTP_INTERNAL: u16 = 500;

/// Everything the fake host remembers.
#[derive(Debug, Default)]
pub struct State {
    /// Bridges that exist.
    pub bridges: BTreeSet<String>,
    /// Images in the cache, by path.
    pub images: BTreeSet<std::path::PathBuf>,
    /// Machine directories that exist.
    pub dirs: BTreeSet<std::path::PathBuf>,
    /// OS disks that exist.
    pub disks: BTreeSet<std::path::PathBuf>,
    /// Per-machine installer copies that exist, by path.
    pub installers: BTreeSet<std::path::PathBuf>,
    /// Seeds written, by path.
    pub seeds: BTreeMap<std::path::PathBuf, Vec<u8>>,
    /// Taps and the bridge each joined.
    pub taps: BTreeMap<String, String>,
    /// Units and their state.
    pub units: BTreeMap<String, UnitState>,
    /// Why each failed unit failed.
    pub unit_failures: BTreeMap<String, String>,
    /// VMs by unit name, with their state.
    pub vms: BTreeMap<String, VmState>,
    /// Neighbour entries: (mac, bridge) → ip.
    pub neighbours: Vec<(String, String, Ipv4Addr)>,
    /// Environment files written, by path.
    pub env_files: BTreeMap<std::path::PathBuf, String>,
    /// Machines (by UID) with a report listener.
    pub listening: BTreeSet<String>,
    /// Machines (by UID) whose guest reported `phase=installed`.
    pub reported_installed: BTreeSet<String>,
    /// Devices removed with `vm.remove-device`, as `(unit, id)`.
    pub removed_devices: Vec<(String, String)>,
    /// Machines (by UID) whose TPM was manufactured.
    pub manufactured: BTreeSet<String>,
    /// TPM state handed to the guest, by machine UID.
    pub adopted: BTreeSet<String>,
    /// VMM version `vmm.ping` reports.
    pub vmm_version: String,
    /// Every call, in order, for assertions.
    pub calls: Vec<String>,
}

/// The fake host.
#[derive(Debug, Default)]
pub struct FakeHost {
    /// Its state, for tests to seed and inspect.
    pub state: Mutex<State>,
}

impl FakeHost {
    /// A host with `bridges`, `images` in the cache, and a v53.0 VMM.
    #[must_use]
    pub fn new(bridges: &[&str], images: &[&std::path::Path]) -> Self {
        let state = State {
            bridges: bridges.iter().map(|b| (*b).to_string()).collect(),
            images: images.iter().map(|p| p.to_path_buf()).collect(),
            vmm_version: "53.0.0".into(),
            ..State::default()
        };
        Self {
            state: Mutex::new(state),
        }
    }

    /// Make `unit` fail the way systemd reports it: loaded, `failed`, with
    /// `detail` as its result. As on a real host, its VM is gone with it.
    pub fn fail_unit(&self, unit: &str, detail: &str) {
        let mut s = self.lock();
        s.units.insert(unit.to_string(), UnitState::Failed);
        s.unit_failures.insert(unit.to_string(), detail.to_string());
        s.vms.remove(unit);
    }

    /// `swtpm_setup` finished: the TPM is manufactured and, as systemd
    /// may do with a finished oneshot instance, its setup unit is unloaded.
    pub fn finish_tpm_setup(&self, plan: &MachinePlan) {
        let mut s = self.lock();
        s.manufactured.insert(plan.uid.clone());
        if let Some(t) = &plan.tpm {
            s.units.remove(&t.setup_unit);
        }
    }

    /// The guest sends `phase=installed`. As on the real host, it is heard
    /// only while the provider is listening.
    pub fn guest_reports_installed(&self, plan: &MachinePlan) {
        let mut s = self.lock();
        if s.listening.contains(&plan.uid) {
            s.reported_installed.insert(plan.uid.clone());
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn api_error(messages: &[&str]) -> Error {
        Error::Vmm(banlieue_cloud_hypervisor::Error::Api {
            status: HTTP_INTERNAL,
            messages: messages.iter().map(|m| (*m).to_string()).collect(),
        })
    }

    /// The VMM API is only reachable with the unit running.
    fn require_vmm(s: &State, plan: &MachinePlan) -> Result<()> {
        if s.units.get(&plan.unit).is_some_and(UnitState::is_running) {
            return Ok(());
        }
        Err(Error::Vmm(banlieue_cloud_hypervisor::Error::Io(
            std::io::Error::new(std::io::ErrorKind::NotFound, "api.sock: no such file"),
        )))
    }
}

fn io_err(kind: std::io::ErrorKind, msg: String) -> Error {
    Error::Io(std::io::Error::new(kind, msg))
}

#[async_trait]
impl HostOps for FakeHost {
    async fn prepare_dirs(&self, plan: &MachinePlan) -> Result<()> {
        let mut s = self.lock();
        s.calls.push("prepare_dirs".into());
        s.dirs.insert(plan.machine_dir.clone());
        s.dirs.insert(plan.run_dir.clone());
        Ok(())
    }

    async fn ensure_os_disk(&self, plan: &MachinePlan) -> Result<DiskOutcome> {
        let mut s = self.lock();
        s.calls.push("ensure_os_disk".into());
        if !s.dirs.contains(&plan.machine_dir) {
            return Err(io_err(
                std::io::ErrorKind::NotFound,
                "machine dir missing".into(),
            ));
        }
        if s.disks.contains(&plan.os_disk) {
            return Ok(DiskOutcome::Existing);
        }
        if plan.empty_os_disk {
            s.disks.insert(plan.os_disk.clone());
            return Ok(DiskOutcome::CreatedEmpty);
        }
        if !s.images.contains(&plan.image) {
            return Err(io_err(
                std::io::ErrorKind::NotFound,
                format!("{}: image not in cache", plan.image.display()),
            ));
        }
        s.disks.insert(plan.os_disk.clone());
        Ok(DiskOutcome::Created(CloneKind::Reflink))
    }

    async fn ensure_install_media(&self, plan: &MachinePlan) -> Result<()> {
        let mut s = self.lock();
        s.calls.push("ensure_install_media".into());
        if !plan.has_install_media() {
            s.installers.remove(&plan.install_media);
            return Ok(());
        }
        if !s.images.contains(&plan.image) {
            return Err(io_err(
                std::io::ErrorKind::NotFound,
                format!("{}: image not in cache", plan.image.display()),
            ));
        }
        s.installers.insert(plan.install_media.clone());
        Ok(())
    }

    async fn write_seed(&self, plan: &MachinePlan, iso: Vec<u8>) -> Result<()> {
        let mut s = self.lock();
        s.calls.push("write_seed".into());
        if !s.dirs.contains(&plan.machine_dir) {
            return Err(io_err(
                std::io::ErrorKind::NotFound,
                "machine dir missing".into(),
            ));
        }
        s.seeds.insert(plan.seed.clone(), iso);
        Ok(())
    }

    async fn remove_files(&self, plan: &MachinePlan) -> Result<()> {
        let mut s = self.lock();
        s.calls.push("remove_files".into());
        s.dirs.remove(&plan.machine_dir);
        s.dirs.remove(&plan.run_dir);
        s.disks.remove(&plan.os_disk);
        s.seeds.remove(&plan.seed);
        Ok(())
    }

    async fn files_exist(&self, plan: &MachinePlan) -> bool {
        let s = self.lock();
        s.dirs.contains(&plan.machine_dir) || s.dirs.contains(&plan.run_dir)
    }

    async fn prepare_tpm(&self, plan: &MachinePlan) -> Result<()> {
        let mut s = self.lock();
        s.calls.push("prepare_tpm".into());
        if plan.tpm.is_some() && !s.dirs.contains(&plan.machine_dir) {
            return Err(io_err(
                std::io::ErrorKind::NotFound,
                "machine dir missing".into(),
            ));
        }
        Ok(())
    }

    async fn reset_tpm_state(&self, _plan: &MachinePlan) -> Result<()> {
        self.lock().calls.push("reset_tpm_state".into());
        Ok(())
    }

    async fn tpm_manufactured(&self, plan: &MachinePlan) -> bool {
        self.lock().manufactured.contains(&plan.uid)
    }

    async fn adopt_tpm_state(&self, plan: &MachinePlan) -> Result<()> {
        let mut s = self.lock();
        s.calls.push("adopt_tpm_state".into());
        if !s.manufactured.contains(&plan.uid) {
            return Err(io_err(
                std::io::ErrorKind::NotFound,
                "no TPM state to adopt".into(),
            ));
        }
        s.adopted.insert(plan.uid.clone());
        Ok(())
    }

    async fn ek_certificates(&self, plan: &MachinePlan) -> Result<Vec<String>> {
        let s = self.lock();
        Ok(if s.manufactured.contains(&plan.uid) {
            vec![format!("ek-of-{}", plan.uid)]
        } else {
            Vec::new()
        })
    }

    async fn tpm_socket_ready(&self, plan: &MachinePlan) -> bool {
        let s = self.lock();
        plan.tpm
            .as_ref()
            .is_some_and(|t| s.units.get(&t.unit).is_some_and(UnitState::is_running))
    }

    async fn vm_remove_device(&self, plan: &MachinePlan, id: &str) -> Result<()> {
        let mut s = self.lock();
        s.calls.push(format!("vm_remove_device {id}"));
        Self::require_vmm(&s, plan)?;
        if !s.vms.contains_key(&plan.unit) {
            return Err(Self::api_error(&["Error from API", "VM is not created"]));
        }
        s.removed_devices.push((plan.unit.clone(), id.to_string()));
        Ok(())
    }

    async fn ensure_report_listener(&self, plan: &MachinePlan) -> Result<()> {
        self.lock().listening.insert(plan.uid.clone());
        Ok(())
    }

    async fn guest_reported_installed(&self, plan: &MachinePlan) -> bool {
        self.lock().reported_installed.contains(&plan.uid)
    }

    async fn stop_report_listener(&self, plan: &MachinePlan) {
        let mut s = self.lock();
        s.listening.remove(&plan.uid);
        s.reported_installed.remove(&plan.uid);
    }

    async fn ensure_taps(&self, plan: &MachinePlan) -> Result<()> {
        let mut s = self.lock();
        s.calls.push("ensure_taps".into());
        for nic in &plan.nics {
            if !s.bridges.contains(&nic.bridge) {
                return Err(io_err(
                    std::io::ErrorKind::NotFound,
                    format!("bridge {} not found", nic.bridge),
                ));
            }
            s.taps.insert(nic.tap.clone(), nic.bridge.clone());
        }
        Ok(())
    }

    async fn remove_taps(&self, plan: &MachinePlan) -> Result<()> {
        let mut s = self.lock();
        s.calls.push("remove_taps".into());
        for nic in &plan.nics {
            s.taps.remove(&nic.tap);
        }
        Ok(())
    }

    async fn unit_state(&self, name: &str) -> Result<Option<UnitState>> {
        Ok(self.lock().units.get(name).cloned())
    }

    async fn unit_failure(&self, name: &str) -> Result<Option<String>> {
        let s = self.lock();
        if s.units.get(name) != Some(&UnitState::Failed) {
            return Ok(None);
        }
        Ok(s.unit_failures.get(name).cloned())
    }

    async fn start_unit(&self, spec: &UnitStart) -> Result<()> {
        let mut s = self.lock();
        s.calls.push(format!("start_unit {}", spec.name));
        if s.units.contains_key(&spec.name) {
            return Err(Error::Systemd(format!(
                "Unit {} was already loaded or has a fragment file.",
                spec.name
            )));
        }
        if let Some(env) = &spec.environment {
            // As on the host: an unsafe value is refused before any start.
            let text = env
                .render()
                .map_err(|e| io_err(std::io::ErrorKind::InvalidInput, e))?;
            s.env_files.insert(env.path.clone(), text);
        }
        s.units.insert(spec.name.clone(), UnitState::Active);
        Ok(())
    }

    async fn stop_unit(&self, name: &str) -> Result<()> {
        let mut s = self.lock();
        s.calls.push(format!("stop_unit {name}"));
        s.units.remove(name);
        s.unit_failures.remove(name);
        s.vms.remove(name);
        Ok(())
    }

    async fn list_units(&self, pattern: &str) -> Result<Vec<(String, UnitState)>> {
        let prefix = pattern.trim_end_matches('*');
        Ok(self
            .lock()
            .units
            .iter()
            .filter(|(n, _)| n.starts_with(prefix))
            .map(|(n, st)| (n.clone(), st.clone()))
            .collect())
    }

    async fn api_socket_ready(&self, plan: &MachinePlan) -> Result<bool> {
        Ok(self
            .lock()
            .units
            .get(&plan.unit)
            .is_some_and(UnitState::is_running))
    }

    async fn vmm_ping(&self, plan: &MachinePlan) -> Result<VmmPing> {
        let s = self.lock();
        Self::require_vmm(&s, plan)?;
        Ok(VmmPing {
            version: s.vmm_version.clone(),
            build_version: None,
        })
    }

    async fn vm_info(&self, plan: &MachinePlan) -> Result<Option<VmInfo>> {
        let s = self.lock();
        Self::require_vmm(&s, plan)?;
        Ok(s.vms.get(&plan.unit).map(|state| VmInfo {
            config: banlieue_cloud_hypervisor::types::VmConfig::default(),
            state: *state,
        }))
    }

    async fn vm_create(&self, plan: &MachinePlan) -> Result<()> {
        let mut s = self.lock();
        s.calls.push("vm_create".into());
        Self::require_vmm(&s, plan)?;
        // The VMM connects to swtpm at create; with no socket it refuses.
        if let Some(t) = &plan.tpm
            && !s.units.get(&t.unit).is_some_and(UnitState::is_running)
        {
            return Err(Self::api_error(&[
                "Error from API",
                "The VM could not be created",
                "Error creating TPM device",
            ]));
        }
        if s.vms.contains_key(&plan.unit) {
            return Err(Self::api_error(&[
                "Error from API",
                "The VM could not be created",
                "VM is already created",
            ]));
        }
        s.vms.insert(plan.unit.clone(), VmState::Created);
        Ok(())
    }

    async fn vm_boot(&self, plan: &MachinePlan) -> Result<()> {
        let mut s = self.lock();
        s.calls.push("vm_boot".into());
        Self::require_vmm(&s, plan)?;
        match s.vms.get(&plan.unit) {
            None => Err(Self::api_error(&["Error from API", "VM is not created"])),
            Some(VmState::Running) => {
                Err(Self::api_error(&["Error from API", "VM is already booted"]))
            }
            Some(_) => {
                s.vms.insert(plan.unit.clone(), VmState::Running);
                Ok(())
            }
        }
    }

    async fn vm_power_button(&self, plan: &MachinePlan) -> Result<()> {
        let mut s = self.lock();
        s.calls.push("vm_power_button".into());
        Self::require_vmm(&s, plan)?;
        if s.vms.get(&plan.unit) != Some(&VmState::Running) {
            return Err(Self::api_error(&[
                "Error from API",
                "Error triggering power button",
                "VM is not running",
            ]));
        }
        // A cooperative guest shuts down.
        s.vms.insert(plan.unit.clone(), VmState::Shutdown);
        Ok(())
    }

    async fn guest_addresses(&self, plan: &MachinePlan) -> Result<Vec<Ipv4Addr>> {
        let s = self.lock();
        let mut out = Vec::new();
        for nic in &plan.nics {
            for (mac, bridge, ip) in &s.neighbours {
                if *mac == nic.mac && *bridge == nic.bridge && !out.contains(ip) {
                    out.push(*ip);
                }
            }
        }
        Ok(out)
    }
}
