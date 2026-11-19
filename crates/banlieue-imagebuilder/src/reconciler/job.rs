// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! What the push Job ([`super::push`]) and the pre-lay Job
//! ([`super::prelay`]) share: both work on one finished build's file on its
//! artifacts PVC, carry the same labels, and are owned by the `OSArtifact`.

use std::collections::BTreeMap;

use banlieue_api::banlieue::BuildArtifactStatus;
use serde_json::Value;

/// The parts of a Job derived from the build it serves.
#[derive(Debug)]
pub(super) struct JobParts {
    /// The artifacts PVC the Job mounts.
    pub pvc: String,
    /// The built file on that PVC.
    pub file: String,
    /// Labels of the Job and its pod.
    pub labels: BTreeMap<String, String>,
    /// The owning `OSArtifact`, so a rebuild garbage-collects the Job
    /// (ADR-0027).
    pub owner_references: Option<Value>,
}

/// Derive a Job's [`JobParts`].
///
/// # Arguments
/// * `component` - value of the `app.kubernetes.io/component` label.
/// * `vmimage` - the `VMImage` name, for the `banlieue.io/vmimage` label.
/// * `artifact` - the `Ready` build artifact.
/// * `os_artifact_uid` - the live `OSArtifact`'s uid: the Job's owner.
pub(super) fn job_parts(
    component: &str,
    vmimage: &str,
    artifact: &BuildArtifactStatus,
    os_artifact_uid: &str,
) -> JobParts {
    JobParts {
        pvc: artifact
            .pvc_ref
            .as_ref()
            .map(|r| r.name.clone())
            .unwrap_or_default(),
        file: artifact.file.clone().unwrap_or_default(),
        labels: BTreeMap::from([
            ("app.kubernetes.io/name".to_string(), "banlieue".to_string()),
            (
                "app.kubernetes.io/component".to_string(),
                component.to_string(),
            ),
            ("banlieue.io/vmimage".to_string(), vmimage.to_string()),
        ]),
        owner_references: banlieue_provider_sdk::osartifact::owner_references(
            &artifact.os_artifact_ref,
            Some(os_artifact_uid),
        ),
    }
}

#[cfg(test)]
#[path = "job_tests.rs"]
mod job_tests;
