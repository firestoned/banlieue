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

    /// ADR-0176: the assertion comes from the projected token unless the
    /// operator asks for OpenBao, explicitly.
    #[test]
    fn the_assertion_source_defaults_to_the_projected_token_file() {
        if std::env::var_os("BANLIEUE_ASSERTION_SOURCE").is_some() {
            return;
        }
        assert_eq!(parse(&[]).assertion_source, AssertionSourceKind::File);
    }

    #[test]
    fn the_assertion_source_flag_selects_openbao_and_rejects_anything_else() {
        let cli = parse(&["--assertion-source", "openbao"]);
        assert_eq!(cli.assertion_source, AssertionSourceKind::Openbao);
        let cli = parse(&["--assertion-source", "file"]);
        assert_eq!(cli.assertion_source, AssertionSourceKind::File);

        let mut argv = vec!["azure", "--assertion-source", "vault"];
        argv.truncate(3);
        assert!(Wrapper::try_parse_from(argv).is_err());
    }

    fn openbao_env(name: &str) -> Option<String> {
        match name {
            "BANLIEUE_OPENBAO_ADDR" => Some("https://openbao.example.com".into()),
            "BANLIEUE_OPENBAO_AUTH_MOUNT" => Some("k8s-jwt".into()),
            "BANLIEUE_OPENBAO_LOGIN_ROLE" => Some("azure-prov".into()),
            "BANLIEUE_OPENBAO_TOKEN_ROLE" => Some("azure-prov".into()),
            "BANLIEUE_OPENBAO_SA_TOKEN_FILE" => Some("/var/run/secrets/openbao/token".into()),
            _ => None,
        }
    }

    #[test]
    fn the_file_source_needs_no_openbao_settings() {
        install_default_crypto_provider();
        let cli = parse(&["--assertion-source", "file"]);
        assert!(client_factory(&cli, |_| None).is_ok());
    }

    #[test]
    fn the_openbao_source_names_the_first_missing_setting() {
        let cli = parse(&["--assertion-source", "openbao"]);
        let err = client_factory(&cli, |_| None).err().expect("refused");
        assert!(
            format!("{err:#}").contains("BANLIEUE_OPENBAO_ADDR"),
            "{err:#}"
        );
    }

    #[test]
    fn the_openbao_source_builds_from_complete_settings() {
        install_default_crypto_provider();
        let cli = parse(&["--assertion-source", "openbao"]);
        assert!(client_factory(&cli, openbao_env).is_ok());
    }

    #[test]
    fn the_openbao_source_refuses_a_cleartext_address() {
        install_default_crypto_provider();
        let cli = parse(&["--assertion-source", "openbao"]);
        let err = client_factory(&cli, |k| {
            if k == "BANLIEUE_OPENBAO_ADDR" {
                Some("http://openbao.example.com".into())
            } else {
                openbao_env(k)
            }
        })
        .err()
        .expect("refused");
        assert!(format!("{err:#}").contains("https"), "{err:#}");
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
