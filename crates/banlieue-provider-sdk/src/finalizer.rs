// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Patch-based finalizer add and remove helpers.
//!
//! Controllers use these to ensure cleanup runs before Kubernetes garbage
//! collects an object. The pattern is:
//!
//! 1. On reconcile, if no deletion timestamp is set, call
//!    [`ensure_finalizer`].
//! 2. If a deletion timestamp is set, perform cleanup, then call
//!    [`remove_finalizer`].
//!
//! Both helpers are idempotent and use JSON Merge Patch semantics so they do
//! not conflict with server-side apply on the same object.

use kube::{
    Resource, ResourceExt,
    api::{Api, Patch, PatchParams},
};
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::error::Result;

/// Returns the finalizer list that *would* be sent in a patch to add
/// `finalizer`, or `None` if the finalizer is already present (no patch
/// needed).
///
/// Extracted as a pure function so the add path can be unit-tested without a
/// kube client.
pub fn finalizer_list_with(current: &[String], finalizer: &str) -> Option<Vec<String>> {
    if current.iter().any(|f| f == finalizer) {
        return None;
    }
    let mut next = current.to_vec();
    next.push(finalizer.to_string());
    Some(next)
}

/// Returns the finalizer list that *would* be sent in a patch to remove
/// `finalizer`, or `None` if the finalizer is not present (no patch needed).
pub fn finalizer_list_without(current: &[String], finalizer: &str) -> Option<Vec<String>> {
    let next: Vec<String> = current
        .iter()
        .filter(|f| f.as_str() != finalizer)
        .cloned()
        .collect();
    if next.len() == current.len() {
        return None;
    }
    Some(next)
}

/// Adds `finalizer` to `object.metadata.finalizers` if it is not already
/// present. No-op when the finalizer is already on the object.
///
/// Uses JSON Merge Patch — the apiserver appends the supplied finalizers
/// without disturbing other metadata managed by other controllers.
pub async fn ensure_finalizer<K>(api: &Api<K>, object: &K, finalizer: &str) -> Result<()>
where
    K: Resource<DynamicType = ()> + ResourceExt + Clone + DeserializeOwned + std::fmt::Debug,
{
    let Some(next) = finalizer_list_with(object.finalizers(), finalizer) else {
        return Ok(());
    };
    let patch = json!({ "metadata": { "finalizers": next } });
    api.patch(
        &object.name_any(),
        &PatchParams::default(),
        &Patch::Merge(&patch),
    )
    .await?;
    Ok(())
}

/// Removes `finalizer` from `object.metadata.finalizers`. No-op when the
/// finalizer is not present.
pub async fn remove_finalizer<K>(api: &Api<K>, object: &K, finalizer: &str) -> Result<()>
where
    K: Resource<DynamicType = ()> + ResourceExt + Clone + DeserializeOwned + std::fmt::Debug,
{
    let Some(next) = finalizer_list_without(object.finalizers(), finalizer) else {
        return Ok(());
    };
    let patch = json!({ "metadata": { "finalizers": next } });
    api.patch(
        &object.name_any(),
        &PatchParams::default(),
        &Patch::Merge(&patch),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "finalizer_tests.rs"]
mod finalizer_tests;
