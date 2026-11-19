// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::defaults`].

#[cfg(test)]
mod tests {
    use super::super::*;

    /// Health and metrics are two listeners in one process: the same port
    /// would make the second bind fail at startup.
    #[test]
    fn health_and_metrics_ports_differ() {
        assert_ne!(DEFAULT_HEALTH_PORT, DEFAULT_METRICS_PORT);
    }

    /// The import image is tagged with the workspace version, so a provider
    /// run without `--import-image` imports with its own build.
    #[test]
    fn the_import_image_is_tagged_with_the_crate_version() {
        assert_eq!(
            DEFAULT_IMPORT_IMAGE,
            format!("ghcr.io/firestoned/banlieue:v{}", env!("CARGO_PKG_VERSION"))
        );
    }

    /// Import Jobs run in the build namespace, never beside the controllers.
    #[test]
    fn the_build_namespace_is_not_the_system_namespace() {
        assert_ne!(DEFAULT_BUILD_NAMESPACE, DEFAULT_LEADER_ELECTION_NAMESPACE);
        assert_eq!(DEFAULT_IMPORT_SERVICE_ACCOUNT, "banlieue-import");
    }
}
