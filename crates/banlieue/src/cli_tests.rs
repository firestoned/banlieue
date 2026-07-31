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
    #[cfg(feature = "imagebuilder")]
    fn imagebuilder_subcommand_parses() {
        let cli = Cli::parse_from(["banlieue", "imagebuilder", "--no-leader-elect"]);
        assert!(matches!(cli.command, Command::Imagebuilder(_)));
    }

    #[test]
    #[cfg(feature = "libvirt")]
    fn provider_libvirt_subcommand_parses() {
        let cli = Cli::parse_from(["banlieue", "provider", "libvirt", "--no-leader-elect"]);
        match cli.command {
            Command::Provider(p) => assert!(matches!(p.backend, ProviderBackend::Libvirt(_))),
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
        #[cfg(feature = "imagebuilder")]
        assert!(
            script.contains("imagebuilder"),
            "missing imagebuilder subcommand"
        );
        assert!(
            script.contains("completion"),
            "missing completion subcommand"
        );
    }

    // ----------------------------------------------------------------------
    // operator + bootstrap (ADR-0012 / ADR-0013)
    // ----------------------------------------------------------------------

    #[test]
    fn operator_subcommand_parses() {
        let cli = Cli::parse_from(["banlieue", "operator", "--no-leader-elect"]);
        assert!(matches!(cli.command, Command::Operator(_)));
    }

    #[test]
    fn bootstrap_operator_subcommand_parses() {
        let cli = Cli::parse_from(["banlieue", "bootstrap", "operator", "--dry-run"]);
        assert!(matches!(cli.command, Command::Bootstrap(_)));
    }

    #[test]
    fn bootstrap_operator_accepts_the_install_flags() {
        use banlieue_operator::bootstrap::BootstrapTarget;
        let cli = Cli::parse_from([
            "banlieue",
            "bootstrap",
            "operator",
            "--namespace",
            "banlieue-prod",
            "--version",
            "v9.9.9",
            "--registry",
            "registry.internal:5000",
        ]);
        match cli.command {
            Command::Bootstrap(b) => match b.target {
                BootstrapTarget::Operator { common, .. } => {
                    assert_eq!(common.namespace, "banlieue-prod");
                    assert_eq!(common.version, "v9.9.9");
                    assert_eq!(common.registry.as_deref(), Some("registry.internal:5000"));
                    assert!(!common.dry_run);
                }
                _ => panic!("expected the operator target"),
            },
            _ => panic!("expected bootstrap subcommand"),
        }
    }

    #[test]
    fn bootstrap_provider_requires_a_backend() {
        assert!(Cli::try_parse_from(["banlieue", "bootstrap", "provider"]).is_err());
    }

    #[test]
    fn bootstrap_requires_a_target() {
        assert!(Cli::try_parse_from(["banlieue", "bootstrap"]).is_err());
    }

    /// The bootstrap CLI offers backends by name at runtime, so the compiled-in
    /// list must actually reflect the enabled features.
    #[test]
    #[cfg(feature = "vsphere")]
    fn compiled_backends_includes_vsphere() {
        assert!(COMPILED_BACKENDS.contains(&"vsphere"));
    }

    #[test]
    fn compiled_backends_is_never_empty_in_a_default_build() {
        assert!(
            !COMPILED_BACKENDS.is_empty(),
            "a default build must ship at least one backend"
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

    // ---------- cross-crate contract ------------------------------------

    /// `banlieue-imagebuilder` creates the artifacts PVC in its
    /// `--build-namespace`; the libvirt provider mounts that PVC into the
    /// import Job it creates in *its* `--build-namespace`. A PVC cannot be
    /// mounted across namespaces (ADR-0010), so the two defaults must agree or
    /// the documented install is broken out of the box.
    ///
    /// Each crate's own tests pass regardless — they only ever see one side.
    /// This is the only crate that links both, so it is the only place the
    /// disagreement is visible. It was a real bug: the imagebuilder defaulted
    /// to `banlieue-imagebuild` and the provider to `banlieue-system`, and the
    /// mismatch only surfaced on a real cluster.
    #[test]
    fn imagebuilder_and_libvirt_provider_agree_on_the_build_namespace() {
        use clap::Parser;

        #[derive(Parser)]
        struct ImagebuilderWrapper {
            #[command(flatten)]
            cli: banlieue_imagebuilder::Cli,
        }
        #[derive(Parser)]
        struct LibvirtWrapper {
            #[command(flatten)]
            cli: banlieue_provider_libvirt::Cli,
        }

        let ib = ImagebuilderWrapper::parse_from(["imagebuilder"]).cli;
        let lv = LibvirtWrapper::parse_from(["libvirt"]).cli;

        // Tolerations must agree: if the artifacts volume lands on a tainted
        // build node, both the build pod and the import Job that mounts it
        // need permission to be there.
        assert_eq!(
            ib.build_toleration, lv.build_toleration,
            "both must accept the same --build-toleration flag"
        );
        // The node SELECTOR is deliberately imagebuilder-only. A build pod is
        // placed by policy; an import Job is placed by the PVC it mounts, and
        // the scheduler resolves that from the bound PV without our help.
        assert_eq!(
            ib.build_namespace, lv.build_namespace,
            "imagebuilder writes the artifacts PVC into {:?} but the libvirt \
             provider mounts it from {:?}; a PVC cannot cross namespaces",
            ib.build_namespace, lv.build_namespace
        );
    }
}
