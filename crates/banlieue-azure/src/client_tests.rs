// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `client.rs`, against a one-shot local server that answers
//! with what ARM answered in the roadmap 23 spike (`fixtures/`).

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use async_trait::async_trait;

    use super::super::*;
    use crate::api::{ArmApi, PowerAction};
    use crate::auth::TokenProvider;
    use crate::error::{Error, Result};
    use crate::operation::{OperationHandle, OperationKind, OperationStatus};
    use crate::tags::OwnershipTags;
    use crate::test_server::{Recorded, install_provider, ok, respond, serve, serve_with};
    use crate::token::{AccessToken, parse_token_response};
    use crate::vm::{DEFAULT_ADMIN_USERNAME, NicRequest, VmRequest};

    const SUB: &str = "00000000-0000-0000-0000-000000000003";
    const RG: &str = "rg-banlieue-example";
    const NIC: &str = "banlieue-uid-nic";
    const VM: &str = "banlieue-uid";
    const BEARER: &str = "arm-token";
    const NIC_JSON: &str = include_str!("../fixtures/nic.json");
    const RUNNING_JSON: &str = include_str!("../fixtures/instance-view-running.json");
    const USAGES_JSON: &str = include_str!("../fixtures/usages.json");
    const SKUS_JSON: &str = include_str!("../fixtures/resource-skus.json");
    const RESOURCES_JSON: &str = include_str!("../fixtures/resources.json");

    /// Counts exchanges and invalidations; never talks to anyone.
    #[derive(Default)]
    struct CountingToken {
        invalidated: AtomicU32,
    }

    #[async_trait]
    impl TokenProvider for CountingToken {
        async fn token(&self) -> Result<AccessToken> {
            parse_token_response(
                format!(r#"{{"access_token":"{BEARER}","expires_in":3600}}"#).as_bytes(),
                Instant::now(),
            )
        }
        async fn invalidate(&self) {
            self.invalidated.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn client_with(endpoint: &str, token: Arc<CountingToken>) -> ArmClient {
        install_provider();
        let mut config = ArmConfig::new(SUB);
        config.endpoint = endpoint.to_string();
        ArmClient::new(config, token).expect("client")
    }

    fn client(endpoint: &str) -> ArmClient {
        client_with(endpoint, Arc::new(CountingToken::default()))
    }

    fn rg_path(rest: &str) -> String {
        format!("/subscriptions/{SUB}/resourceGroups/{RG}/providers/{rest}")
    }

    fn tags() -> std::collections::BTreeMap<String, String> {
        OwnershipTags::new("uid", "AzureVirtualMachine", "ns", "p").to_map()
    }

    fn nic_request() -> NicRequest {
        NicRequest {
            location: "eastus2".to_string(),
            subnet_id: "/subscriptions/s/resourceGroups/net/providers/Microsoft.Network/virtualNetworks/v/subnets/sn".to_string(),
            accelerated_networking: true,
            tags: tags(),
        }
    }

    fn vm_request() -> VmRequest {
        VmRequest {
            location: "eastus2".to_string(),
            zone: "1".to_string(),
            size: "Standard_D2s_v7".to_string(),
            image_version_id: "/subscriptions/s/resourceGroups/g/providers/Microsoft.Compute/galleries/gal/images/i/versions/1.0.0".to_string(),
            os_disk_size_gb: 32,
            os_disk_sku: "StandardSSD_LRS".to_string(),
            os_disk_name: crate::vm::os_disk_name("uid"),
            nic_id: format!("{}{}", "/subscriptions/s", rg_path("Microsoft.Network/networkInterfaces/n")),
            computer_name: VM.to_string(),
            admin_username: DEFAULT_ADMIN_USERNAME.to_string(),
            ssh_public_key: "ssh-ed25519 AAAA banlieue".to_string(),
            custom_data: b"#cloud-config\n".to_vec(),
            trusted_launch: true,
            tags: tags(),
            disk_encryption_set_id: None,
            encryption_at_host: false,
        }
    }

    fn json_body(r: &Recorded) -> serde_json::Value {
        serde_json::from_slice(&r.body).expect("json body")
    }

    // ---- configuration -------------------------------------------------

    #[test]
    fn the_default_endpoint_is_public_azure() {
        install_provider();
        let c = ArmClient::new(ArmConfig::new(SUB), Arc::new(CountingToken::default())).unwrap();
        assert_eq!(c.endpoint(), DEFAULT_ARM_ENDPOINT);
    }

    #[test]
    fn a_plain_http_endpoint_is_refused_unless_loopback() {
        install_provider();
        let mut config = ArmConfig::new(SUB);
        config.endpoint = "http://management.azure.com".to_string();
        assert!(ArmClient::new(config, Arc::new(CountingToken::default())).is_err());
    }

    #[test]
    fn a_subscription_that_could_change_the_path_is_refused() {
        install_provider();
        for bad in ["", "a/b", "../x", "s?x=1", "s#f", "s%2f"] {
            assert!(
                ArmClient::new(ArmConfig::new(bad), Arc::new(CountingToken::default())).is_err(),
                "{bad:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_name_that_could_change_the_path_is_refused_before_any_request() {
        // No responses: any request would fail to connect, not be refused.
        let (endpoint, server) = serve(vec![]).await;
        let c = client(&endpoint);
        for bad in ["", "a/b", "..", "n?x", "n#x", "n%2f", "a b"] {
            let err = c.delete_nic(RG, bad).await.unwrap_err();
            assert!(
                matches!(err, Error::InvalidRequest { .. }),
                "{bad:?}: {err}"
            );
            let err = c.vm_instance_view(bad, VM).await.unwrap_err();
            assert!(
                matches!(err, Error::InvalidRequest { .. }),
                "{bad:?}: {err}"
            );
        }
        assert!(c.usages("east us").await.is_err());
        assert!(server.await.unwrap().is_empty());
    }

    // ---- creates -------------------------------------------------------

    #[tokio::test]
    async fn put_nic_sends_the_body_with_the_bearer_token_and_returns_the_operation() {
        let (endpoint, server) = serve_with(|base| {
            let op = format!("{base}/subscriptions/{SUB}/providers/Microsoft.Network/locations/eastus2/operations/op-1?api-version=2024-05-01");
            vec![respond(
                201,
                &[("Azure-AsyncOperation", &op), ("Retry-After", "10")],
                NIC_JSON,
            )]
        })
        .await;
        let handle = client(&endpoint)
            .put_nic(RG, NIC, &nic_request())
            .await
            .expect("put");
        let seen = server.await.unwrap();
        assert_eq!(
            seen[0].target(),
            format!(
                "PUT {}?api-version={NETWORK_API_VERSION}",
                rg_path(&format!("Microsoft.Network/networkInterfaces/{NIC}"))
            )
        );
        assert_eq!(
            seen[0].header("authorization").as_deref(),
            Some("Bearer arm-token")
        );
        assert_eq!(
            seen[0].header("content-type").as_deref(),
            Some("application/json")
        );
        assert_eq!(json_body(&seen[0]), nic_request().to_body().unwrap());
        let handle = handle.expect("an operation");
        assert_eq!(handle.kind, OperationKind::AsyncOperation);
        assert!(
            handle
                .url
                .ends_with("operations/op-1?api-version=2024-05-01")
        );
        assert_eq!(handle.retry_after, Some(Duration::from_secs(10)));
    }

    #[tokio::test]
    async fn put_vm_that_completes_synchronously_returns_no_operation() {
        let (endpoint, server) = serve(vec![ok("{}")]).await;
        let handle = client(&endpoint)
            .put_vm(RG, VM, &vm_request())
            .await
            .expect("put");
        assert!(handle.is_none());
        let seen = server.await.unwrap();
        assert_eq!(
            seen[0].target(),
            format!(
                "PUT {}?api-version={COMPUTE_API_VERSION}",
                rg_path(&format!("Microsoft.Compute/virtualMachines/{VM}"))
            )
        );
        assert_eq!(json_body(&seen[0]), vm_request().to_body().unwrap());
    }

    #[tokio::test]
    async fn an_operation_url_off_the_endpoint_is_refused() {
        let (endpoint, _server) = serve(vec![respond(
            201,
            &[(
                "Azure-AsyncOperation",
                "https://attacker.example.com/operations/1",
            )],
            "{}",
        )])
        .await;
        let err = client(&endpoint)
            .put_vm(RG, VM, &vm_request())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::UntrustedUrl(_)), "{err}");
    }

    #[tokio::test]
    async fn arm_refusals_come_back_typed() {
        // Captured: Standard_D2as_v6 in a zone restricted for the subscription.
        let sku = r#"{"error":{"code":"SkuNotAvailable","message":"The requested VM size Standard_D2as_v6 is currently not available in location 'eastus2' zones '2'."}}"#;
        let (endpoint, _s) = serve(vec![
            respond(409, &[], sku),
            respond(429, &[("Retry-After", "7")], "{}"),
            respond(
                400,
                &[],
                r#"{"error":{"code":"InvalidParameter","message":"x"}}"#,
            ),
        ])
        .await;
        let c = client(&endpoint);
        let err = c.put_vm(RG, VM, &vm_request()).await.unwrap_err();
        assert!(matches!(err, Error::SkuNotAvailable { .. }), "{err}");
        assert!(err.is_zone_specific());
        let err = c.put_vm(RG, VM, &vm_request()).await.unwrap_err();
        assert!(
            matches!(err, Error::Throttled { retry_after } if retry_after == Duration::from_secs(7)),
            "{err}"
        );
        let err = c.put_vm(RG, VM, &vm_request()).await.unwrap_err();
        assert_eq!(err.code(), Some("InvalidParameter"));
    }

    #[tokio::test]
    async fn a_refused_token_is_invalidated_so_the_next_call_exchanges_again() {
        let token = Arc::new(CountingToken::default());
        let (endpoint, _s) = serve(vec![respond(
            401,
            &[],
            r#"{"error":{"code":"ExpiredAuthenticationToken","message":"expired"}}"#,
        )])
        .await;
        let err = client_with(&endpoint, Arc::clone(&token))
            .vm_instance_view(RG, VM)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Unauthorized { .. }), "{err}");
        assert_eq!(token.invalidated.load(Ordering::Relaxed), 1);
    }

    // ---- deletes -------------------------------------------------------

    #[tokio::test]
    async fn deleting_what_is_gone_is_success() {
        let gone = r#"{"error":{"code":"ResourceNotFound","message":"not found"}}"#;
        let (endpoint, server) = serve(vec![respond(204, &[], ""), respond(404, &[], gone)]).await;
        let c = client(&endpoint);
        assert!(c.delete_vm(RG, VM).await.expect("204").is_none());
        assert!(c.delete_disk(RG, "d").await.expect("404").is_none());
        let seen = server.await.unwrap();
        assert_eq!(
            seen[0].target(),
            format!(
                "DELETE {}?api-version={COMPUTE_API_VERSION}",
                rg_path(&format!("Microsoft.Compute/virtualMachines/{VM}"))
            )
        );
        assert_eq!(
            seen[1].target(),
            format!(
                "DELETE {}?api-version={DISK_API_VERSION}",
                rg_path("Microsoft.Compute/disks/d")
            )
        );
    }

    #[tokio::test]
    async fn a_missing_resource_group_is_not_mistaken_for_a_deleted_resource() {
        let body = r#"{"error":{"code":"ResourceGroupNotFound","message":"Resource group 'rg' could not be found."}}"#;
        let (endpoint, _s) = serve(vec![respond(404, &[], body)]).await;
        let err = client(&endpoint).delete_nic(RG, NIC).await.unwrap_err();
        assert_eq!(err.code(), Some("ResourceGroupNotFound"));
    }

    #[tokio::test]
    async fn an_accepted_delete_returns_its_location_operation() {
        let (endpoint, _s) = serve_with(|base| {
            let location = format!("{base}/subscriptions/{SUB}/providers/Microsoft.Network/locations/eastus2/operationResults/op-2?api-version=2024-05-01");
            vec![respond(202, &[("Location", &location)], "")]
        })
        .await;
        let handle = client(&endpoint)
            .delete_nic(RG, NIC)
            .await
            .expect("delete")
            .expect("operation");
        assert_eq!(handle.kind, OperationKind::Location);
        assert!(handle.url.starts_with(&endpoint));
    }

    // ---- reads ---------------------------------------------------------

    #[tokio::test]
    async fn the_nic_address_and_instance_view_are_read_from_arm_s_answers() {
        let (endpoint, server) = serve(vec![ok(NIC_JSON), ok(RUNNING_JSON)]).await;
        let c = client(&endpoint);
        assert_eq!(
            c.nic_private_ip(RG, NIC).await.unwrap().as_deref(),
            Some("10.240.1.4")
        );
        assert!(c.vm_instance_view(RG, VM).await.unwrap().guest_ready());
        let seen = server.await.unwrap();
        assert_eq!(
            seen[1].target(),
            format!(
                "GET {}/instanceView?api-version={COMPUTE_API_VERSION}",
                rg_path(&format!("Microsoft.Compute/virtualMachines/{VM}"))
            )
        );
        assert_eq!(
            seen[1].header("authorization").as_deref(),
            Some("Bearer arm-token")
        );
    }

    #[tokio::test]
    async fn the_vm_tags_are_read_from_the_vm_itself() {
        let body = r#"{"name":"banlieue-uid","tags":{"banlieue-owner-uid":"uid"}}"#;
        let (endpoint, server) = serve(vec![ok(body)]).await;
        let tags = client(&endpoint)
            .vm_tags(RG, VM)
            .await
            .expect("read")
            .expect("exists");
        assert_eq!(
            tags.get("banlieue-owner-uid").map(String::as_str),
            Some("uid")
        );
        let seen = server.await.unwrap();
        assert_eq!(
            seen[0].target(),
            format!(
                "GET {}?api-version={COMPUTE_API_VERSION}",
                rg_path(&format!("Microsoft.Compute/virtualMachines/{VM}"))
            )
        );
    }

    #[tokio::test]
    async fn disk_tags_are_merged_through_the_tags_api() {
        // ADR-0173: the OS disk is tagged after the create, by name.
        let (endpoint, server) = serve(vec![ok(r#"{"properties":{"tags":{}}}"#)]).await;
        client(&endpoint)
            .merge_disk_tags(RG, "banlieue-uid-osdisk", &tags())
            .await
            .expect("merged");
        let seen = server.await.unwrap();
        assert_eq!(
            seen[0].target(),
            format!(
                "PATCH {}/providers/Microsoft.Resources/tags/default?api-version={RESOURCES_API_VERSION}",
                rg_path("Microsoft.Compute/disks/banlieue-uid-osdisk")
            )
        );
        let body = json_body(&seen[0]);
        assert_eq!(body["operation"], "Merge");
        assert_eq!(body["properties"]["tags"], serde_json::json!(tags()));
    }

    #[tokio::test]
    async fn tagging_a_missing_disk_is_not_found() {
        let body = r#"{"error":{"code":"ResourceNotFound","message":"x"}}"#;
        let (endpoint, _s) = serve(vec![respond(404, &[], body)]).await;
        let err = client(&endpoint)
            .merge_disk_tags(RG, "banlieue-uid-osdisk", &tags())
            .await
            .unwrap_err();
        assert!(err.is_not_found());
    }

    #[tokio::test]
    async fn a_refused_disk_tag_never_reaches_arm() {
        // No server: a call would fail as transport, not as a tag error.
        let mut bad = tags();
        bad.insert("a/b".to_string(), "x".to_string());
        let err = client("https://management.azure.com")
            .merge_disk_tags(RG, "banlieue-uid-osdisk", &bad)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidTag(_)), "{err:?}");
    }

    #[tokio::test]
    async fn the_tags_of_a_missing_vm_are_none() {
        let body = r#"{"error":{"code":"ResourceNotFound","message":"x"}}"#;
        let (endpoint, _s) = serve(vec![respond(404, &[], body)]).await;
        assert!(
            client(&endpoint)
                .vm_tags(RG, VM)
                .await
                .expect("read")
                .is_none()
        );
    }

    /// A missing group is a configuration error, never "the VM is gone".
    #[tokio::test]
    async fn a_missing_resource_group_is_not_a_missing_vm() {
        let body = r#"{"error":{"code":"ResourceGroupNotFound","message":"Resource group 'rg' could not be found."}}"#;
        let (endpoint, _s) = serve(vec![respond(404, &[], body)]).await;
        let err = client(&endpoint).vm_tags(RG, VM).await.unwrap_err();
        assert_eq!(err.code(), Some("ResourceGroupNotFound"));
    }

    #[tokio::test]
    async fn a_missing_vm_is_not_found() {
        let body = r#"{"error":{"code":"ResourceNotFound","message":"x"}}"#;
        let (endpoint, _s) = serve(vec![respond(404, &[], body)]).await;
        assert!(
            client(&endpoint)
                .vm_instance_view(RG, VM)
                .await
                .unwrap_err()
                .is_not_found()
        );
    }

    #[tokio::test]
    async fn power_actions_post_with_an_empty_body() {
        let (endpoint, server) = serve(vec![
            respond(202, &[], ""),
            respond(202, &[], ""),
            respond(202, &[], ""),
            respond(202, &[], ""),
        ])
        .await;
        let c = client(&endpoint);
        for action in [
            PowerAction::Start,
            PowerAction::Deallocate,
            PowerAction::PowerOff,
            PowerAction::Restart,
        ] {
            assert!(c.power(RG, VM, action).await.expect("power").is_none());
        }
        let seen = server.await.unwrap();
        let vm = rg_path(&format!("Microsoft.Compute/virtualMachines/{VM}"));
        for (r, verb) in seen
            .iter()
            .zip(["start", "deallocate", "powerOff", "restart"])
        {
            assert_eq!(
                r.target(),
                format!("POST {vm}/{verb}?api-version={COMPUTE_API_VERSION}")
            );
            // ARM answers 411 to a POST without a length.
            assert_eq!(r.header("content-length").as_deref(), Some("0"));
        }
    }

    #[tokio::test]
    async fn skus_are_filtered_to_the_location_and_follow_next_link() {
        let (endpoint, server) = serve_pages(SKUS_JSON).await;
        let skus = client(&endpoint)
            .resource_skus("eastus2")
            .await
            .expect("skus");
        let seen = server.await.unwrap();
        assert_eq!(seen.len(), 2, "both pages fetched");
        assert!(seen[0].target().starts_with(&format!(
            "GET /subscriptions/{SUB}/providers/Microsoft.Compute/skus?api-version={SKU_API_VERSION}"
        )));
        assert!(
            seen[0]
                .target()
                .contains("%24filter=location+eq+%27eastus2%27"),
            "{}",
            seen[0].target()
        );
        assert!(seen[1].target().contains("page=2"));
        assert!(skus.iter().any(|s| s.name == "Standard_D2s_v7"));
    }

    /// Page one: no resources and a `nextLink` to page two, which is `last`.
    async fn serve_pages(last: &str) -> (String, tokio::task::JoinHandle<Vec<Recorded>>) {
        serve_with(|base| {
            let next = format!(
                "{base}/subscriptions/{SUB}/providers/Microsoft.Compute/skus?api-version={SKU_API_VERSION}&page=2"
            );
            vec![
                ok(&format!(r#"{{"value":[],"nextLink":"{next}"}}"#)),
                ok(last),
            ]
        })
        .await
    }

    #[tokio::test]
    async fn a_next_link_off_the_endpoint_is_refused() {
        let (endpoint, _s) = serve(vec![ok(
            r#"{"value":[],"nextLink":"https://attacker.example.com/page2"}"#,
        )])
        .await;
        let err = client(&endpoint)
            .resource_skus("eastus2")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::UntrustedUrl(_)), "{err}");
    }

    #[tokio::test]
    async fn usages_and_resources_are_read_from_their_endpoints() {
        let (endpoint, server) = serve(vec![ok(USAGES_JSON), ok(RESOURCES_JSON)]).await;
        let c = client(&endpoint);
        let usages = c.usages("eastus2").await.expect("usages");
        assert!(crate::types::vcpu_headroom(&usages, "StandardDsv7Family").is_some());
        let resources = c.list_resources(RG).await.expect("resources");
        assert!(!resources.is_empty());
        let seen = server.await.unwrap();
        assert_eq!(
            seen[0].target(),
            format!(
                "GET /subscriptions/{SUB}/providers/Microsoft.Compute/locations/eastus2/usages?api-version={USAGE_API_VERSION}"
            )
        );
        assert_eq!(
            seen[1].target(),
            format!(
                "GET /subscriptions/{SUB}/resourceGroups/{RG}/resources?api-version={RESOURCES_API_VERSION}&%24expand=createdTime"
            )
        );
    }

    #[tokio::test]
    async fn a_response_over_the_size_cap_is_refused() {
        let big = format!(r#"{{"value":[],"pad":"{}"}}"#, "x".repeat(2048));
        let (endpoint, _s) = serve(vec![ok(&big)]).await;
        install_provider();
        let mut config = ArmConfig::new(SUB);
        config.endpoint = endpoint;
        config.max_response_bytes = 1024;
        let c = ArmClient::new(config, Arc::new(CountingToken::default())).unwrap();
        let err = c.usages("eastus2").await.unwrap_err();
        assert!(matches!(err, Error::Decode(_)), "{err}");
    }

    // ---- operations ----------------------------------------------------

    fn handle(endpoint: &str, kind: OperationKind) -> OperationHandle {
        OperationHandle {
            url: format!(
                "{endpoint}/subscriptions/{SUB}/providers/Microsoft.Compute/locations/eastus2/operations/op-1?api-version={COMPUTE_API_VERSION}"
            ),
            kind,
            retry_after: None,
        }
    }

    #[tokio::test]
    async fn an_async_operation_reports_its_status() {
        let (endpoint, server) = serve(vec![
            ok(r#"{"status":"InProgress"}"#),
            ok(r#"{"status":"Failed","error":{"code":"OSProvisioningTimedOut","message":"m"}}"#),
        ])
        .await;
        let c = client(&endpoint);
        let h = handle(&endpoint, OperationKind::AsyncOperation);
        assert_eq!(
            c.operation_status(&h).await.unwrap(),
            OperationStatus::InProgress
        );
        assert!(matches!(
            c.operation_status(&h).await.unwrap(),
            OperationStatus::Failed { ref code, .. } if code == "OSProvisioningTimedOut"
        ));
        let seen = server.await.unwrap();
        assert!(seen[0].target().starts_with("GET /subscriptions/"));
        assert_eq!(
            seen[0].header("authorization").as_deref(),
            Some("Bearer arm-token")
        );
    }

    #[tokio::test]
    async fn a_location_operation_is_running_until_it_answers_with_a_final_status() {
        let (endpoint, _s) = serve(vec![
            respond(202, &[], ""),
            respond(204, &[], ""),
            respond(
                400,
                &[],
                r#"{"error":{"code":"InvalidParameter","message":"m"}}"#,
            ),
            respond(503, &[], "{}"),
        ])
        .await;
        let c = client(&endpoint);
        let h = handle(&endpoint, OperationKind::Location);
        assert_eq!(
            c.operation_status(&h).await.unwrap(),
            OperationStatus::InProgress
        );
        assert_eq!(
            c.operation_status(&h).await.unwrap(),
            OperationStatus::Succeeded
        );
        assert!(matches!(
            c.operation_status(&h).await.unwrap(),
            OperationStatus::Failed { ref code, .. } if code == "InvalidParameter"
        ));
        // A 503 is ARM being unwell, not the operation failing.
        assert!(c.operation_status(&h).await.unwrap_err().is_retryable());
    }

    #[tokio::test]
    async fn a_stored_handle_that_was_tampered_with_is_never_polled() {
        // A handle comes back from a CR's status, which a user may edit: it
        // is checked again before the bearer token goes anywhere.
        let (endpoint, server) = serve(vec![]).await;
        let c = client(&endpoint);
        let mut h = handle(&endpoint, OperationKind::AsyncOperation);
        h.url = "https://attacker.example.com/op".to_string();
        let err = c.operation_status(&h).await.unwrap_err();
        assert!(matches!(err, Error::UntrustedUrl(_)), "{err}");
        assert!(server.await.unwrap().is_empty());
    }
}

/// The publish Job's calls (ADR-0162), against the local server.
#[cfg(test)]
mod publish_tests {
    use std::sync::Arc;
    use std::time::Instant;

    use async_trait::async_trait;

    use super::super::*;
    use crate::api::{ArmApi, DiskAccess, PublishApi};
    use crate::auth::TokenProvider;
    use crate::error::Result;
    use crate::gallery::ImageDefinitionRequest;
    use crate::tags::OwnershipTags;
    use crate::test_server::{install_provider, ok, respond, serve, serve_with};
    use crate::token::{AccessToken, parse_token_response};

    const SUB: &str = "00000000-0000-0000-0000-000000000003";
    const RG: &str = "rg-gallery";
    const GAL: &str = "gal";
    const DEF: &str = "kairos-tl";
    const VER: &str = "1.2.3";

    struct Token;

    #[async_trait]
    impl TokenProvider for Token {
        async fn token(&self) -> Result<AccessToken> {
            parse_token_response(
                br#"{"access_token":"arm-token","expires_in":3600}"#,
                Instant::now(),
            )
        }
    }

    fn client(endpoint: &str) -> ArmClient {
        install_provider();
        let mut config = ArmConfig::new(SUB);
        config.endpoint = endpoint.to_string();
        ArmClient::new(config, Arc::new(Token)).expect("client")
    }

    fn version_path() -> String {
        format!(
            "/subscriptions/{SUB}/resourceGroups/{RG}/providers/Microsoft.Compute/galleries/{GAL}/images/{DEF}/versions/{VER}?api-version={GALLERY_API_VERSION}"
        )
    }

    #[tokio::test]
    async fn get_version_reads_it_and_a_404_is_none() {
        let (base, server) = serve(vec![
            ok(r#"{"id":"/v","tags":{"banlieue-source-sha256":"ab"},"properties":{"provisioningState":"Succeeded"}}"#),
            respond(404, &[], r#"{"error":{"code":"ResourceNotFound","message":"x"}}"#),
        ])
        .await;
        let c = client(&base);
        let v = c
            .get_gallery_image_version(RG, GAL, DEF, VER)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(v.provisioning_state, "Succeeded");
        assert_eq!(v.tags["banlieue-source-sha256"], "ab");
        assert!(
            c.get_gallery_image_version(RG, GAL, DEF, VER)
                .await
                .unwrap()
                .is_none()
        );
        let seen = server.await.unwrap();
        assert_eq!(seen[0].target(), format!("GET {}", version_path()));
        assert_eq!(
            seen[0].header("authorization").as_deref(),
            Some("Bearer arm-token")
        );
    }

    #[tokio::test]
    async fn a_missing_resource_group_is_an_error_not_none() {
        let (base, server) = serve(vec![respond(
            404,
            &[],
            r#"{"error":{"code":"ResourceGroupNotFound","message":"x"}}"#,
        )])
        .await;
        assert!(client(&base).get_disk(RG, "d").await.is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn delete_version_of_nothing_is_success() {
        let (base, server) = serve(vec![respond(
            404,
            &[],
            r#"{"error":{"code":"ResourceNotFound","message":"x"}}"#,
        )])
        .await;
        let h = client(&base)
            .delete_gallery_image_version(RG, GAL, DEF, VER)
            .await
            .unwrap();
        assert_eq!(h, None);
        assert_eq!(
            server.await.unwrap()[0].target(),
            format!("DELETE {}", version_path())
        );
    }

    #[tokio::test]
    async fn an_invalid_version_name_is_refused_before_any_request() {
        let (base, server) = serve(vec![]).await;
        assert!(
            client(&base)
                .get_gallery_image_version(RG, GAL, DEF, "1.2")
                .await
                .is_err()
        );
        assert!(server.await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn put_definition_sends_the_features() {
        let (base, server) = serve(vec![respond(201, &[], "{}")]).await;
        let request = ImageDefinitionRequest {
            location: "eastus2".into(),
            offer: "kairos".into(),
            trusted_launch: true,
            tags: OwnershipTags::new("uid", "VMImage", "ns", "p").to_map(),
        };
        client(&base)
            .put_gallery_image_definition(RG, GAL, DEF, &request)
            .await
            .unwrap();
        let seen = server.await.unwrap();
        assert!(seen[0].target().starts_with("PUT /subscriptions/"));
        assert!(seen[0].body_str().contains("DiskControllerTypes"));
        assert!(seen[0].body_str().contains("TrustedLaunch"));
    }

    #[tokio::test]
    async fn grant_access_synchronously_returns_the_sas() {
        let (base, server) = serve(vec![ok(
            r#"{"accessSAS":"https://md-x.z1.blob.storage.azure.net/a?sig=S"}"#,
        )])
        .await;
        let access = client(&base)
            .grant_disk_write_access(RG, "d", 3600)
            .await
            .unwrap();
        let DiskAccess::Granted(sas) = access else {
            panic!("granted");
        };
        assert_eq!(
            sas.host().as_deref(),
            Some("md-x.z1.blob.storage.azure.net")
        );
        let seen = server.await.unwrap();
        assert!(seen[0].target().contains("/disks/d/beginGetAccess?"));
        assert!(seen[0].body_str().contains(r#""access":"Write""#));
        assert!(seen[0].body_str().contains(r#""durationInSeconds":3600"#));
    }

    #[tokio::test]
    async fn grant_access_as_an_operation_is_polled_for_the_sas() {
        let (base, server) = serve_with(|base| {
            let op = format!("{base}/op/1");
            vec![
                respond(202, &[("Location", op.as_str())], ""),
                respond(202, &[], ""),
                ok(r#"{"accessSAS":"https://md-x.z1.blob.storage.azure.net/a?sig=S"}"#),
            ]
        })
        .await;
        let c = client(&base);
        let DiskAccess::Pending(handle) = c.grant_disk_write_access(RG, "d", 3600).await.unwrap()
        else {
            panic!("pending");
        };
        assert!(c.disk_access_result(&handle).await.unwrap().is_none());
        assert!(c.disk_access_result(&handle).await.unwrap().is_some());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn an_async_operation_status_carries_the_sas_in_its_output() {
        let (base, server) = serve_with(|base| {
            let op = format!("{base}/op/2");
            vec![
                respond(202, &[("Azure-AsyncOperation", op.as_str())], ""),
                ok(r#"{"status":"InProgress"}"#),
                ok(r#"{"status":"Succeeded","properties":{"output":{"accessSAS":"https://md-x.z1.blob.storage.azure.net/a?sig=S"}}}"#),
            ]
        })
        .await;
        let c = client(&base);
        let DiskAccess::Pending(handle) = c.grant_disk_write_access(RG, "d", 3600).await.unwrap()
        else {
            panic!("pending");
        };
        assert!(c.disk_access_result(&handle).await.unwrap().is_none());
        assert!(c.disk_access_result(&handle).await.unwrap().is_some());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn revoke_and_snapshot_and_list_use_the_documented_paths() {
        let (base, server) = serve(vec![
            respond(200, &[], ""),
            ok(r#"{"value":[{"id":"/v/1","properties":{"provisioningState":"Succeeded"}}]}"#),
        ])
        .await;
        let c = client(&base);
        c.revoke_disk_access(RG, "d").await.unwrap();
        assert_eq!(
            c.list_gallery_image_versions(RG, GAL, DEF)
                .await
                .unwrap()
                .len(),
            1
        );
        let seen = server.await.unwrap();
        assert!(seen[0].target().starts_with("POST "));
        assert!(seen[0].target().contains("/disks/d/endGetAccess?"));
        assert!(
            seen[1]
                .target()
                .contains(&format!("/images/{DEF}/versions?"))
        );
    }
}
