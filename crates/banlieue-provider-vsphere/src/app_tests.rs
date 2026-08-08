// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::app`].
//!
//! These cover argument parsing (defaults + overrides) and the pure
//! `build_leader_config` mapping. The async `run` loop is exercised
//! end-to-end against vcsim / a real API server, not unit-tested here.

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
        let mut argv = vec!["vsphere"];
        argv.extend_from_slice(args);
        Wrapper::parse_from(argv).cli
    }

    #[test]
    fn defaults_are_applied() {
        let cli = parse(&[]);
        assert_eq!(cli.health_port, DEFAULT_HEALTH_PORT);
        assert_eq!(cli.metrics_port, DEFAULT_METRICS_PORT);
        assert_eq!(cli.log_format, "text");
        assert!(!cli.no_leader_elect);
        assert_eq!(cli.leader_election_id, DEFAULT_LEADER_ELECTION_ID);
        assert_eq!(
            cli.vsphere_task_timeout_secs,
            DEFAULT_VSPHERE_TASK_TIMEOUT_SECS
        );
        assert_eq!(cli.import_image, DEFAULT_IMPORT_IMAGE);
    }

    /// `banlieue-operator` passes `--import-image <ref>` to every spawned
    /// provider (workload.rs) so the whole fleet runs one image. The vSphere
    /// provider must accept it even though its import path is a later
    /// iteration — otherwise the operator-spawned Deployment crashes at
    /// arg-parse with "unexpected argument '--import-image'".
    #[test]
    fn import_image_override_parses() {
        let cli = parse(&["--import-image", "registry.example/banlieue:local-dev"]);
        assert_eq!(cli.import_image, "registry.example/banlieue:local-dev");
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

    #[test]
    fn task_timeout_override_parses() {
        let cli = parse(&["--vsphere-task-timeout-secs", "120"]);
        assert_eq!(cli.vsphere_task_timeout_secs, 120);
    }

    // ----------------------------------------------------------------------
    // --provider-name (per-instance topology, ADR-0003)
    // ----------------------------------------------------------------------

    /// `banlieue-operator` passes `--provider-name` on every spawned workload.
    /// If this flag is not accepted, clap exits non-zero and the Deployment
    /// crash-loops.
    #[test]
    fn provider_name_flag_is_accepted() {
        let cli = parse(&["--provider-name", "prod-vc"]);
        assert_eq!(cli.provider_name.as_deref(), Some("prod-vc"));
    }

    /// Unset means "watch every Provider of this class" — the pre-ADR-0003
    /// behaviour, still used by a statically installed provider.
    #[test]
    fn provider_name_defaults_to_unset() {
        assert_eq!(parse(&[]).provider_name, None);
    }

    /// A per-instance workload must narrow its watch server-side rather than
    /// filtering in the reconciler, or every pod caches every backend's objects.
    #[test]
    fn provider_name_produces_a_field_selector_for_that_object() {
        let cfg = provider_watch_config(Some("prod-vc"));
        assert_eq!(cfg.field_selector.as_deref(), Some("metadata.name=prod-vc"));
    }

    #[test]
    fn no_provider_name_leaves_the_watch_unscoped() {
        assert!(provider_watch_config(None).field_selector.is_none());
    }

    /// The full flag set the operator emits must parse as a unit — this is the
    /// contract between `banlieue-operator`'s Deployment builder and this CLI.
    #[test]
    fn the_flag_set_emitted_by_the_operator_parses() {
        let cli = parse(&[
            "--provider-name",
            "prod-vc",
            "--namespace",
            "tenant-a",
            "--leader-election-id",
            "banlieue-provider-vsphere-prod-vc",
            "--leader-election-namespace",
            "tenant-a",
        ]);
        assert_eq!(cli.provider_name.as_deref(), Some("prod-vc"));
        assert_eq!(cli.namespace.as_deref(), Some("tenant-a"));
        assert_eq!(cli.leader_election_id, "banlieue-provider-vsphere-prod-vc");
    }
}
