// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Server-side apply helper.
//!
//! Server-side apply is the way controllers should write owned objects: the
//! field-manager string identifies which controller owns which fields, and
//! the apiserver merges concurrent edits from multiple managers safely.
//!
//! ## A manager names a writer, not a class (ADR-0087)
//!
//! A field manager owns exactly the field set it last applied. Two *writers*
//! sharing one manager name therefore delete each other's work: when the
//! second applies, the apiserver sees that manager no longer declaring the
//! first's fields and removes them. On a merge-keyed list such as
//! `VMImage.status.perProvider` that produces a write loop, not an error,
//! because each writer's watch fires on the other's removal.
//!
//! Provider classes can run more than one writer: one deployment per
//! `Provider` (ADR-0003), and one systemd unit per host for the
//! host-resident Cloud Hypervisor provider (ADR-0060). So a provider's
//! manager name must carry its `Provider` identity, which is what
//! [`provider_field_manager`] builds. The class constants below are
//! **ingredients, not manager names**, except for the two single-writer
//! binaries.
//!
//! | Manager | Writers | Scoped per `Provider`? |
//! | --- | --- | --- |
//! | `banlieue.io/controller` | one (`banlieue-controller`) | no |
//! | `banlieue.io/imagebuilder` | one (`banlieue-imagebuilder`) | no |
//! | `banlieue.io/provider-vsphere` | one per `Provider` | **yes** |
//! | `banlieue.io/provider-proxmox` | one per `Provider` | **yes** |
//! | `banlieue.io/provider-libvirt` | one per `Provider` | **yes** |
//! | `banlieue.io/provider-cloud-hypervisor` | one per host | **yes** |

use kube::{
    Resource, ResourceExt,
    api::{Api, DeleteParams, Patch, PatchParams},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::error::Result;

/// Field manager for the main `banlieue-controller`.
pub const FIELD_MANAGER_CONTROLLER: &str = "banlieue.io/controller";

/// Field manager for the vSphere provider.
pub const FIELD_MANAGER_PROVIDER_VSPHERE: &str = "banlieue.io/provider-vsphere";

/// Field manager for the Proxmox provider.
pub const FIELD_MANAGER_PROVIDER_PROXMOX: &str = "banlieue.io/provider-proxmox";

/// Field manager for the Azure provider (ADR-0164).
pub const FIELD_MANAGER_PROVIDER_AZURE: &str = "banlieue.io/provider-azure";

/// Field manager for the EC2 provider (ADR-0174).
pub const FIELD_MANAGER_PROVIDER_EC2: &str = "banlieue.io/provider-ec2";

/// Field manager for the libvirt provider.
pub const FIELD_MANAGER_PROVIDER_LIBVIRT: &str = "banlieue.io/provider-libvirt";

/// Field manager **prefix** for the host-resident Cloud Hypervisor provider
/// (ADR-0060). One writer per host, so it must be scoped through
/// [`provider_field_manager`] before use.
pub const FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR: &str = "banlieue.io/provider-cloud-hypervisor";

/// Field manager for `banlieue-imagebuilder`. Writes exclusively to
/// `VMImage.status.buildArtifact` — never `status.perProvider[]`, which
/// stays owned by each provider's own field manager (ADR-0010).
pub const FIELD_MANAGER_IMAGEBUILDER: &str = "banlieue.io/imagebuilder";

/// Maximum length the apiserver accepts for
/// `metadata.managedFields[].manager`. An over-long manager makes the apply
/// fail outright, so [`provider_field_manager`] truncates to fit.
pub const FIELD_MANAGER_MAX_LEN: usize = 128;

/// Hex characters of identity hash kept when a name must be truncated. Eight
/// is enough that two providers in one namespace will not collide, while
/// leaving the readable prefix intact.
const FIELD_MANAGER_HASH_HEX: usize = 8;

/// Build the field manager for one provider writer: the class prefix plus the
/// `Provider`'s namespace and name (ADR-0087).
///
/// ```text
/// banlieue.io/provider-cloud-hypervisor/banlieue-system/host-a
/// ```
///
/// Two writers of one class get two managers, so neither can remove the
/// other's row from a merge-keyed list. Take the arguments from the
/// `Provider` the binary is serving, never from a literal: a hard-coded value
/// reintroduces exactly the shared-manager bug this exists to prevent.
///
/// When the result would exceed [`FIELD_MANAGER_MAX_LEN`] the identity is
/// replaced by a stable hash of it, keeping the class prefix readable. The
/// hash means truncation cannot collapse two writers back into one manager.
#[must_use]
pub fn provider_field_manager(
    class: &str,
    provider_namespace: &str,
    provider_name: &str,
) -> String {
    let full = format!("{class}/{provider_namespace}/{provider_name}");
    if full.len() <= FIELD_MANAGER_MAX_LEN {
        return full;
    }

    // Deterministic across restarts: a manager that changed per process would
    // orphan every row the previous one owned.
    let digest = stable_hash_hex(&format!("{provider_namespace}/{provider_name}"));
    let suffix = format!("/{digest}");
    let keep = FIELD_MANAGER_MAX_LEN.saturating_sub(suffix.len());
    let mut prefix = class.to_string();
    prefix.truncate(keep);
    format!("{prefix}{suffix}")
}

/// Lowercase hex of a stable 64-bit hash, trimmed to
/// [`FIELD_MANAGER_HASH_HEX`] characters.
///
/// `DefaultHasher` is explicitly not stable across Rust releases, so the
/// mixing is written out here: this value lands in cluster state and must mean
/// the same thing after a toolchain bump. FNV-1a, which is adequate because
/// the input space is a handful of names per namespace and this is not a
/// security boundary.
fn stable_hash_hex(input: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x1000_0000_01b3;

    let mut hash = FNV_OFFSET;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    let hex = format!("{hash:016x}");
    hex[..FIELD_MANAGER_HASH_HEX].to_string()
}

/// Apply `object` via server-side apply.
///
/// `field_manager` must name **one writer**, not a class of writers: see the
/// module docs and ADR-0087. For a provider, build it with
/// [`provider_field_manager`].
///
/// `force` is set to `true`, which takes ownership of fields previously owned
/// by a different manager. That is correct for a whole object this binary
/// solely owns, and it is **wrong for a shared, merge-keyed status list**: it
/// converts an ownership conflict into a silent takeover. Row writes into
/// `VMImage.status.perProvider` therefore apply without force (ADR-0087
/// decision 3), so a conflict surfaces as the defect it is.
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

/// The field manager a provider workload writes its shared rows with
/// (ADR-0087, Amendment 2026-11-01): scoped to its `Provider` when it was
/// started for one (`provider_name`), since it then writes only that
/// `Provider`'s row and a shared manager would remove the rows the other
/// workloads applied; the class constant for a static install, which writes
/// every row.
#[must_use]
pub fn status_field_manager(
    class: &str,
    provider_name: Option<&str>,
    provider_namespace: &str,
) -> String {
    match provider_name {
        Some(name) => provider_field_manager(class, provider_namespace, name),
        None => class.to_string(),
    }
}

/// HTTP status the API server returns for an object that does not exist.
pub const HTTP_NOT_FOUND: u16 = 404;

/// The apply body for a status write: `apiVersion`, `kind` and `status`.
///
/// `status` is either a whole status struct or a partial
/// `serde_json::Value`; with server-side apply the manager owns exactly the
/// fields present. No `metadata` is sent: the object is named by the request
/// path, and the API server rejects nothing for its absence.
///
/// # Errors
/// When `status` does not serialize to JSON.
pub fn status_patch<K, S>(status: &S) -> std::result::Result<Value, serde_json::Error>
where
    K: Resource<DynamicType = ()>,
    S: Serialize + ?Sized,
{
    Ok(json!({
        "apiVersion": K::api_version(&()),
        "kind": K::kind(&()),
        "status": serde_json::to_value(status)?,
    }))
}

/// The apply body that sets `spec.providerID` and nothing else.
#[must_use]
pub fn provider_id_patch<K>(provider_id: &str) -> Value
where
    K: Resource<DynamicType = ()>,
{
    json!({
        "apiVersion": K::api_version(&()),
        "kind": K::kind(&()),
        "spec": { "providerID": provider_id },
    })
}

/// Server-side-apply `status` onto the status subresource of `name`.
///
/// `field_manager` names one writer (module docs, ADR-0087). Pass
/// `force = true` for a status this writer solely owns, and `false` for rows
/// in a shared, merge-keyed list, where a conflict must surface instead of
/// becoming a takeover.
///
/// # Arguments
/// * `api` - the `Api` scoped to the object's namespace (or cluster-wide).
/// * `name` - the object to patch.
/// * `field_manager` - the writer's field manager.
/// * `status` - a whole status struct, or a partial `serde_json::Value`.
/// * `force` - whether to take fields owned by another manager.
///
/// # Errors
/// The API server's refusal, or [`kube::Error::SerdeError`] when `status`
/// does not serialize.
pub async fn apply_status<K, S>(
    api: &Api<K>,
    name: &str,
    field_manager: &str,
    status: &S,
    force: bool,
) -> std::result::Result<(), kube::Error>
where
    K: Resource<DynamicType = ()> + Clone + DeserializeOwned + std::fmt::Debug,
    S: Serialize + ?Sized,
{
    let patch = status_patch::<K, S>(status).map_err(kube::Error::SerdeError)?;
    let mut params = PatchParams::apply(field_manager);
    params.force = force;
    api.patch_status(name, &params, &Patch::Apply(&patch))
        .await?;
    Ok(())
}

/// Server-side-apply `spec.providerID` on `name`, without force.
///
/// CAPI puts `providerID` on spec and the provider sets it, as its own field
/// under the provider's field manager, so it never contends with the
/// controller's ownership of the rest of spec.
///
/// # Errors
/// The API server's refusal.
pub async fn apply_provider_id<K>(
    api: &Api<K>,
    name: &str,
    field_manager: &str,
    provider_id: &str,
) -> std::result::Result<(), kube::Error>
where
    K: Resource<DynamicType = ()> + Clone + DeserializeOwned + std::fmt::Debug,
{
    let patch = provider_id_patch::<K>(provider_id);
    api.patch(
        name,
        &PatchParams::apply(field_manager),
        &Patch::Apply(&patch),
    )
    .await?;
    Ok(())
}

/// Whether `error` is the API server saying the object does not exist.
#[must_use]
pub fn is_not_found(error: &kube::Error) -> bool {
    matches!(error, kube::Error::Api(status) if status.code == HTTP_NOT_FOUND)
}

/// Delete `name` through `api`, treating "already gone" as success so that
/// cleanup is idempotent.
///
/// # Errors
/// Any failure other than a 404.
pub async fn delete_ignoring_404<K>(
    api: &Api<K>,
    name: &str,
    params: &DeleteParams,
) -> std::result::Result<(), kube::Error>
where
    K: Clone + DeserializeOwned + std::fmt::Debug,
{
    match api.delete(name, params).await {
        Ok(_) => Ok(()),
        Err(e) if is_not_found(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
#[path = "ssa_tests.rs"]
mod ssa_tests;
