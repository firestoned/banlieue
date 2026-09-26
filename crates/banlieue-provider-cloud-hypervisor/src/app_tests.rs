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
        let mut argv = vec!["cloud-hypervisor"];
        argv.extend_from_slice(args);
        Wrapper::parse_from(argv).cli
    }

    fn config() -> HostConfig {
        HostConfig::parse(
            r#"
[provider]
name = "ch-a"
namespace = "banlieue-system"
kubeconfig = "/etc/banlieue/kubeconfig"
[vmm]
binary = "/usr/local/bin/cloud-hypervisor"
version = "v53.0"
firmware = "/opt/banlieue/firmware/f/CLOUDHV.fd"
[paths]
run_root = "/run/banlieue/ch"
state_root = "/var/lib/banlieue"
[guests]
uid_base = 2000000
uid_count = 10
[storage_classes]
fast = "/srv/banlieue/ch"
[network_classes]
lan = "br0"
"#,
        )
        .unwrap()
    }

    #[test]
    fn defaults_are_production() {
        let cli = parse(&[]);
        assert_eq!(cli.config, PathBuf::from(DEFAULT_CONFIG_PATH));
        assert_eq!(cli.bus, BusArg::System);
        assert!(!cli.no_leader_elect);
        assert_eq!(cli.health_port, DEFAULT_HEALTH_PORT);
    }

    #[test]
    fn session_bus_is_selectable_for_development() {
        let cli = parse(&["--bus", "session", "--config", "/tmp/x.toml"]);
        assert_eq!(cli.bus, BusArg::Session);
        assert_eq!(systemd::Bus::from(cli.bus), systemd::Bus::Session);
        assert_eq!(cli.config, PathBuf::from("/tmp/x.toml"));
    }

    #[test]
    /// The Lease is the one the operator's Role grants an External
    /// provider by name: the operator's workload name for this Provider.
    fn the_lease_is_the_one_the_operator_grants() {
        let lc = leader_config(&config(), "cloud-hypervisor", Some("id-1".into()));
        assert_eq!(lc.namespace, "banlieue-system");
        assert_eq!(
            lc.lease_name,
            banlieue_provider_sdk::naming::workload_name("cloud-hypervisor", "ch-a")
        );
        assert_eq!(lc.identity, "id-1");
    }

    #[test]
    fn the_provider_watch_is_narrowed_to_this_host() {
        let wc = provider_watch_config(&config());
        assert_eq!(wc.field_selector.as_deref(), Some("metadata.name=ch-a"));
    }

    /// The import unit's command line parses back into the subcommand.
    #[test]
    fn the_import_subcommand_parses_after_the_config_flag() {
        let cli = parse(&[
            "--config",
            "/etc/banlieue/cloud-hypervisor.toml",
            "import",
            "--reference",
            "registry.internal:5000/d@sha256:0",
            "--file",
            "sha256-0.raw",
        ]);
        assert_eq!(
            cli.config,
            std::path::Path::new("/etc/banlieue/cloud-hypervisor.toml")
        );
        let Some(ChCommand::Import(args)) = cli.command else {
            panic!("expected import");
        };
        assert_eq!(args.file, "sha256-0.raw");
    }
}
