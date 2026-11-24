// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The client interface reconcilers depend on (ADR-0164 Decision 2).
//!
//! Reconcilers take `Arc<dyn ArmApi>` so unit tests substitute
//! [`FakeArm`](crate::fake::FakeArm), which refuses what ARM refuses. Every
//! call that ARM may run asynchronously returns `Option<OperationHandle>`:
//! `Some` to store and observe on a later reconcile, `None` when ARM finished
//! synchronously. No method blocks on an operation.
//!
//! Deleting something that does not exist is success (`Ok(None)`), as it is
//! in ARM (a 204).

use async_trait::async_trait;

use crate::error::Result;
use crate::operation::{OperationHandle, OperationStatus};
use crate::types::{InstanceView, ResourceSku, TaggedResource, Usage};
use crate::vm::{NicRequest, VmRequest};

/// A change of power state (ADR-0164 Decision 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    /// `POST .../start`.
    Start,
    /// `POST .../deallocate`: stops compute billing. What
    /// `desiredPowerState: PoweredOff` maps to.
    Deallocate,
    /// `POST .../powerOff`: stops the guest but keeps the allocation.
    PowerOff,
    /// `POST .../restart`.
    Restart,
}

/// Everything banlieue asks of Azure Resource Manager for a VM.
#[async_trait]
pub trait ArmApi: Send + Sync {
    /// `PUT .../networkInterfaces/{name}`; idempotent.
    async fn put_nic(
        &self,
        resource_group: &str,
        name: &str,
        request: &NicRequest,
    ) -> Result<Option<OperationHandle>>;

    /// The NIC's primary private address; `None` until assigned.
    async fn nic_private_ip(&self, resource_group: &str, name: &str) -> Result<Option<String>>;

    /// `DELETE .../networkInterfaces/{name}`. Refused while a VM uses it.
    async fn delete_nic(&self, resource_group: &str, name: &str)
    -> Result<Option<OperationHandle>>;

    /// `PUT .../virtualMachines/{name}`; idempotent.
    async fn put_vm(
        &self,
        resource_group: &str,
        name: &str,
        request: &VmRequest,
    ) -> Result<Option<OperationHandle>>;

    /// `GET .../virtualMachines/{name}/instanceView`.
    async fn vm_instance_view(&self, resource_group: &str, name: &str) -> Result<InstanceView>;

    /// `DELETE .../virtualMachines/{name}`; takes its NIC and OS disk
    /// (`deleteOption: Delete`).
    async fn delete_vm(&self, resource_group: &str, name: &str) -> Result<Option<OperationHandle>>;

    /// `POST .../virtualMachines/{name}/{action}`.
    async fn power(
        &self,
        resource_group: &str,
        name: &str,
        action: PowerAction,
    ) -> Result<Option<OperationHandle>>;

    /// `DELETE .../disks/{name}` (orphan sweep).
    async fn delete_disk(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<OperationHandle>>;

    /// Polls a stored operation.
    async fn operation_status(&self, handle: &OperationHandle) -> Result<OperationStatus>;

    /// `GET /subscriptions/{s}/providers/Microsoft.Compute/skus` for
    /// `location`, VM sizes only.
    async fn resource_skus(&self, location: &str) -> Result<Vec<ResourceSku>>;

    /// `GET .../locations/{location}/usages`.
    async fn usages(&self, location: &str) -> Result<Vec<Usage>>;

    /// `GET .../resourceGroups/{rg}/resources?$expand=createdTime`.
    async fn list_resources(&self, resource_group: &str) -> Result<Vec<TaggedResource>>;
}
