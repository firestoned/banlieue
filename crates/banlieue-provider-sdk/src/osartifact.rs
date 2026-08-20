// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared knowledge of kairos-operator's `OSArtifact` CRD (`build.kairos.io`)
//! needed by a provider's per-zone import Job to own itself by the
//! `OSArtifact` whose PVC it mounts (ADR-0027).
//!
//! Mirrors, but deliberately does not import, `banlieue-imagebuilder`'s own
//! `OSARTIFACT_GROUP`/`OSARTIFACT_VERSION`/`OSARTIFACT_KIND` constants — a
//! provider has no dependency on `banlieue-imagebuilder` and only ever needs
//! the GVK for an `ownerReference`, not the full `ApiResource` imagebuilder
//! builds for its `OSArtifact` `DynamicObject` `Api`.

use serde_json::{Value, json};

/// `OSArtifact`'s `apiVersion`, for an `ownerReference`.
pub const OSARTIFACT_API_VERSION: &str = "build.kairos.io/v1alpha2";
/// `OSArtifact`'s `kind`, for an `ownerReference`.
pub const OSARTIFACT_KIND: &str = "OSArtifact";

/// Build the single-entry `ownerReferences` array binding a per-zone import
/// Job's lifecycle to the `OSArtifact` whose PVC it mounts.
///
/// Deleting a stale `OSArtifact` (a rebuild) then garbage-collects the Job
/// immediately, instead of the Job — and its mount on the old artifacts
/// PVC — outliving the artifact for up to its own `ttlSecondsAfterFinished`
/// (ADR-0027).
///
/// Returns `None` when `os_artifact_uid` is not yet known (banlieue-
/// imagebuilder has not yet observed the live `OSArtifact`) — the caller
/// creates the Job without an owner reference in that case and picks one up
/// on a later reconcile once the field is populated; this is fail-open on
/// missing metadata, not an error.
///
/// `blockOwnerDeletion` is deliberately omitted: setting it requires
/// `update` on the owner's `finalizers` subresource, RBAC neither provider
/// otherwise needs — the same rationale already applied to the
/// `OSArtifact`→`VMImage` owner reference in `banlieue-imagebuilder`.
pub fn owner_references(os_artifact_name: &str, os_artifact_uid: Option<&str>) -> Option<Value> {
    let uid = os_artifact_uid?;
    Some(json!([{
        "apiVersion": OSARTIFACT_API_VERSION,
        "kind": OSARTIFACT_KIND,
        "name": os_artifact_name,
        "uid": uid,
    }]))
}

#[cfg(test)]
#[path = "osartifact_tests.rs"]
mod osartifact_tests;
