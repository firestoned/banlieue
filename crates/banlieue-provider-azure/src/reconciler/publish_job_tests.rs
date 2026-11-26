// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the publish Job's manifest, name, state and output.

#[cfg(test)]
mod tests {
    use banlieue_api::banlieue::{
        AzureImageGallery, AzurePublisherIdentity, BuildArtifactKind, BuildArtifactPhase,
        BuildArtifactStatus, ProviderCapabilities,
    };
    use banlieue_api::common::LocalObjectReference;
    use k8s_openapi::api::batch::v1::{JobCondition, JobStatus};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
    use k8s_openapi::jiff::{SignedDuration, Timestamp};

    use super::super::*;
    use crate::test_support::{self, HOME, PROVIDER};

    const VMIMAGE_UID: &str = "11111111-2222-3333-4444-555555555555";
    const ARTIFACT_UID: &str = "66666666-7777-8888-9999-000000000000";
    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn vmimage() -> VMImage {
        let mut image: VMImage = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "VMImage",
            "metadata": { "name": "kairos", "uid": VMIMAGE_UID },
            "spec": {
                "osFamily": "linux",
                "osDistribution": "kairos",
                "osVersion": "4.4",
                "architecture": "amd64",
                "sources": [],
            },
        }))
        .unwrap();
        image.metadata.generation = Some(1);
        image
    }

    fn artifact() -> BuildArtifactStatus {
        BuildArtifactStatus {
            kind: BuildArtifactKind::CloudImage,
            phase: BuildArtifactPhase::Ready,
            os_artifact_ref: "kairos-abc".into(),
            os_artifact_uid: Some(ARTIFACT_UID.into()),
            pvc_ref: Some(LocalObjectReference {
                name: "kairos-abc-artifacts".into(),
            }),
            file: Some("kairos-abc.raw".into()),
            reason: None,
            message: None,
            checksum: Some(DIGEST.into()),
            oci_artifact: None,
            first_boot_sealing_layer: false,
            prelaid_disk_size_gi_b: None,
        }
    }

    fn gallery() -> AzureImageGallery {
        AzureImageGallery {
            resource_group: "rg-img".into(),
            name: "banlieue".into(),
        }
    }

    fn publisher() -> AzurePublisherIdentity {
        AzurePublisherIdentity {
            client_id: "00000000-0000-0000-0000-000000000003".into(),
        }
    }

    fn manifest(mode: Mode, trusted_launch: bool) -> Value {
        let provider = test_support::provider(ProviderCapabilities::default());
        let azure = test_support::azure();
        let image = vmimage();
        let artifact = artifact();
        let gallery = gallery();
        let publisher = publisher();
        build_job(&JobInputs {
            job_name: "job",
            namespace: "banlieue-imagebuild",
            image: "ghcr.io/firestoned/banlieue:v0.0.0",
            service_account: DEFAULT_PUBLISHER_SERVICE_ACCOUNT,
            mode,
            vmimage: &image,
            provider: &provider,
            azure: &azure,
            gallery: &gallery,
            publisher: &publisher,
            definition: "kairos",
            version: "1.2.3",
            artifact: (mode == Mode::Publish).then_some(&artifact),
            checksum: (mode == Mode::Publish).then_some(DIGEST),
            trusted_launch,
            tolerations: &[],
        })
    }

    fn args_of(job: &Value) -> Vec<String> {
        serde_json::from_value(job["spec"]["template"]["spec"]["containers"][0]["args"].clone())
            .unwrap()
    }

    fn flag(args: &[String], name: &str) -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    }

    #[test]
    fn the_publish_job_runs_image_publish_with_the_providers_settings() {
        let job = manifest(Mode::Publish, false);
        let args = args_of(&job);
        assert_eq!(&args[..3], ["provider", "azure", "image-publish"]);
        assert_eq!(flag(&args, "--mode").as_deref(), Some("publish"));
        assert_eq!(
            flag(&args, "--endpoint").as_deref(),
            Some(test_support::ENDPOINT)
        );
        assert_eq!(
            flag(&args, "--tenant-id").as_deref(),
            Some(test_support::TENANT)
        );
        // The publisher's identity, never the runtime provider's.
        assert_eq!(
            flag(&args, "--client-id").as_deref(),
            Some(publisher().client_id.as_str())
        );
        assert_ne!(
            flag(&args, "--client-id").as_deref(),
            Some(test_support::CLIENT)
        );
        assert_eq!(
            flag(&args, "--subscription-id").as_deref(),
            Some(test_support::SUBSCRIPTION)
        );
        assert_eq!(
            flag(&args, "--location").as_deref(),
            Some(test_support::LOCATION)
        );
        assert_eq!(
            flag(&args, "--gallery-resource-group").as_deref(),
            Some("rg-img")
        );
        assert_eq!(flag(&args, "--gallery-name").as_deref(), Some("banlieue"));
        assert_eq!(flag(&args, "--image-definition").as_deref(), Some("kairos"));
        assert_eq!(flag(&args, "--version").as_deref(), Some("1.2.3"));
        assert_eq!(
            flag(&args, "--source").as_deref(),
            Some("/artifacts/kairos-abc.raw")
        );
        assert_eq!(flag(&args, "--checksum").as_deref(), Some(DIGEST));
        assert_eq!(flag(&args, "--owner-uid").as_deref(), Some(VMIMAGE_UID));
        assert_eq!(
            flag(&args, "--provider"),
            Some(format!("{HOME}.{PROVIDER}"))
        );
        assert_eq!(
            flag(&args, "--federated-token-file").as_deref(),
            Some("/var/run/secrets/azure/tokens/azure-identity-token")
        );
        assert!(flag(&args, "--authority-host").is_none());
        assert!(!args.iter().any(|a| a == "--trusted-launch"));
        assert!(
            args_of(&manifest(Mode::Publish, true))
                .iter()
                .any(|a| a == "--trusted-launch")
        );
    }

    #[test]
    fn the_pod_is_restricted_and_holds_only_the_projected_token() {
        let job = manifest(Mode::Publish, false);
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(pod["serviceAccountName"], DEFAULT_PUBLISHER_SERVICE_ACCOUNT);
        assert_eq!(pod["automountServiceAccountToken"], false);
        assert_eq!(pod["securityContext"]["runAsNonRoot"], true);
        assert_eq!(
            pod["securityContext"]["seccompProfile"]["type"],
            "RuntimeDefault"
        );
        let container = &pod["containers"][0];
        assert_eq!(
            container["securityContext"]["allowPrivilegeEscalation"],
            false
        );
        assert_eq!(container["securityContext"]["readOnlyRootFilesystem"], true);
        assert_eq!(
            container["securityContext"]["capabilities"]["drop"][0],
            "ALL"
        );

        let volumes = pod["volumes"].as_array().unwrap();
        assert!(volumes.iter().all(|v| v.get("secret").is_none()));
        let token = volumes.iter().find(|v| v["name"] == TOKEN_FILE).unwrap();
        let projection = &token["projected"]["sources"][0]["serviceAccountToken"];
        assert_eq!(projection["audience"], "api://AzureADTokenExchange");
        assert_eq!(projection["expirationSeconds"], TOKEN_EXPIRATION_SECS);
        let pvc = volumes.iter().find(|v| v["name"] == "artifacts").unwrap();
        assert_eq!(
            pvc["persistentVolumeClaim"]["claimName"],
            "kairos-abc-artifacts"
        );
        assert_eq!(pvc["persistentVolumeClaim"]["readOnly"], true);
        let mounts = container["volumeMounts"].as_array().unwrap();
        let artifacts = mounts.iter().find(|m| m["name"] == "artifacts").unwrap();
        assert_eq!(artifacts["mountPath"], "/artifacts");
        assert_eq!(artifacts["readOnly"], true);
    }

    #[test]
    fn the_job_is_bounded_labelled_and_owned_by_its_osartifact() {
        let job = manifest(Mode::Publish, false);
        assert_eq!(job["metadata"]["namespace"], "banlieue-imagebuild");
        assert_eq!(job["spec"]["activeDeadlineSeconds"], ACTIVE_DEADLINE_SECS);
        const { assert!(ACTIVE_DEADLINE_SECS > 90 * 60) };
        assert_eq!(job["spec"]["backoffLimit"], BACKOFF_LIMIT);
        let labels = &job["metadata"]["labels"];
        assert_eq!(labels[LABEL_VMIMAGE], "kairos");
        assert_eq!(labels[LABEL_COMPONENT], COMPONENT);
        assert_eq!(labels[LABEL_MODE], "publish");
        assert_eq!(
            job["spec"]["template"]["metadata"]["labels"][LABEL_COMPONENT],
            COMPONENT
        );
        let owner = &job["metadata"]["ownerReferences"][0];
        assert_eq!(owner["kind"], "OSArtifact");
        assert_eq!(owner["uid"], ARTIFACT_UID);
    }

    #[test]
    fn a_delete_job_mounts_no_build_and_is_owned_by_its_vmimage() {
        let job = manifest(Mode::Delete, false);
        let args = args_of(&job);
        assert_eq!(flag(&args, "--mode").as_deref(), Some("delete"));
        assert!(flag(&args, "--source").is_none());
        assert!(flag(&args, "--checksum").is_none());
        let pod = &job["spec"]["template"]["spec"];
        assert!(
            pod["volumes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["name"] != "artifacts")
        );
        let owner = &job["metadata"]["ownerReferences"][0];
        assert_eq!(owner["kind"], "VMImage");
        assert_eq!(owner["uid"], VMIMAGE_UID);
        assert_eq!(job["metadata"]["labels"][LABEL_MODE], "delete");
    }

    #[test]
    fn job_names_are_deterministic_bounded_and_distinct() {
        let provider = test_support::provider(ProviderCapabilities::default());
        let long_definition = "a-very-long-vmimage-name-that-goes-on-and-on-and-on-tl";
        let a = job_name(Mode::Publish, &provider, long_definition, "1.2.3");
        assert_eq!(
            a,
            job_name(Mode::Publish, &provider, long_definition, "1.2.3")
        );
        assert!(a.len() <= banlieue_provider_sdk::k8s_name::MAX_NAME_LEN);
        assert_ne!(
            a,
            job_name(Mode::Publish, &provider, long_definition, "1.2.4")
        );
        assert_ne!(
            a,
            job_name(Mode::Delete, &provider, long_definition, "1.2.3")
        );
        let mut other = provider.clone();
        other.metadata.namespace = Some("elsewhere".into());
        assert_ne!(a, job_name(Mode::Publish, &other, long_definition, "1.2.3"));
    }

    fn failed_job(failed_secs_ago: i64, now: Timestamp) -> Job {
        Job {
            status: Some(JobStatus {
                failed: Some(1),
                conditions: Some(vec![JobCondition {
                    type_: "Failed".into(),
                    status: "True".into(),
                    last_transition_time: Some(Time(
                        now - SignedDuration::from_secs(failed_secs_ago),
                    )),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn job_state_reads_absent_running_succeeded_and_failed() {
        let now = Timestamp::now();
        assert_eq!(job_state(None, now), JobState::Absent);
        assert_eq!(job_state(Some(&Job::default()), now), JobState::Running);
        let succeeded = Job {
            status: Some(JobStatus {
                succeeded: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(job_state(Some(&succeeded), now), JobState::Succeeded);
        assert_eq!(
            job_state(Some(&failed_job(1, now)), now),
            JobState::Failed { retry_due: false }
        );
        assert_eq!(
            job_state(Some(&failed_job(PUBLISH_RETRY_AFTER_SECS, now)), now),
            JobState::Failed { retry_due: true }
        );
    }

    #[test]
    fn termination_messages_parse_into_the_three_shapes() {
        assert_eq!(
            parse_output(
                r#"{"versionId":"/subscriptions/s/x/versions/1.2.3","sourceDigest":"sha256:ab","minOsDiskSizeGiB":64}"#
            ),
            Some(JobOutput::Published {
                version_id: "/subscriptions/s/x/versions/1.2.3".into(),
                source_digest: "sha256:ab".into(),
                min_os_disk_size_gib: Some(64),
            })
        );
        assert_eq!(
            parse_output(r#"{"deleted":true}"#),
            Some(JobOutput::Deleted)
        );
        assert_eq!(
            parse_output(r#"{"error":"QuotaExceeded","message":"no"}"#),
            Some(JobOutput::Error {
                code: "QuotaExceeded".into(),
                message: "no".into(),
            })
        );
        assert_eq!(parse_output("panicked at src/main.rs"), None);
        assert_eq!(parse_output(r#"{"versionId":"x"}"#), None);
    }

    #[test]
    fn the_watch_selects_only_publish_jobs() {
        assert_eq!(
            job_watch_selector(),
            "app.kubernetes.io/component=azure-publish"
        );
    }
}
