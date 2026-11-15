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
        let mut argv = vec!["controller"];
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
            cli.leader_election_namespace,
            DEFAULT_LEADER_ELECTION_NAMESPACE
        );
    }

    /// ADR-0130: no scope flag leaves both unset, so the resolver falls back
    /// to the controller's own namespace instead of every namespace.
    #[test]
    fn scope_flags_default_to_unset() {
        let cli = parse(&[]);
        assert_eq!(cli.namespace_selector, None);
        assert_eq!(cli.namespace, None);
    }

    #[test]
    fn namespace_selector_flag_is_parsed() {
        let cli = parse(&["--namespace-selector", "banlieue.io/tenant=true"]);
        assert_eq!(
            cli.namespace_selector.as_deref(),
            Some("banlieue.io/tenant=true")
        );
    }

    /// The flags are not mutually exclusive: the selector wins (ADR-0130), so
    /// a manifest that sets both must still start.
    #[test]
    fn selector_and_namespace_may_both_be_given() {
        let cli = parse(&["--namespace-selector", "team=a", "--namespace", "team-a"]);
        assert!(cli.namespace_selector.is_some() && cli.namespace.is_some());
    }

    /// A change in the served set reaches every controller as a trigger.
    #[tokio::test]
    async fn scope_trigger_fires_when_the_served_set_changes() {
        let (tx, rx) = tokio::sync::watch::channel(0_u64);
        let mut trigger = Box::pin(scope_trigger(rx));
        tx.send_modify(|n| *n += 1);
        assert_eq!(futures::StreamExt::next(&mut trigger).await, Some(()));
    }

    /// ADR-0088 is Accepted (2026-10-26), so Decision 7's gate is lifted:
    /// first-boot sealing is on unless an operator turns it off.
    #[test]
    fn first_boot_sealing_is_on_by_default() {
        assert!(parse(&[]).enable_first_boot_sealing);
    }

    /// The bare flag from the gated release still parses, and still enables.
    #[test]
    fn bare_first_boot_sealing_flag_still_enables() {
        assert!(parse(&["--enable-first-boot-sealing"]).enable_first_boot_sealing);
    }

    /// An operator can still opt out, which restores ADR-0048's refusal for
    /// every image that declares `firstBootSealing`.
    #[test]
    fn first_boot_sealing_can_be_turned_off() {
        assert!(!parse(&["--enable-first-boot-sealing=false"]).enable_first_boot_sealing);
        assert!(parse(&["--enable-first-boot-sealing=true"]).enable_first_boot_sealing);
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
    fn build_leader_config_falls_back_to_default_identity() {
        let cli = parse(&[]);
        let cfg = build_leader_config(&cli);
        // Identity is non-empty (POD_NAME / HOSTNAME / "unknown").
        assert!(!cfg.identity.is_empty());
    }
}
