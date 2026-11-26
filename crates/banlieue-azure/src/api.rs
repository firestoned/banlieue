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

use std::collections::BTreeMap;

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::gallery::{
    GalleryImageDefinition, GalleryImageVersion, GalleryImageVersionRequest,
    ImageDefinitionRequest, ManagedDisk, Snapshot, SnapshotRequest, UploadDiskRequest,
};
use crate::operation::{OperationHandle, OperationStatus};
use crate::types::{InstanceView, ResourceSku, TaggedResource, Usage};
use crate::upload::SasUrl;
use crate::vm::{NicRequest, VmRequest};

const CODE_NOT_SUPPORTED: &str = "NotSupported";

/// What a test double that does not model galleries answers.
fn not_supported(what: &str) -> Error {
    Error::InvalidRequest {
        code: CODE_NOT_SUPPORTED.to_string(),
        message: format!("{what} is not implemented by this ArmApi"),
    }
}

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

    /// The tags of `GET .../virtualMachines/{name}`; `None` when the VM
    /// does not exist. A strongly consistent read, unlike the resource-group
    /// listing, so ownership is checked against the VM as it is now.
    async fn vm_tags(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<BTreeMap<String, String>>>;

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

    /// `GET .../galleries/{gallery}/images/{image}/versions/{version}`;
    /// `None` when it does not exist (ADR-0162 Decision 8: the provider
    /// reads the published version by name). Read-only, so the runtime
    /// provider's identity may call it.
    ///
    /// The default answers `NotSupported`, for test doubles that do not
    /// model galleries; [`ArmClient`](crate::ArmClient) and
    /// [`FakeArm`](crate::fake::FakeArm) implement it.
    async fn get_gallery_image_version(
        &self,
        _resource_group: &str,
        _gallery: &str,
        _image: &str,
        _version: &str,
    ) -> Result<Option<GalleryImageVersion>> {
        Err(not_supported("get_gallery_image_version"))
    }

    /// `DELETE .../galleries/{gallery}/images/{image}/versions/{version}`;
    /// `Ok(None)` when it is already gone.
    ///
    /// The default answers `NotSupported`, as
    /// [`ArmApi::get_gallery_image_version`].
    async fn delete_gallery_image_version(
        &self,
        _resource_group: &str,
        _gallery: &str,
        _image: &str,
        _version: &str,
    ) -> Result<Option<OperationHandle>> {
        Err(not_supported("delete_gallery_image_version"))
    }
}

/// The result of `beginGetAccess` on an upload disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiskAccess {
    /// ARM answered with the write SAS at once.
    Granted(SasUrl),
    /// ARM started an operation; poll it with
    /// [`PublishApi::disk_access_result`].
    Pending(OperationHandle),
}

/// What the publish Job asks of ARM beyond [`ArmApi`] (ADR-0162): the image
/// definition, the upload disk and its write access, the snapshot, and the
/// image version. Only the publisher's identity holds these rights
/// (ADR-0159 Decision 3); the runtime provider never calls them.
#[async_trait]
pub trait PublishApi: ArmApi {
    /// `GET .../galleries/{g}/images/{def}`; `None` when absent.
    async fn get_gallery_image_definition(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
    ) -> Result<Option<GalleryImageDefinition>>;

    /// `PUT .../galleries/{g}/images/{def}`. Features are immutable once
    /// created.
    async fn put_gallery_image_definition(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
        request: &ImageDefinitionRequest,
    ) -> Result<Option<OperationHandle>>;

    /// `DELETE .../galleries/{g}/images/{def}`; refused while it has
    /// versions.
    async fn delete_gallery_image_definition(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
    ) -> Result<Option<OperationHandle>>;

    /// `GET .../galleries/{g}/images/{def}/versions`.
    async fn list_gallery_image_versions(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
    ) -> Result<Vec<GalleryImageVersion>>;

    /// `PUT .../galleries/{g}/images/{def}/versions/{ver}`: a long-running
    /// operation (7 to 22 minutes in the spike).
    async fn put_gallery_image_version(
        &self,
        resource_group: &str,
        gallery: &str,
        image: &str,
        version: &str,
        request: &GalleryImageVersionRequest,
    ) -> Result<Option<OperationHandle>>;

    /// `GET .../disks/{name}`; `None` when absent.
    async fn get_disk(&self, resource_group: &str, name: &str) -> Result<Option<ManagedDisk>>;

    /// `PUT .../disks/{name}` with `createOption: Upload`.
    async fn put_upload_disk(
        &self,
        resource_group: &str,
        name: &str,
        request: &UploadDiskRequest,
    ) -> Result<Option<OperationHandle>>;

    /// `POST .../disks/{name}/beginGetAccess` for `Write`, valid for
    /// `duration_secs`.
    async fn grant_disk_write_access(
        &self,
        resource_group: &str,
        name: &str,
        duration_secs: u32,
    ) -> Result<DiskAccess>;

    /// The SAS a pending [`DiskAccess`] produced; `None` while it runs.
    async fn disk_access_result(&self, handle: &OperationHandle) -> Result<Option<SasUrl>>;

    /// `POST .../disks/{name}/endGetAccess`: finalises the upload.
    async fn revoke_disk_access(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<OperationHandle>>;

    /// `GET .../snapshots/{name}`; `None` when absent.
    async fn get_snapshot(&self, resource_group: &str, name: &str) -> Result<Option<Snapshot>>;

    /// `PUT .../snapshots/{name}`.
    async fn put_snapshot(
        &self,
        resource_group: &str,
        name: &str,
        request: &SnapshotRequest,
    ) -> Result<Option<OperationHandle>>;

    /// `DELETE .../snapshots/{name}`; `Ok(None)` when already gone.
    async fn delete_snapshot(
        &self,
        resource_group: &str,
        name: &str,
    ) -> Result<Option<OperationHandle>>;
}
