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
        let mut argv = vec!["operator"];
        argv.extend_from_slice(args);
        Wrapper::parse_from(argv).cli
    }

    #[test]
    fn defaults_are_applied() {
        let cli = parse(&[]);
        assert_eq!(cli.namespace, DEFAULT_NAMESPACE);
        assert_eq!(cli.health_port, DEFAULT_HEALTH_PORT);
        assert_eq!(cli.metrics_port, DEFAULT_METRICS_PORT);
        assert_eq!(cli.log_format, "text");
        assert!(!cli.no_leader_elect);
        assert_eq!(cli.leader_election_id, DEFAULT_LEADER_ELECTION_ID);
    }

    /// ADR-0131: no flag means no scope chosen, not "every namespace".
    #[test]
    fn no_watch_flag_is_set_by_default() {
        let cli = parse(&[]);
        assert_eq!(cli.watch_namespace, None);
        assert_eq!(cli.watch_namespace_selector, None);
    }

    #[test]
    fn watch_namespace_override_parses() {
        let cli = parse(&["--watch-namespace", "tenant-a"]);
        assert_eq!(cli.watch_namespace.as_deref(), Some("tenant-a"));
    }

    #[test]
    fn watch_namespace_selector_parses() {
        let cli = parse(&["--watch-namespace-selector", "banlieue.io/tenant=true"]);
        assert_eq!(
            cli.watch_namespace_selector.as_deref(),
            Some("banlieue.io/tenant=true")
        );
    }

    /// The default must not be every namespace: with no flag the operator
    /// serves the namespace it runs in.
    #[test]
    fn the_default_scope_is_the_operators_own_namespace() {
        let scope = watch_scope(&parse(&[])).unwrap();
        assert_eq!(scope, Scope::Namespace(DEFAULT_NAMESPACE.to_string()));
    }

    #[test]
    fn a_custom_install_namespace_is_the_default_scope() {
        let scope = watch_scope(&parse(&["--namespace", "banlieue-prod"])).unwrap();
        assert_eq!(scope, Scope::Namespace("banlieue-prod".to_string()));
    }

    /// `--namespace` is where the operator runs, never the scope on its own
    /// account when `--watch-namespace` is given.
    #[test]
    fn watch_namespace_wins_over_the_install_namespace() {
        let cli = parse(&[
            "--namespace",
            "banlieue-prod",
            "--watch-namespace",
            "tenant-a",
        ]);
        assert_eq!(
            watch_scope(&cli).unwrap(),
            Scope::Namespace("tenant-a".to_string())
        );
    }

    #[test]
    fn the_selector_wins_over_both_namespaces() {
        let cli = parse(&[
            "--watch-namespace",
            "tenant-a",
            "--watch-namespace-selector",
            "banlieue.io/tenant=true",
        ]);
        assert_eq!(
            watch_scope(&cli).unwrap(),
            Scope::Selector("banlieue.io/tenant=true".to_string())
        );
    }

    #[test]
    fn the_scope_is_implicit_only_when_no_watch_flag_chose_it() {
        assert!(watch_scope_is_implicit(&parse(&[])));
        assert!(!watch_scope_is_implicit(&parse(&[
            "--watch-namespace",
            "a"
        ])));
        assert!(!watch_scope_is_implicit(&parse(&[
            "--watch-namespace-selector",
            "a=b"
        ])));
    }

    #[test]
    fn port_overrides_parse() {
        let cli = parse(&["--health-port", "9091", "--metrics-port", "9090"]);
        assert_eq!(cli.health_port, 9091);
        assert_eq!(cli.metrics_port, 9090);
    }

    #[test]
    fn log_flags_parse() {
        let cli = parse(&["--log-format", "json", "--log-level", "debug"]);
        assert_eq!(cli.log_format, "json");
        assert_eq!(cli.log_level.as_deref(), Some("debug"));
    }

    #[test]
    fn leader_election_can_be_disabled() {
        assert!(parse(&["--no-leader-elect"]).no_leader_elect);
    }

    // ----------------------------------------------------------------------
    // build_leader_config
    // ----------------------------------------------------------------------

    #[test]
    fn leader_config_maps_flags_through() {
        let cli = parse(&[
            "--leader-election-namespace",
            "banlieue-system",
            "--leader-election-id",
            "banlieue-operator",
            "--leader-election-identity",
            "pod-abc",
        ]);
        let cfg = build_leader_config(&cli);

        assert_eq!(cfg.namespace, "banlieue-system");
        assert_eq!(cfg.lease_name, "banlieue-operator");
        assert_eq!(cfg.identity, "pod-abc");
    }

    #[test]
    fn leader_config_falls_back_to_a_derived_identity() {
        let cfg = build_leader_config(&parse(&[]));
        assert!(
            !cfg.identity.is_empty(),
            "identity must always resolve to something"
        );
    }

    /// The operator's Lease must not collide with the per-instance provider
    /// Leases it creates, which are named `banlieue-provider-<class>-<name>`.
    #[test]
    fn operator_lease_name_does_not_collide_with_provider_leases() {
        assert!(!DEFAULT_LEADER_ELECTION_ID.starts_with(crate::naming::WORKLOAD_NAME_PREFIX));
    }

    /// ADR-0102: four lease durations by default, overridable.
    #[test]
    fn provider_stale_after_defaults_to_sixty_seconds_and_overrides() {
        assert_eq!(
            parse(&[]).provider_stale_after_secs,
            crate::heartbeat::DEFAULT_STALE_AFTER_SECS
        );
        assert_eq!(
            parse(&["--provider-stale-after-secs", "120"]).provider_stale_after_secs,
            120
        );
    }
}
