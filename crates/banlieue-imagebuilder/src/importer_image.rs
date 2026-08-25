// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Configurable image for `banlieue-imagebuilder`'s `OSArtifact`
//! `spec.importers[]` containers (today, only the ISO-overlay materializer,
//! ADR-0022 Decision #3), so a cluster whose nodes cannot reach the public
//! default can point it at an internal mirror instead.

use crate::reconciler::vmimage::ISO_OVERLAY_IMPORTER_IMAGE;

/// Image reference and pull secrets for `spec.importers[]` init containers.
///
/// `pull_secrets` also becomes the `OSArtifact`'s pod-wide
/// `spec.imagePullSecrets`: Kubernetes pull secrets are always pod-scoped
/// (there is no per-container equivalent), so the same list covers the main
/// build container's image too when it is pulled from the same mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImporterImage {
    /// Full image reference (`repo[:tag][@sha256:digest]`).
    pub reference: String,
    /// Names of Secrets used to pull it, for a private or mirrored registry.
    pub pull_secrets: Vec<String>,
}

impl Default for ImporterImage {
    fn default() -> Self {
        Self {
            reference: ISO_OVERLAY_IMPORTER_IMAGE.to_string(),
            pull_secrets: Vec::new(),
        }
    }
}

impl ImporterImage {
    /// Build from CLI flag values.
    #[must_use]
    pub fn from_flags(reference: &str, pull_secrets: &[String]) -> Self {
        Self {
            reference: reference.to_string(),
            pull_secrets: pull_secrets.to_vec(),
        }
    }
}

#[cfg(test)]
#[path = "importer_image_tests.rs"]
mod importer_image_tests;
