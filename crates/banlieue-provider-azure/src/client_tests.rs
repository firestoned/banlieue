// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `client.rs`: the connection-to-client mapping, offline.

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use banlieue_azure::fake::FakeArm;

    use banlieue_azure::client::DEFAULT_ARM_ENDPOINT;
    use banlieue_azure::token::{ARM_SCOPE, DEFAULT_AUTHORITY};

    use super::super::*;
    use crate::cloud::{
        AZURE_CLOUDS, CHINA_ARM_ENDPOINT, CHINA_AUTHORITY_HOST, PUBLIC_ARM_ENDPOINT,
        PUBLIC_AUTHORITY_HOST, US_GOV_ARM_ENDPOINT, US_GOV_AUTHORITY_HOST,
    };
    use crate::reconciler::provider::{failure_reason, reasons};
    use crate::test_support::{self, CLIENT, ENDPOINT, LOCATION, SUBSCRIPTION, TENANT};

    const TOKEN_FILE: &str = "/var/run/secrets/azure/tokens/azure-identity-token";

    #[test]
    fn the_default_token_path_is_where_the_operator_mounts_it() {
        assert_eq!(DEFAULT_FEDERATED_TOKEN_FILE, TOKEN_FILE);
    }

    #[test]
    fn a_connection_maps_to_the_exchange_and_arm_settings() {
        let (fed, arm) = client_configs(
            &test_support::connection(Some(test_support::azure())),
            Path::new(TOKEN_FILE),
        )
        .expect("configs");
        assert_eq!(fed.tenant_id, TENANT);
        assert_eq!(fed.client_id, CLIENT);
        assert_eq!(fed.authority, DEFAULT_AUTHORITY);
        assert_eq!(fed.scope, ARM_SCOPE);
        assert_eq!(fed.token_file, Path::new(TOKEN_FILE));
        assert_eq!(arm.subscription_id, SUBSCRIPTION);
        assert_eq!(arm.endpoint, ENDPOINT);
    }

    /// A sovereign cloud: authority, endpoint and scope are that cloud's,
    /// trailing slashes dropped.
    #[test]
    fn a_sovereign_cloud_gets_its_own_authority_and_scope() {
        let mut azure = test_support::azure();
        azure.authority_host = Some(format!("{US_GOV_AUTHORITY_HOST}/"));
        let mut conn = test_support::connection(Some(azure));
        conn.endpoint = format!("{US_GOV_ARM_ENDPOINT}/");
        let (fed, arm) = client_configs(&conn, Path::new(TOKEN_FILE)).expect("configs");
        assert_eq!(fed.authority, US_GOV_AUTHORITY_HOST);
        assert_eq!(fed.scope, format!("{US_GOV_ARM_ENDPOINT}/.default"));
        assert_eq!(arm.endpoint, US_GOV_ARM_ENDPOINT);
    }

    /// No authority declared: the endpoint's own cloud's login host, never
    /// public Azure's for a sovereign endpoint.
    #[test]
    fn an_unset_authority_follows_the_endpoint() {
        let mut conn = test_support::connection(Some(test_support::azure()));
        conn.endpoint = CHINA_ARM_ENDPOINT.into();
        let (fed, _) = client_configs(&conn, Path::new(TOKEN_FILE)).expect("configs");
        assert_eq!(fed.authority, CHINA_AUTHORITY_HOST);
    }

    /// The public pair the cloud table names is the one the client crate
    /// defaults to.
    #[test]
    fn the_public_cloud_matches_the_client_defaults() {
        assert_eq!(PUBLIC_ARM_ENDPOINT, DEFAULT_ARM_ENDPOINT);
        assert_eq!(PUBLIC_AUTHORITY_HOST, DEFAULT_AUTHORITY);
        assert_eq!(AZURE_CLOUDS[0].arm_scope(), ARM_SCOPE);
    }

    /// The hosts that receive tokens are never taken from the Provider on
    /// trust, and the refusal is a `ConnectionInvalid` condition.
    #[test]
    fn an_unknown_endpoint_or_authority_is_connection_invalid() {
        let mut conn = test_support::connection(Some(test_support::azure()));
        conn.endpoint = "https://management.example.com".into();
        let err = client_configs(&conn, Path::new(TOKEN_FILE)).expect_err("unknown endpoint");
        assert_eq!(failure_reason(&err), reasons::CONNECTION_INVALID);

        let mut azure = test_support::azure();
        azure.authority_host = Some("https://login.example.com".into());
        let err = client_configs(
            &test_support::connection(Some(azure)),
            Path::new(TOKEN_FILE),
        )
        .expect_err("unknown authority");
        assert_eq!(failure_reason(&err), reasons::CONNECTION_INVALID);
    }

    #[test]
    fn an_empty_endpoint_is_public_azure() {
        let mut conn = test_support::connection(Some(test_support::azure()));
        conn.endpoint = String::new();
        let (_, arm) = client_configs(&conn, Path::new(TOKEN_FILE)).expect("configs");
        assert_eq!(arm.endpoint, DEFAULT_ARM_ENDPOINT);
    }

    #[test]
    fn a_connection_without_an_azure_block_is_refused() {
        let err = client_configs(&test_support::connection(None), Path::new(TOKEN_FILE))
            .expect_err("no azure block");
        assert!(matches!(err, Error::Invalid { .. }), "{err}");
    }

    #[test]
    fn the_checked_block_is_returned_only_for_a_known_cloud() {
        let conn = test_support::connection(Some(test_support::azure()));
        assert_eq!(
            checked_azure_connection(&conn).expect("known").tenant_id,
            TENANT
        );
        let mut bad = conn.clone();
        bad.endpoint = "https://management.azure.com:8443".into();
        assert!(checked_azure_connection(&bad).is_err());
        assert!(checked_azure_connection(&test_support::connection(None)).is_err());
    }

    /// One client per connection, so the cached Entra ID token survives
    /// across reconciles.
    #[tokio::test]
    async fn the_http_factory_reuses_a_client_per_connection() {
        install_default_crypto_provider();
        let factory = HttpClientFactory::new(TOKEN_FILE.into());
        let conn = test_support::connection(Some(test_support::azure()));
        let a = factory.build(&conn).await.expect("client");
        let b = factory.build(&conn).await.expect("client");
        assert!(Arc::ptr_eq(&a, &b));

        let mut other = test_support::azure();
        other.location = format!("{LOCATION}x");
        other.subscription_id = "00000000-0000-0000-0000-000000000009".into();
        let c = factory
            .build(&test_support::connection(Some(other)))
            .await
            .expect("client");
        assert!(!Arc::ptr_eq(&a, &c));
    }

    /// A plain-HTTP endpoint would send the bearer token in clear.
    #[tokio::test]
    async fn the_http_factory_refuses_a_cleartext_endpoint() {
        install_default_crypto_provider();
        let factory = HttpClientFactory::new(TOKEN_FILE.into());
        let mut conn = test_support::connection(Some(test_support::azure()));
        conn.endpoint = "http://management.example.com".into();
        assert!(factory.build(&conn).await.is_err());
    }

    #[tokio::test]
    async fn the_static_factory_still_requires_the_azure_block() {
        let api: Arc<dyn ArmApi> = Arc::new(FakeArm::from_fixtures(LOCATION).expect("fake"));
        let factory = StaticClientFactory::new(api);
        assert!(
            factory
                .build(&test_support::connection(None))
                .await
                .is_err()
        );
        assert!(
            factory
                .build(&test_support::connection(Some(test_support::azure())))
                .await
                .is_ok()
        );
        let mut foreign = test_support::connection(Some(test_support::azure()));
        foreign.endpoint = "https://management.azure.com.evil.example".into();
        assert!(factory.build(&foreign).await.is_err());
    }

    // ----- ADR-0172: governance settings are checked before any client -----

    #[test]
    fn a_governed_connection_is_accepted() {
        let conn = test_support::connection(Some(test_support::governed_azure()));
        assert!(checked_azure_connection(&conn).is_ok());
        assert!(check_governance(&test_support::governed_azure()).is_ok());
    }

    /// An extra tag in banlieue's own namespace, in any case, could forge an
    /// ownership tag on ARM's side: `ConnectionInvalid`, no client.
    #[test]
    fn a_tag_that_would_override_an_ownership_tag_is_connection_invalid() {
        let mut azure = test_support::governed_azure();
        azure
            .tags
            .insert("Banlieue-Owner-Uid".into(), "someone-else".into());
        let err =
            checked_azure_connection(&test_support::connection(Some(azure))).expect_err("refused");
        assert_eq!(failure_reason(&err), reasons::CONNECTION_INVALID);
        assert!(err.to_string().contains("tags"), "{err}");
    }

    #[test]
    fn an_invalid_tag_name_is_connection_invalid() {
        let mut azure = test_support::governed_azure();
        azure.tags.insert("cost/centre".into(), "1".into());
        assert!(check_governance(&azure).is_err());
    }

    #[test]
    fn a_disk_encryption_set_in_another_subscription_is_connection_invalid() {
        let mut azure = test_support::governed_azure();
        azure.disk_encryption_set_id = Some(
            "/subscriptions/00000000-0000-0000-0000-0000000000ff/resourceGroups/rg-sec/providers/Microsoft.Compute/diskEncryptionSets/des-1".into(),
        );
        let err = check_governance(&azure).expect_err("refused");
        assert_eq!(failure_reason(&err), reasons::CONNECTION_INVALID);
        assert!(err.to_string().contains("diskEncryptionSetId"), "{err}");
    }

    #[test]
    fn a_malformed_disk_encryption_set_is_connection_invalid() {
        let mut azure = test_support::governed_azure();
        azure.disk_encryption_set_id = Some("des-1".into());
        assert!(check_governance(&azure).is_err());
    }

    #[tokio::test]
    async fn both_factories_refuse_an_invalid_governance_setting() {
        install_default_crypto_provider();
        let mut azure = test_support::governed_azure();
        azure.disk_encryption_set_id = Some("des-1".into());
        let conn = test_support::connection(Some(azure));
        let http = HttpClientFactory::new(TOKEN_FILE.into());
        assert!(http.build(&conn).await.is_err());
        let api: Arc<dyn ArmApi> = Arc::new(FakeArm::from_fixtures(LOCATION).expect("fake"));
        assert!(StaticClientFactory::new(api).build(&conn).await.is_err());
    }
}
