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
}
