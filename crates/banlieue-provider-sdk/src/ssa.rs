// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Server-side apply helper.
//!
//! Server-side apply is the way controllers should write owned objects: the
//! field-manager string identifies which controller owns which fields, and
//! the apiserver merges concurrent edits from multiple managers safely.
//!
//! Field-manager strings used in banlieue:
//!
//! - `banlieue.io/controller` — the main `banlieue-controller` binary.
//! - `banlieue.io/provider-vsphere` — the vSphere provider binary.
//! - `banlieue.io/provider-proxmox` — the Proxmox provider binary.
//! - `banlieue.io/provider-libvirt` — the libvirt provider binary.

use kube::{
    Resource, ResourceExt,
    api::{Api, Patch, PatchParams},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::error::Result;

/// Field manager for the main `banlieue-controller`.
pub const FIELD_MANAGER_CONTROLLER: &str = "banlieue.io/controller";

/// Field manager for the vSphere provider.
pub const FIELD_MANAGER_PROVIDER_VSPHERE: &str = "banlieue.io/provider-vsphere";

/// Field manager for the Proxmox provider.
pub const FIELD_MANAGER_PROVIDER_PROXMOX: &str = "banlieue.io/provider-proxmox";

/// Field manager for the libvirt provider.
pub const FIELD_MANAGER_PROVIDER_LIBVIRT: &str = "banlieue.io/provider-libvirt";

/// Apply `object` via server-side apply. The owning controller's
/// `field_manager` must be unique — two managers writing the same fields will
/// fight unless one yields ownership.
///
/// `force` is set to `true` to take ownership of any fields previously owned
/// by a different manager. This matches the documented banlieue pattern: each
/// CRD has exactly one controller writing each subresource.
pub async fn server_side_apply<K>(api: &Api<K>, field_manager: &str, object: &K) -> Result<K>
where
    K: Resource<DynamicType = ()>
        + ResourceExt
        + Clone
        + Serialize
        + DeserializeOwned
        + std::fmt::Debug,
{
    let params = PatchParams::apply(field_manager).force();
    let patched = api
        .patch(&object.name_any(), &params, &Patch::Apply(object))
        .await?;
    Ok(patched)
}
