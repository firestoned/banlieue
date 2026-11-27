// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `app.rs`.

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Wrapper {
        #[command(flatten)]
        cli: Cli,
    }

    fn parse(args: &[&str]) -> Cli {
        let mut argv = vec!["ec2"];
        argv.extend_from_slice(args);
        Wrapper::parse_from(argv).cli
    }

    #[test]
    fn defaults_are_applied() {
        let cli = parse(&[]);
        assert_eq!(cli.health_port, DEFAULT_HEALTH_PORT);
        assert!(!cli.no_leader_elect);
        assert_eq!(cli.leader_election_id, DEFAULT_LEADER_ELECTION_ID);
        assert_eq!(cli.provider_name, None);
        assert_eq!(cli.watch_namespace_selector, None);
    }

    /// ADR-0159: the token path defaults to where the operator mounts it
    /// (unless the environment overrides it, as the operator does).
    #[test]
    fn the_token_file_defaults_to_the_projected_token_path() {
        if std::env::var_os("AWS_WEB_IDENTITY_TOKEN_FILE").is_some() {
            return;
        }
        assert_eq!(
            parse(&[]).web_identity_token_file,
            Path::new(DEFAULT_WEB_IDENTITY_TOKEN_FILE)
        );
    }

    #[test]
    fn the_token_file_and_scope_flags_are_parsed() {
        let cli = parse(&[
            "--web-identity-token-file",
            "/tmp/token",
            "--provider-name",
            "ec2-cc",
            "--watch-namespace-selector",
            "banlieue.io/tenant=true",
        ]);
        assert_eq!(cli.web_identity_token_file, Path::new("/tmp/token"));
        assert_eq!(cli.provider_name.as_deref(), Some("ec2-cc"));
        assert_eq!(
            cli.watch_namespace_selector.as_deref(),
            Some("banlieue.io/tenant=true")
        );
    }

    #[test]
    fn the_role_names_the_binary() {
        assert_eq!(ROLE, "banlieue-provider-ec2");
    }
}
