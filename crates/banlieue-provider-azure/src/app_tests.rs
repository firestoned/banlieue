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
        let mut argv = vec!["azure"];
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
        if std::env::var_os("AZURE_FEDERATED_TOKEN_FILE").is_some() {
            return;
        }
        assert_eq!(
            parse(&[]).federated_token_file,
            Path::new(DEFAULT_FEDERATED_TOKEN_FILE)
        );
    }

    #[test]
    fn the_token_file_flag_is_parsed() {
        let cli = parse(&["--federated-token-file", "/tmp/token"]);
        assert_eq!(cli.federated_token_file, Path::new("/tmp/token"));
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
            "az-east",
            "--namespace",
            "banlieue-system",
            "--leader-election-id",
            "banlieue-provider-azure-az-east",
            "--leader-election-namespace",
            "banlieue-system",
            "--watch-namespace-selector",
            "banlieue.io/tenant=true",
            "--import-image",
            "ghcr.io/firestoned/banlieue:v0.1.0",
            "--build-toleration",
            "dedicated=build:NoSchedule",
            "--log-level",
            "debug",
            "--log-format",
            "json",
        ]);
        assert_eq!(cli.provider_name.as_deref(), Some("az-east"));
        assert_eq!(cli.namespace.as_deref(), Some("banlieue-system"));
        assert_eq!(cli.log_format, "json");
        // The publish Jobs run the image the operator names (ADR-0162).
        assert_eq!(cli.import_image, "ghcr.io/firestoned/banlieue:v0.1.0");
        assert_eq!(cli.build_toleration, vec!["dedicated=build:NoSchedule"]);
    }

    #[test]
    fn the_publish_jobs_default_to_the_imagebuild_namespace_and_publisher() {
        if std::env::var_os("BANLIEUE_BUILD_NAMESPACE").is_some()
            || std::env::var_os("BANLIEUE_PUBLISHER_SERVICE_ACCOUNT").is_some()
        {
            return;
        }
        let cli = parse(&[]);
        assert_eq!(cli.build_namespace, DEFAULT_BUILD_NAMESPACE);
        assert_eq!(cli.publisher_service_account, "banlieue-azure-publisher");
    }

    #[test]
    fn a_publish_job_maps_to_its_vmimage() {
        use k8s_openapi::api::batch::v1::Job;
        use kube::api::ObjectMeta;

        let labelled = Job {
            metadata: ObjectMeta {
                labels: Some(std::collections::BTreeMap::from([(
                    LABEL_VMIMAGE.to_string(),
                    "kairos".to_string(),
                )])),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            vmimage_ref_from_job(labelled).map(|r| r.name),
            Some("kairos".to_string())
        );
        assert!(vmimage_ref_from_job(Job::default()).is_none());
    }
}
