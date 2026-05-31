// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the unified `banlieue` CLI dispatch tree.

#[cfg(test)]
mod tests {
    use super::super::*;
    use clap::CommandFactory;

    #[test]
    fn command_tree_is_valid() {
        // clap's own consistency checks (duplicate flags, bad defaults, …).
        Cli::command().debug_assert();
    }

    #[test]
    fn controller_subcommand_parses() {
        let cli = Cli::parse_from(["banlieue", "controller", "--no-leader-elect"]);
        assert!(matches!(cli.command, Command::Controller(_)));
    }

    #[test]
    #[cfg(feature = "vsphere")]
    fn provider_vsphere_subcommand_parses() {
        let cli = Cli::parse_from(["banlieue", "provider", "vsphere", "--no-leader-elect"]);
        match cli.command {
            Command::Provider(p) => assert!(matches!(p.backend, ProviderBackend::Vsphere(_))),
            _ => panic!("expected provider subcommand"),
        }
    }

    #[test]
    fn missing_subcommand_is_an_error() {
        // No role given → clap returns an error rather than a parsed Cli.
        assert!(Cli::try_parse_from(["banlieue"]).is_err());
    }

    #[test]
    fn unknown_provider_backend_is_an_error() {
        assert!(Cli::try_parse_from(["banlieue", "provider", "nope"]).is_err());
    }

    #[test]
    fn completion_subcommand_parses_zsh() {
        let cli = Cli::parse_from(["banlieue", "completion", "zsh"]);
        match cli.command {
            Command::Completion(args) => assert_eq!(args.shell, clap_complete::Shell::Zsh),
            _ => panic!("expected completion subcommand"),
        }
    }

    #[test]
    fn completion_accepts_other_shells() {
        for sh in ["bash", "fish", "elvish", "powershell"] {
            assert!(
                Cli::try_parse_from(["banlieue", "completion", sh]).is_ok(),
                "shell {sh} should parse"
            );
        }
    }

    #[test]
    fn completion_rejects_unknown_shell() {
        assert!(Cli::try_parse_from(["banlieue", "completion", "tcsh"]).is_err());
    }

    #[test]
    fn completion_requires_a_shell() {
        assert!(Cli::try_parse_from(["banlieue", "completion"]).is_err());
    }

    #[test]
    fn zsh_completion_script_is_non_empty_and_covers_the_tree() {
        let mut buf: Vec<u8> = Vec::new();
        write_completion(clap_complete::Shell::Zsh, &mut buf);
        let script = String::from_utf8(buf).expect("utf-8 completion");
        // zsh scripts open with the #compdef directive naming the binary.
        assert!(
            script.contains("#compdef banlieue"),
            "missing compdef header"
        );
        // The subcommand tree should be reflected in the script.
        assert!(
            script.contains("controller"),
            "missing controller subcommand"
        );
        assert!(script.contains("provider"), "missing provider subcommand");
        assert!(
            script.contains("completion"),
            "missing completion subcommand"
        );
    }

    #[test]
    fn bash_completion_script_names_the_binary() {
        let mut buf: Vec<u8> = Vec::new();
        write_completion(clap_complete::Shell::Bash, &mut buf);
        let script = String::from_utf8(buf).expect("utf-8 completion");
        assert!(!script.is_empty());
        assert!(
            script.contains("banlieue"),
            "bash script should name the binary"
        );
    }
}
