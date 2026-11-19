// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Flag defaults every banlieue binary agrees on.
//!
//! Each of these was declared once per binary with the same value. They are
//! contracts between binaries, not preferences: the imagebuilder creates the
//! artifacts PVC in the build namespace and every provider's import Job
//! mounts it there, and a PVC cannot be mounted across namespaces.

/// Port the health listener (`/livez`, `/readyz`) binds (ADR-0093).
pub const DEFAULT_HEALTH_PORT: u16 = 8081;

/// Port the metrics listener (`/metrics`) binds, the same for every role
/// (ADR-0091 Decision 1).
pub const DEFAULT_METRICS_PORT: u16 = 8080;

/// Namespace the leader-election Lease lives in.
pub const DEFAULT_LEADER_ELECTION_NAMESPACE: &str = "banlieue-system";

/// Namespace holding the artifacts PVC and the import Jobs. Must match
/// `banlieue-imagebuilder`'s `--build-namespace` (ADR-0016, ADR-0020).
pub const DEFAULT_BUILD_NAMESPACE: &str = "banlieue-imagebuild";

/// Default image for `VMImage` import Jobs. Normally overridden by
/// `banlieue-operator`, which passes `--import-image` with the running image
/// so the whole fleet stays on one build. Every workspace crate shares one
/// version, so the tag is the same whichever crate reads it.
pub const DEFAULT_IMPORT_IMAGE: &str =
    concat!("ghcr.io/firestoned/banlieue:v", env!("CARGO_PKG_VERSION"));

/// ServiceAccount an import Job runs as: a dedicated read-only identity,
/// never the provider's own, created in the build namespace by
/// `banlieue bootstrap imagebuilder` (ADR-0016 section 4).
pub const DEFAULT_IMPORT_SERVICE_ACCOUNT: &str = "banlieue-import";

#[cfg(test)]
#[path = "defaults_tests.rs"]
mod defaults_tests;
