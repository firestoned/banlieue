// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared reconcile context for `banlieue-imagebuilder`.

use banlieue_provider_sdk::scheduling::BuildScheduling;
use kube::Client;

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
}

impl Context {
    /// Construct a new [`Context`].
    #[must_use]
    pub fn new(client: Client, build_namespace: String, scheduling: BuildScheduling) -> Self {
        Self {
            client,
            build_namespace,
            scheduling,
        }
    }
}
