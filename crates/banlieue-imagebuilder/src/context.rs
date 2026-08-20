// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context for `banlieue-imagebuilder`.

use banlieue_provider_sdk::scheduling::BuildScheduling;
use kube::Client;

use crate::importer_image::ImporterImage;

/// Context passed into every reconcile call.
#[derive(Clone)]
pub struct Context {
    /// Kubernetes API client.
    pub client: Client,

    /// Namespace `OSArtifact` CRs (and their resulting artifacts PVCs) are
    /// created in. Every `VMImage` build lands here regardless of the
    /// `VMImage`'s own scope (VMImage is cluster-scoped) — this is also the
    /// namespace a provider's per-zone import Jobs must run in to mount the
    /// shared artifacts PVC (ADR-0010).
    pub build_namespace: String,

    /// Where build pods may run. Empty means no constraint (ADR-0016
    /// follow-up).
    pub scheduling: BuildScheduling,

    /// Image (and pull secrets) for `OSArtifact` `spec.importers[]`
    /// containers, e.g. the ISO-overlay materializer. Defaults to the public
    /// `busybox` image; overridable for clusters that pull from an internal
    /// mirror (ADR-0022 Decision #4).
    pub importer_image: ImporterImage,
}

impl Context {
    /// Construct a new [`Context`].
    #[must_use]
    pub fn new(
        client: Client,
        build_namespace: String,
        scheduling: BuildScheduling,
        importer_image: ImporterImage,
    ) -> Self {
        Self {
            client,
            build_namespace,
            scheduling,
            importer_image,
        }
    }
}
