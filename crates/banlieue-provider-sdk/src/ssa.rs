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

#[cfg(test)]
#[path = "ssa_tests.rs"]
mod ssa_tests;
