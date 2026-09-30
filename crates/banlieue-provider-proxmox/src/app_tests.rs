// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `app.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Wrapper {
        #[command(flatten)]
        cli: Cli,
    }

    fn parse(args: &[&str]) -> Cli {
        let mut argv = vec!["proxmox"];
        argv.extend_from_slice(args);
        Wrapper::parse_from(argv).cli
    }

    #[test]
    fn defaults_are_applied() {
        let cli = parse(&[]);
        assert_eq!(cli.health_port, DEFAULT_HEALTH_PORT);
        assert_eq!(cli.log_format, "text");
        assert!(!cli.no_leader_elect);
        assert_eq!(cli.leader_election_id, DEFAULT_LEADER_ELECTION_ID);
        assert_eq!(
            cli.leader_election_namespace,
            DEFAULT_LEADER_ELECTION_NAMESPACE
        );
        assert_eq!(cli.provider_name, None);
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

    /// The operator builds this exact command line for every backend
    /// (`banlieue-operator/src/workload.rs::build_args`). A flag this CLI
    /// rejects would crash-loop the provider pod on start.
    #[test]
    fn every_flag_the_operator_passes_is_accepted() {
        let cli = parse(&[
            "--provider-name",
            "pve-a",
            "--namespace",
            "banlieue-system",
            "--leader-election-id",
            "banlieue-provider-proxmox-pve-a",
            "--leader-election-namespace",
            "banlieue-system",
            "--import-image",
            "ghcr.io/firestoned/banlieue:v0.1.0",
            "--build-toleration",
            "dedicated=build:NoSchedule",
            "--log-level",
            "debug",
            "--log-format",
            "json",
        ]);
        assert_eq!(cli.provider_name.as_deref(), Some("pve-a"));
        assert_eq!(cli.namespace.as_deref(), Some("banlieue-system"));
        assert_eq!(cli.log_format, "json");
    }

    #[test]
    fn the_provider_watch_is_narrowed_server_side_by_name() {
        let cfg = provider_watch_config(Some("pve-a"));
        assert_eq!(cfg.field_selector.as_deref(), Some("metadata.name=pve-a"));
        assert_eq!(provider_watch_config(None).field_selector, None);
    }
}
