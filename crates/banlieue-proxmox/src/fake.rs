// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! An in-memory Proxmox for unit tests.
//!
//! It refuses what the real API refuses (rules/testing.md, rule 1): cloning
//! onto a VMID that exists, deleting a running VM, starting a running one,
//! shrinking a disk, uploading an ISO to a storage without `iso` content. A
//! task that fails leaves the object untouched, as in Proxmox.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;

use crate::api::ProxmoxApi;
use crate::error::{Error, Result};
use crate::types::{
    CloneParams, ClusterVm, GuestInterface, NetworkIface, Node, Storage, TaskStatus, Version,
    VmConfig, VmId, VmStatus, Volume,
};
use crate::upid::Upid;
use crate::wire::Params;

/// First VMID Proxmox hands out.
const FIRST_VMID: u32 = 100;
const HTTP_INTERNAL_SERVER_ERROR: u16 = 500;
const BYTES_PER_KIB: u64 = 1024;
const BYTES_PER_MIB: u64 = BYTES_PER_KIB * 1024;
const BYTES_PER_GIB: u64 = BYTES_PER_MIB * 1024;
const BYTES_PER_TIB: u64 = BYTES_PER_GIB * 1024;
const FAKE_VERSION: &str = "9.0.3";
const AGENT_KEY: &str = "agent";
const DELETE_KEY: &str = "delete";
const DESCRIPTION_KEY: &str = "description";
const DEFAULT_BRIDGE: &str = "vmbr0";

#[derive(Clone)]
struct FakeVm {
    node: String,
    name: String,
    running: bool,
    template: bool,
    config: BTreeMap<String, String>,
}

struct FakeTask {
    polls: u32,
    exitstatus: String,
}

struct State {
    nodes: Vec<String>,
    storages: Vec<Storage>,
    bridges: Vec<String>,
    offline: Vec<String>,
    vms: BTreeMap<u32, FakeVm>,
    volumes: BTreeMap<String, Volume>,
    tasks: BTreeMap<String, FakeTask>,
    guest_interfaces: BTreeMap<u32, Vec<GuestInterface>>,
    task_counter: u32,
    polls_before_done: u32,
    fail_next: Option<String>,
}

/// In-memory [`ProxmoxApi`].
pub struct FakeProxmox {
    state: Mutex<State>,
}

fn refuse(message: impl Into<String>) -> Error {
    Error::Api {
        status: HTTP_INTERNAL_SERVER_ERROR,
        message: message.into(),
    }
}

fn storage(name: &str, kind: &str, content: &str) -> Storage {
    Storage {
        storage: name.to_string(),
        kind: kind.to_string(),
        content: content.to_string(),
        enabled: true,
        active: true,
        shared: false,
        total: None,
        avail: None,
    }
}

/// Parse `20G`, `512M`, `1T` or plain bytes.
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let (digits, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = digits.parse().ok()?;
    let mult = match unit {
        "" => 1,
        "K" => BYTES_PER_KIB,
        "M" => BYTES_PER_MIB,
        "G" => BYTES_PER_GIB,
        "T" => BYTES_PER_TIB,
        _ => return None,
    };
    Some(n * mult)
}

fn config_disk_size(value: &str) -> Option<u64> {
    value
        .split(',')
        .find_map(|kv| kv.strip_prefix("size="))
        .and_then(parse_size)
}

fn format_size(bytes: u64) -> String {
    if bytes.is_multiple_of(BYTES_PER_GIB) {
        return format!("{}G", bytes / BYTES_PER_GIB);
    }
    format!("{}M", bytes / BYTES_PER_MIB)
}

impl FakeProxmox {
    /// A standalone node with `local` (iso, vztmpl), `local-lvm` (images,
    /// rootdir) and a `vmbr0` bridge.
    #[must_use]
    pub fn single_node(node: &str) -> Self {
        Self {
            state: Mutex::new(State {
                nodes: vec![node.to_string()],
                storages: vec![
                    storage("local", "dir", "iso,vztmpl,backup"),
                    storage("local-lvm", "lvmthin", "images,rootdir"),
                ],
                bridges: vec![DEFAULT_BRIDGE.to_string()],
                offline: Vec::new(),
                vms: BTreeMap::new(),
                volumes: BTreeMap::new(),
                tasks: BTreeMap::new(),
                guest_interfaces: BTreeMap::new(),
                task_counter: 0,
                polls_before_done: 0,
                fail_next: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("fake state poisoned")
    }

    /// Insert a stopped VM.
    pub fn add_vm(&self, node: &str, vmid: u32, name: &str) {
        self.insert_vm(node, vmid, name, false);
    }

    /// Rename a VM, as an admin would in the UI. A no-op for an unknown VMID.
    pub fn rename_vm(&self, vmid: u32, name: &str) {
        if let Some(vm) = self.lock().vms.get_mut(&vmid) {
            vm.name = name.to_string();
        }
    }

    /// Insert a template.
    pub fn add_template(&self, node: &str, vmid: u32, name: &str) {
        self.insert_vm(node, vmid, name, true);
    }

    fn insert_vm(&self, node: &str, vmid: u32, name: &str, template: bool) {
        self.lock().vms.insert(
            vmid,
            FakeVm {
                node: node.to_string(),
                name: name.to_string(),
                running: false,
                template,
                config: BTreeMap::new(),
            },
        );
    }

    /// Add a node (it shares the fake's storages and bridges).
    pub fn add_node(&self, node: &str) {
        let mut s = self.lock();
        if !s.nodes.iter().any(|n| n == node) {
            s.nodes.push(node.to_string());
        }
    }

    /// Mark a node offline in \`GET /nodes\`.
    pub fn set_node_offline(&self, node: &str) {
        self.lock().offline.push(node.to_string());
    }

    /// Add a storage, or replace one of the same name.
    pub fn add_storage(&self, name: &str, kind: &str, content: &str) {
        let mut s = self.lock();
        s.storages.retain(|st| st.storage != name);
        s.storages.push(storage(name, kind, content));
    }

    /// Add a Linux bridge.
    pub fn add_bridge(&self, iface: &str) {
        self.lock().bridges.push(iface.to_string());
    }

    /// Make each new task report `running` for `n` polls before it stops.
    pub fn set_task_polls_before_done(&self, n: u32) {
        self.lock().polls_before_done = n;
    }

    /// Make the next task stop with this exit status, without applying its effect.
    pub fn fail_next_task(&self, exitstatus: &str) {
        self.lock().fail_next = Some(exitstatus.to_string());
    }

    /// How many times `upid` has been polled.
    #[must_use]
    pub fn task_polls(&self, upid: &Upid) -> u32 {
        self.lock().tasks.get(upid.as_str()).map_or(0, |t| t.polls)
    }

    /// What the guest agent will report for `vmid`.
    pub fn set_guest_interfaces(&self, vmid: u32, interfaces: Vec<GuestInterface>) {
        self.lock().guest_interfaces.insert(vmid, interfaces);
    }
}

impl State {
    fn require_node(&self, node: &str) -> Result<()> {
        if self.nodes.iter().any(|n| n == node) {
            return Ok(());
        }
        Err(refuse(format!(
            "hostname lookup '{node}' failed - no such node"
        )))
    }

    fn vm(&mut self, node: &str, vmid: u32) -> Result<&mut FakeVm> {
        self.require_node(node)?;
        match self.vms.get_mut(&vmid) {
            Some(vm) if vm.node == node => Ok(vm),
            _ => Err(refuse(format!(
                "Configuration file 'nodes/{node}/qemu-server/{vmid}.conf' does not exist"
            ))),
        }
    }

    /// Create a task; the flag says whether its effect should be applied.
    fn task(&mut self, node: &str, kind: &str, id: u32) -> (Upid, bool) {
        self.task_counter += 1;
        let raw = format!(
            "UPID:{node}:{:08X}:00000000:00000000:{kind}:{id}:banlieue@pve!fake:",
            self.task_counter
        );
        let upid = Upid::parse(&raw).expect("fake builds valid UPIDs");
        let failure = self.fail_next.take();
        let ok = failure.is_none();
        self.tasks.insert(
            raw,
            FakeTask {
                polls: 0,
                exitstatus: failure.unwrap_or_else(|| "OK".to_string()),
            },
        );
        (upid, ok)
    }
}

#[async_trait]
impl ProxmoxApi for FakeProxmox {
    async fn version(&self) -> Result<Version> {
        Ok(Version {
            version: FAKE_VERSION.to_string(),
            release: Some("9.0".to_string()),
            repoid: None,
        })
    }

    async fn list_nodes(&self) -> Result<Vec<Node>> {
        let s = self.lock();
        Ok(s.nodes
            .iter()
            .map(|n| Node {
                node: n.clone(),
                status: if s.offline.contains(n) {
                    "offline"
                } else {
                    "online"
                }
                .to_string(),
                maxcpu: None,
                maxmem: None,
            })
            .collect())
    }

    async fn cluster_vms(&self) -> Result<Vec<ClusterVm>> {
        Ok(self
            .lock()
            .vms
            .iter()
            .map(|(id, vm)| ClusterVm {
                vmid: *id,
                node: vm.node.clone(),
                name: Some(vm.name.clone()),
                status: if vm.running { "running" } else { "stopped" }.to_string(),
                kind: "qemu".to_string(),
                template: vm.template,
            })
            .collect())
    }

    async fn next_id(&self) -> Result<VmId> {
        let s = self.lock();
        let mut id = FIRST_VMID;
        while s.vms.contains_key(&id) {
            id += 1;
        }
        Ok(VmId(id))
    }

    async fn node_storage(&self, node: &str) -> Result<Vec<Storage>> {
        let s = self.lock();
        s.require_node(node)?;
        Ok(s.storages.clone())
    }

    async fn node_networks(&self, node: &str) -> Result<Vec<NetworkIface>> {
        let s = self.lock();
        s.require_node(node)?;
        Ok(s.bridges
            .iter()
            .map(|iface| NetworkIface {
                iface: iface.clone(),
                kind: "bridge".into(),
                active: true,
                vlan_aware: false,
            })
            .collect())
    }

    async fn storage_content(&self, node: &str, storage: &str) -> Result<Vec<Volume>> {
        let s = self.lock();
        s.require_node(node)?;
        let prefix = format!("{storage}:");
        Ok(s.volumes
            .values()
            .filter(|v| v.volid.starts_with(&prefix))
            .cloned()
            .collect())
    }

    async fn clone_vm(&self, node: &str, template: u32, params: &CloneParams) -> Result<Upid> {
        let mut s = self.lock();
        let source = s.vm(node, template)?.clone();
        let p = params.params();
        let newid: u32 = p
            .get("newid")
            .and_then(|v| v.parse().ok())
            .expect("CloneParams always sets newid");
        if s.vms.contains_key(&newid) {
            return Err(refuse(format!(
                "unable to create VM {newid}: config file already exists"
            )));
        }
        if p.get("full") == Some("0") && !source.template {
            return Err(refuse(
                "linked clone feature is not available for a non-template VM",
            ));
        }
        let target = p.get("target").unwrap_or(node).to_string();
        s.require_node(&target)?;
        let (upid, ok) = s.task(node, "qmclone", template);
        if ok {
            // A clone starts from the source's config, but its description is
            // its own: the template's must not leak onto every guest.
            let mut config = source.config;
            config.remove(DESCRIPTION_KEY);
            if let Some(d) = p.get(DESCRIPTION_KEY) {
                config.insert(DESCRIPTION_KEY.to_string(), d.to_string());
            }
            s.vms.insert(
                newid,
                FakeVm {
                    node: target,
                    name: p
                        .get("name")
                        .map_or_else(|| format!("VM {newid}"), str::to_string),
                    running: false,
                    template: false,
                    config,
                },
            );
        }
        Ok(upid)
    }

    async fn vm_config(&self, node: &str, vmid: u32) -> Result<VmConfig> {
        Ok(VmConfig(self.lock().vm(node, vmid)?.config.clone()))
    }

    async fn set_vm_config(&self, node: &str, vmid: u32, params: &Params) -> Result<()> {
        let mut s = self.lock();
        let vm = s.vm(node, vmid)?;
        for (k, v) in params.iter() {
            if k == DELETE_KEY {
                for gone in v.split(',') {
                    vm.config.remove(gone.trim());
                }
                continue;
            }
            vm.config.insert(k.to_string(), v.to_string());
        }
        Ok(())
    }

    async fn resize_disk(
        &self,
        node: &str,
        vmid: u32,
        disk: &str,
        size: &str,
    ) -> Result<Option<Upid>> {
        let mut s = self.lock();
        let vm = s.vm(node, vmid)?;
        let Some(current) = vm.config.get(disk).cloned() else {
            return Err(refuse(format!("disk '{disk}' does not exist")));
        };
        let old = config_disk_size(&current).ok_or_else(|| refuse("disk has no size"))?;
        let new = match size.strip_prefix('+') {
            Some(delta) => old + parse_size(delta).ok_or_else(|| refuse("invalid size"))?,
            None => parse_size(size).ok_or_else(|| refuse("invalid size"))?,
        };
        if new < old {
            return Err(refuse("shrinking disks is not supported"));
        }
        let updated: Vec<String> = current
            .split(',')
            .map(|kv| {
                if kv.starts_with("size=") {
                    format!("size={}", format_size(new))
                } else {
                    kv.to_string()
                }
            })
            .collect();
        // Like PVE 9: validation is synchronous, the resize itself is a task,
        // and a task that fails leaves the disk as it was.
        let (upid, ok) = s.task(node, "resize", vmid);
        if ok {
            s.vm(node, vmid)?
                .config
                .insert(disk.to_string(), updated.join(","));
        }
        Ok(Some(upid))
    }

    async fn start_vm(&self, node: &str, vmid: u32) -> Result<Upid> {
        let mut s = self.lock();
        if s.vm(node, vmid)?.running {
            return Err(refuse(format!("VM {vmid} already running")));
        }
        let (upid, ok) = s.task(node, "qmstart", vmid);
        if ok {
            s.vm(node, vmid)?.running = true;
        }
        Ok(upid)
    }

    async fn stop_vm(&self, node: &str, vmid: u32) -> Result<Upid> {
        self.power_off(node, vmid, "qmstop")
    }

    async fn shutdown_vm(&self, node: &str, vmid: u32) -> Result<Upid> {
        self.power_off(node, vmid, "qmshutdown")
    }

    async fn vm_status(&self, node: &str, vmid: u32) -> Result<VmStatus> {
        let mut s = self.lock();
        let vm = s.vm(node, vmid)?;
        Ok(VmStatus {
            status: if vm.running { "running" } else { "stopped" }.to_string(),
            name: Some(vm.name.clone()),
            qmpstatus: None,
            agent: vm.config.get(AGENT_KEY).is_some_and(|a| a.starts_with('1')),
        })
    }

    async fn delete_vm(&self, node: &str, vmid: u32) -> Result<Upid> {
        let mut s = self.lock();
        if s.vm(node, vmid)?.running {
            return Err(refuse("VM is running - destroy failed"));
        }
        let (upid, ok) = s.task(node, "qmdestroy", vmid);
        if ok {
            s.vms.remove(&vmid);
        }
        Ok(upid)
    }

    async fn upload_iso(
        &self,
        node: &str,
        storage: &str,
        filename: &str,
        data: Vec<u8>,
    ) -> Result<Upid> {
        let mut s = self.lock();
        s.require_node(node)?;
        let usable = s
            .storages
            .iter()
            .any(|st| st.storage == storage && st.usable_for("iso"));
        if !usable {
            return Err(refuse(format!(
                "storage '{storage}' does not support content type 'iso'"
            )));
        }
        if filename.contains('/') || !filename.ends_with(".iso") {
            return Err(refuse("wrong file extension or path in filename"));
        }
        let volid = format!("{storage}:iso/{filename}");
        let (upid, ok) = s.task(node, "imgcopy", 0);
        if ok {
            s.volumes.insert(
                volid.clone(),
                Volume {
                    volid,
                    content: "iso".into(),
                    size: Some(data.len() as u64),
                    format: Some("iso".into()),
                },
            );
        }
        Ok(upid)
    }

    async fn delete_volume(&self, node: &str, storage: &str, volid: &str) -> Result<Upid> {
        let mut s = self.lock();
        s.require_node(node)?;
        if !volid.starts_with(&format!("{storage}:")) || !s.volumes.contains_key(volid) {
            return Err(refuse(format!("volume '{volid}' does not exist")));
        }
        let (upid, ok) = s.task(node, "imgdel", 0);
        if ok {
            s.volumes.remove(volid);
        }
        Ok(upid)
    }

    async fn task_status(&self, upid: &Upid) -> Result<TaskStatus> {
        let mut s = self.lock();
        s.require_node(upid.node())?;
        let before = s.polls_before_done;
        let Some(task) = s.tasks.get_mut(upid.as_str()) else {
            return Err(refuse(format!("no such task '{upid}'")));
        };
        task.polls = task.polls.saturating_add(1);
        if task.polls <= before {
            return Ok(TaskStatus {
                status: "running".into(),
                exitstatus: None,
            });
        }
        Ok(TaskStatus {
            status: "stopped".into(),
            exitstatus: Some(task.exitstatus.clone()),
        })
    }

    async fn agent_interfaces(&self, node: &str, vmid: u32) -> Result<Vec<GuestInterface>> {
        let mut s = self.lock();
        let vm = s.vm(node, vmid)?;
        if !vm.running {
            return Err(refuse(format!("VM {vmid} is not running")));
        }
        if !vm.config.get(AGENT_KEY).is_some_and(|a| a.starts_with('1')) {
            return Err(refuse("No QEMU guest agent configured"));
        }
        Ok(s.guest_interfaces.get(&vmid).cloned().unwrap_or_default())
    }
}

impl FakeProxmox {
    fn power_off(&self, node: &str, vmid: u32, kind: &str) -> Result<Upid> {
        let mut s = self.lock();
        if !s.vm(node, vmid)?.running {
            return Err(refuse(format!("VM {vmid} not running")));
        }
        let (upid, ok) = s.task(node, kind, vmid);
        if ok {
            s.vm(node, vmid)?.running = false;
        }
        Ok(upid)
    }
}

#[cfg(test)]
#[path = "fake_tests.rs"]
mod fake_tests;
