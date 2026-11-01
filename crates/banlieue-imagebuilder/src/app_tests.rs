// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::app`].
//!
//! These cover argument parsing (defaults + overrides) and the pure
//! `build_leader_config` mapping. The async `run` loop is exercised
//! end-to-end against a real API server, not unit-tested here.

#[cfg(test)]
mod tests {
    use super::super::*;
    use clap::Parser;

    /// Top-level wrapper so the `Args`-only [`Cli`] can be parsed standalone in
    /// tests (the real binary embeds it as a subcommand payload).
    #[derive(Parser)]
    struct Wrapper {
        #[command(flatten)]
        cli: Cli,
    }

    fn parse(args: &[&str]) -> Cli {
        let mut argv = vec!["imagebuilder"];
        argv.extend_from_slice(args);
        Wrapper::parse_from(argv).cli
    }

    #[test]
    fn defaults_are_applied() {
        let cli = parse(&[]);
        assert_eq!(cli.build_namespace, DEFAULT_BUILD_NAMESPACE);
        assert_eq!(cli.health_port, DEFAULT_HEALTH_PORT);
        assert_eq!(cli.metrics_port, DEFAULT_METRICS_PORT);
        assert_eq!(cli.log_format, "text");
        assert!(!cli.no_leader_elect);
        assert_eq!(cli.leader_election_id, DEFAULT_LEADER_ELECTION_ID);
        assert_eq!(cli.build_importer_image, ISO_OVERLAY_IMPORTER_IMAGE);
        assert!(cli.build_importer_image_pull_secrets.is_empty());
        assert!(cli.command.is_none());
        assert!(
            registry_config(&cli).unwrap().is_none(),
            "no registry by default"
        );
    }

    #[test]
    fn registry_flags_build_a_registry_config() {
        let cli = parse(&[
            "--registry-repository",
            "registry.internal:5000/banlieue/disks",
            "--registry-credentials-secret",
            "registry-push",
            "--registry-plain-http",
            "--push-image",
            "mirror.internal/banlieue:v0.1.0",
        ]);
        let r = registry_config(&cli).unwrap().unwrap();
        assert_eq!(
            r.repository.to_string(),
            "registry.internal:5000/banlieue/disks"
        );
        assert_eq!(r.credentials_secret.as_deref(), Some("registry-push"));
        assert!(r.plain_http);
        assert_eq!(r.image, "mirror.internal/banlieue:v0.1.0");
    }

    /// A bad repository fails startup, not the first push.
    #[test]
    fn a_malformed_registry_repository_is_a_startup_error() {
        let cli = parse(&["--registry-repository", "disks"]);
        assert!(registry_config(&cli).is_err());
    }

    const REPO: &str = "registry.internal:5000/banlieue/disks";

    #[test]
    fn no_ca_bundle_flags_means_no_ca_bundle() {
        let cli = parse(&["--registry-repository", REPO]);
        assert!(registry_config(&cli).unwrap().unwrap().ca_bundle.is_none());
    }

    #[test]
    fn a_config_map_ca_bundle_defaults_its_key_to_ca_crt() {
        let cli = parse(&[
            "--registry-repository",
            REPO,
            "--registry-ca-bundle-config-map",
            "corp-ca",
        ]);
        let ca = registry_config(&cli).unwrap().unwrap().ca_bundle.unwrap();
        assert_eq!(ca.source, CaBundleSource::ConfigMap("corp-ca".to_string()));
        assert_eq!(ca.key, "ca.crt");
    }

    #[test]
    fn a_secret_ca_bundle_takes_an_explicit_key() {
        let cli = parse(&[
            "--registry-repository",
            REPO,
            "--registry-ca-bundle-secret",
            "corp-ca",
            "--registry-ca-bundle-key",
            "root.pem",
        ]);
        let ca = registry_config(&cli).unwrap().unwrap().ca_bundle.unwrap();
        assert_eq!(ca.source, CaBundleSource::Secret("corp-ca".to_string()));
        assert_eq!(ca.key, "root.pem");
    }

    /// Two sources for one bundle is ambiguous: refused when parsing, not
    /// resolved by picking one.
    #[test]
    fn a_config_map_and_a_secret_ca_bundle_conflict() {
        let argv = [
            "imagebuilder",
            "--registry-ca-bundle-config-map",
            "a",
            "--registry-ca-bundle-secret",
            "b",
        ];
        assert!(Wrapper::try_parse_from(argv).is_err());
    }

    /// A key with no bundle to take it from is a typo, not a no-op.
    #[test]
    fn a_ca_bundle_key_without_a_bundle_is_a_startup_error() {
        let cli = parse(&[
            "--registry-repository",
            REPO,
            "--registry-ca-bundle-key",
            "root.pem",
        ]);
        let err = registry_config(&cli).unwrap_err().to_string();
        assert!(err.contains("registry-ca-bundle"), "{err}");
    }

    #[test]
    fn a_traversing_ca_bundle_key_is_a_startup_error() {
        let cli = parse(&[
            "--registry-repository",
            REPO,
            "--registry-ca-bundle-secret",
            "corp-ca",
            "--registry-ca-bundle-key",
            "../etc/passwd",
        ]);
        assert!(registry_config(&cli).is_err());
    }

    /// The Job's own `--ca-bundle` flag parses on the push subcommand.
    #[test]
    fn push_subcommand_takes_a_ca_bundle_path() {
        let cli = parse(&[
            "push",
            "--source",
            "/artifacts/x.raw",
            "--target",
            "registry.internal:5000/d:t",
            "--artifact-type",
            "application/vnd.banlieue.disk.raw.v1",
            "--scratch-dir",
            "/scratch",
            "--ca-bundle",
            "/var/run/banlieue/registry-ca/ca.crt",
        ]);
        let Some(ImagebuilderCommand::Push(args)) = cli.command else {
            panic!("expected push");
        };
        assert_eq!(
            args.ca_bundle,
            Some(std::path::PathBuf::from(
                "/var/run/banlieue/registry-ca/ca.crt"
            ))
        );
    }

    #[test]
    fn push_subcommand_parses() {
        let cli = parse(&[
            "push",
            "--source",
            "/artifacts/x.raw",
            "--target",
            "registry.internal:5000/d:t",
            "--artifact-type",
            "application/vnd.banlieue.disk.raw.v1",
            "--scratch-dir",
            "/scratch",
        ]);
        let Some(ImagebuilderCommand::Push(args)) = cli.command else {
            panic!("expected push");
        };
        assert_eq!(args.target, "registry.internal:5000/d:t");
        assert_eq!(args.scratch_dir, std::path::PathBuf::from("/scratch"));
    }

    #[test]
    fn build_importer_image_overrides_parse() {
        let cli = parse(&[
            "--build-importer-image",
            "mirror.internal/library/busybox:1.36@sha256:abc123",
            "--build-importer-image-pull-secret",
            "mirror-pull-secret",
            "--build-importer-image-pull-secret",
            "other-secret",
        ]);
        assert_eq!(
            cli.build_importer_image,
            "mirror.internal/library/busybox:1.36@sha256:abc123"
        );
        assert_eq!(
            cli.build_importer_image_pull_secrets,
            vec!["mirror-pull-secret".to_string(), "other-secret".to_string()]
        );
    }

    #[test]
    fn build_namespace_override_parses() {
        let cli = parse(&["--build-namespace", "kairos-build"]);
        assert_eq!(cli.build_namespace, "kairos-build");
    }

    #[test]
    fn build_leader_config_maps_cli_values() {
        let cli = parse(&[
            "--leader-election-namespace",
            "other-ns",
            "--leader-election-id",
            "custom-lock",
            "--leader-election-identity",
            "pod-1",
        ]);
        let cfg = build_leader_config(&cli);
        assert_eq!(cfg.namespace, "other-ns");
        assert_eq!(cfg.lease_name, "custom-lock");
        assert_eq!(cfg.identity, "pod-1");
    }

    /// ADR-0113: pre-laying is off until an operator names an image that can
    /// run QEMU; a VMImage asking for it is then refused, not shipped raw.
    #[test]
    fn prelay_image_is_unset_by_default_and_settable() {
        assert_eq!(parse(&[]).prelay_image, None);
        let cli = parse(&[
            "--prelay-image",
            "registry.internal:5000/banlieue/prelay:v1",
        ]);
        assert_eq!(
            cli.prelay_image.as_deref(),
            Some("registry.internal:5000/banlieue/prelay:v1")
        );
    }
}
