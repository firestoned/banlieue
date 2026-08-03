// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::app`].

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
        let mut argv = vec!["libvirt"];
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

    // ---------- import Job identity --------------------------------------

    #[test]
    fn the_import_job_runs_as_the_dedicated_read_only_identity() {
        // Not the controller's own SA: that one can create Jobs, so a workload
        // in the privileged build namespace holding it could create further
        // privileged pods (ADR-0016 §4).
        let cli = parse(&[]);
        assert_eq!(cli.import_service_account, "banlieue-import");
        assert_ne!(
            cli.import_service_account, cli.leader_election_id,
            "the import identity must be distinct from the controller's"
        );
    }

    #[test]
    fn the_build_namespace_is_the_privileged_one_not_the_control_plane() {
        // kairos build pods cannot be admitted under `restricted`, so the
        // artifacts PVC — and therefore the import Job — live elsewhere.
        let cli = parse(&[]);
        assert_eq!(cli.build_namespace, "banlieue-imagebuild");
        assert_ne!(cli.build_namespace, DEFAULT_LEADER_ELECTION_NAMESPACE);
    }

    // ---------- watch scoping ---------------------------------------------

    #[test]
    fn provider_name_narrows_the_watch_server_side() {
        // Server-side, so this pod's informer cache holds only its own
        // Provider and one hung backend cannot stall the others (ADR-0003).
        let cfg = provider_watch_config(Some("prod-kvm"));
        assert_eq!(
            cfg.field_selector.as_deref(),
            Some("metadata.name=prod-kvm")
        );
    }

    #[test]
    fn without_a_provider_name_every_provider_is_watched() {
        assert!(provider_watch_config(None).field_selector.is_none());
    }

    #[test]
    fn the_operator_supplied_provider_name_flag_parses() {
        // The operator passes --provider-name to every backend; a libvirt
        // provider that rejected it would crash-loop before its first
        // reconcile.
        let cli = parse(&["--provider-name", "prod-kvm"]);
        assert_eq!(cli.provider_name.as_deref(), Some("prod-kvm"));
    }

    // ---------- the Job's argv must actually parse -------------------------

    /// The import Job execs `banlieue` with args the reconciler generates. If
    /// those args and this CLI ever disagree, nothing fails until a Job is
    /// already running in a cluster — the container exits on a clap error and
    /// the only symptom is a failed import. Parse the real manifest's argv
    /// through the real parser instead.
    #[test]
    fn the_generated_import_job_argv_parses_into_import_args() {
        use banlieue_api::banlieue::{
            ProviderCapabilities, ProviderConnection, ProviderSpec, RawDiskArtifactPhase,
            RawDiskArtifactStatus,
        };
        use banlieue_api::common::LocalObjectReference;
        use kube::api::ObjectMeta;

        let provider = Provider {
            metadata: ObjectMeta {
                name: Some("kvm-1".into()),
                namespace: Some("banlieue-system".into()),
                ..Default::default()
            },
            spec: ProviderSpec {
                provider_class_ref: LocalObjectReference {
                    name: "libvirt".into(),
                },
                connection: ProviderConnection {
                    endpoint: "qemu+tls://kvm-1.example/system".into(),
                    credentials_ref: LocalObjectReference {
                        name: "creds".into(),
                    },
                    insecure_skip_tls_verify: false,
                    ca_bundle: None,
                },
                capabilities: ProviderCapabilities::default(),
                paused: false,
            },
            status: None,
        };
        let artifact = RawDiskArtifactStatus {
            phase: RawDiskArtifactPhase::Ready,
            os_artifact_ref: "build".into(),
            pvc_ref: Some(LocalObjectReference {
                name: "artifacts".into(),
            }),
            disk_file: Some("kairos.raw".into()),
            reason: None,
            message: None,
            checksum: None,
        };

        let job = crate::reconciler::vmimage::build_import_job(
            &crate::reconciler::vmimage::ImportJobInputs {
                job_name: "import-x",
                namespace: "banlieue-system",
                image: "ghcr.io/example/banlieue:v1",
                service_account: Some("sa"),
                vmimage: "kairos-ubuntu",
                provider: &provider,
                pool: "default",
                artifact: &artifact,
                tolerations: &[],
            },
        );

        let argv: Vec<String> = job["spec"]["template"]["spec"]["containers"][0]["args"]
            .as_array()
            .expect("Job carries args")
            .iter()
            .map(|v| v.as_str().expect("every arg is a string").to_string())
            .collect();

        // Drop the `provider libvirt` prefix the top-level binary consumes.
        assert_eq!(&argv[0..2], &["provider", "libvirt"]);
        let mut rest = vec!["libvirt"];
        rest.extend(argv[2..].iter().map(String::as_str));

        let cli = Wrapper::parse_from(rest).cli;
        let Some(LibvirtCommand::Import(args)) = cli.command else {
            panic!("the Job's argv must select the import subcommand");
        };
        assert_eq!(args.vmimage, "kairos-ubuntu");
        assert_eq!(args.provider, "kvm-1");
        assert_eq!(args.provider_namespace, "banlieue-system");
        assert_eq!(args.pool, "default");
        assert_eq!(
            args.source,
            std::path::PathBuf::from("/artifacts/kairos.raw")
        );
        // No checksum on the artifact → no --checksum flag.
        assert_eq!(args.checksum, None);

        // SEC-004: a published checksum must reach the subcommand verbatim.
        let artifact_with_checksum = RawDiskArtifactStatus {
            checksum: Some(
                "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into(),
            ),
            ..artifact
        };
        let job = crate::reconciler::vmimage::build_import_job(
            &crate::reconciler::vmimage::ImportJobInputs {
                job_name: "import-x",
                namespace: "banlieue-system",
                image: "ghcr.io/example/banlieue:v1",
                service_account: Some("sa"),
                vmimage: "kairos-ubuntu",
                provider: &provider,
                pool: "default",
                artifact: &artifact_with_checksum,
                tolerations: &[],
            },
        );
        let argv: Vec<String> = job["spec"]["template"]["spec"]["containers"][0]["args"]
            .as_array()
            .expect("Job carries args")
            .iter()
            .map(|v| v.as_str().expect("every arg is a string").to_string())
            .collect();
        let mut rest = vec!["libvirt"];
        rest.extend(argv[2..].iter().map(String::as_str));
        let cli = Wrapper::parse_from(rest).cli;
        let Some(LibvirtCommand::Import(args)) = cli.command else {
            panic!("the Job's argv must select the import subcommand");
        };
        assert_eq!(
            args.checksum.as_deref(),
            Some("sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08")
        );
    }
}
