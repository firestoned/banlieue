// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The client interface reconcilers depend on (ADR-0074 Decision 2).

use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::types::{
    CloneParams, ClusterVm, GuestInterface, NetworkIface, Node, Storage, TaskStatus, Version,
    VmConfig, VmId, VmStatus, Volume,
};
use crate::upid::Upid;
use crate::wire::Params;

/// How often `wait_task` polls a running task.
pub const TASK_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Everything banlieue asks of Proxmox VE.
///
/// Every mutation that Proxmox runs as a task returns its [`Upid`]; a 200
/// means *queued*, so callers pass it to [`wait_task`](Self::wait_task).
/// Reconcilers take `Arc<dyn ProxmoxApi>` so tests can substitute
/// [`FakeProxmox`](crate::fake::FakeProxmox).
#[async_trait]
pub trait ProxmoxApi: Send + Sync {
    /// `GET /version`.
    async fn version(&self) -> Result<Version>;
    /// `GET /nodes`.
    async fn list_nodes(&self) -> Result<Vec<Node>>;
    /// `GET /cluster/resources?type=vm`.
    async fn cluster_vms(&self) -> Result<Vec<ClusterVm>>;
    /// `GET /cluster/nextid`. Racy across controllers: persist the result.
    async fn next_id(&self) -> Result<VmId>;
    /// `GET /nodes/{node}/storage`.
    async fn node_storage(&self, node: &str) -> Result<Vec<Storage>>;
    /// `GET /nodes/{node}/network`.
    async fn node_networks(&self, node: &str) -> Result<Vec<NetworkIface>>;
    /// `GET /nodes/{node}/storage/{storage}/content`.
    async fn storage_content(&self, node: &str, storage: &str) -> Result<Vec<Volume>>;

    /// `POST /nodes/{node}/qemu/{template}/clone`.
    async fn clone_vm(&self, node: &str, template: u32, params: &CloneParams) -> Result<Upid>;
    /// `GET /nodes/{node}/qemu/{vmid}/config`.
    async fn vm_config(&self, node: &str, vmid: u32) -> Result<VmConfig>;
    /// `PUT /nodes/{node}/qemu/{vmid}/config`. Synchronous. `delete` in
    /// `params` names keys to remove.
    async fn set_vm_config(&self, node: &str, vmid: u32, params: &Params) -> Result<()>;
    /// `PUT /nodes/{node}/qemu/{vmid}/resize`. `size` is absolute (`40G`) or
    /// relative (`+10G`); disks only grow.
    ///
    /// On PVE 9 this is a **task** and returns its UPID, which the caller must
    /// await like any other; older releases resized synchronously and return
    /// `None`. Found live: treating it as synchronous let a configure race the
    /// resize it had not waited for.
    async fn resize_disk(
        &self,
        node: &str,
        vmid: u32,
        disk: &str,
        size: &str,
    ) -> Result<Option<Upid>>;
    /// `POST …/status/start`.
    async fn start_vm(&self, node: &str, vmid: u32) -> Result<Upid>;
    /// `POST …/status/stop`: immediate power-off.
    async fn stop_vm(&self, node: &str, vmid: u32) -> Result<Upid>;
    /// `POST …/status/shutdown`: ACPI shutdown.
    async fn shutdown_vm(&self, node: &str, vmid: u32) -> Result<Upid>;
    /// `GET …/status/current`.
    async fn vm_status(&self, node: &str, vmid: u32) -> Result<VmStatus>;
    /// `DELETE /nodes/{node}/qemu/{vmid}`, purging job/replication config and
    /// destroying unreferenced disks.
    async fn delete_vm(&self, node: &str, vmid: u32) -> Result<Upid>;

    /// `POST /nodes/{node}/storage/{storage}/upload` with `content=iso`.
    async fn upload_iso(
        &self,
        node: &str,
        storage: &str,
        filename: &str,
        data: Vec<u8>,
    ) -> Result<Upid>;
    /// `DELETE /nodes/{node}/storage/{storage}/content/{volid}`.
    async fn delete_volume(&self, node: &str, storage: &str, volid: &str) -> Result<Upid>;

    /// `GET /nodes/{node}/tasks/{upid}/status`.
    async fn task_status(&self, upid: &Upid) -> Result<TaskStatus>;
    /// `GET …/agent/network-get-interfaces`.
    async fn agent_interfaces(&self, node: &str, vmid: u32) -> Result<Vec<GuestInterface>>;

    /// Poll `upid` every `interval` until it stops.
    ///
    /// # Errors
    /// [`Error::TaskFailed`] on an exit status other than `OK`;
    /// [`Error::TaskTimeout`] if it is still running after `timeout` — a
    /// clone that never finishes must not look like one that did.
    async fn wait_task_with(
        &self,
        upid: &Upid,
        timeout: Duration,
        interval: Duration,
    ) -> Result<()> {
        let started = Instant::now();
        loop {
            let status = self.task_status(upid).await?;
            if !status.is_running() {
                if status.succeeded() {
                    return Ok(());
                }
                return Err(Error::TaskFailed {
                    upid: upid.to_string(),
                    exitstatus: status
                        .exitstatus
                        .unwrap_or_else(|| "no exit status".to_string()),
                });
            }
            let waited = started.elapsed();
            if waited >= timeout {
                return Err(Error::TaskTimeout {
                    upid: upid.to_string(),
                    waited,
                });
            }
            tokio::time::sleep(interval.min(timeout - waited)).await;
        }
    }

    /// [`wait_task_with`](Self::wait_task_with) at [`TASK_POLL_INTERVAL`].
    async fn wait_task(&self, upid: &Upid, timeout: Duration) -> Result<()> {
        self.wait_task_with(upid, timeout, TASK_POLL_INTERVAL).await
    }
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod api_tests;
