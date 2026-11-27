// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `client.rs`: the `Provider`-to-client mapping, offline.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use banlieue_api::common::LocalObjectReference;
    use banlieue_aws::FakeEc2;
    use banlieue_aws::sts::SESSION_DURATION;

    use super::super::*;
    use crate::reconciler::provider::{failure_reason, reasons};
    use crate::test_support::{self, ENDPOINT, HOME, PROVIDER, REGION, ROLE_ARN};

    const TOKEN_FILE: &str = "/var/run/secrets/aws/tokens/aws-identity-token";
    const STS_VPCE: &str =
        "https://vpce-0123456789abcdef0-abcd1234.sts.ca-central-1.vpce.amazonaws.com";

    fn provider() -> Provider {
        test_support::provider(Default::default())
    }

    fn config(p: &Provider) -> Result<WebIdentityConfig> {
        web_identity_config(p, PathBuf::from(TOKEN_FILE))
    }

    #[test]
    fn the_default_token_path_is_where_the_operator_mounts_it() {
        assert_eq!(DEFAULT_WEB_IDENTITY_TOKEN_FILE, TOKEN_FILE);
    }

    /// ADR-0174 Decision 6: the role, the region's STS, a fifteen-minute
    /// session named after the `Provider`.
    #[test]
    fn a_provider_maps_to_its_web_identity_exchange() {
        let c = config(&provider()).expect("config");
        assert_eq!(c.region, REGION);
        assert_eq!(c.role_arn, ROLE_ARN);
        assert_eq!(c.role_session_name, format!("{HOME}.{PROVIDER}"));
        assert_eq!(c.token_file, Path::new(TOKEN_FILE));
        assert_eq!(c.session_duration, SESSION_DURATION);
        assert_eq!(SESSION_DURATION.as_secs(), 900);
        assert_eq!(
            c.sts_endpoint.as_deref(),
            Some("https://sts.ca-central-1.amazonaws.com")
        );
    }

    #[test]
    fn a_sts_vpc_endpoint_replaces_the_regional_one() {
        let mut p = provider();
        let mut ec2 = test_support::ec2();
        ec2.sts_endpoint = Some(STS_VPCE.into());
        p.spec.connection.ec2 = Some(ec2);
        assert_eq!(
            config(&p).expect("config").sts_endpoint.as_deref(),
            Some(STS_VPCE)
        );
    }

    /// The hosts that receive tokens are never taken from the `Provider` on
    /// trust, and each refusal is a `ConnectionInvalid` condition.
    #[test]
    fn a_token_destination_outside_the_region_is_refused() {
        let mut elsewhere = provider();
        elsewhere.spec.connection.endpoint = "https://ec2.example.com".into();
        let mut other_sts = provider();
        let mut ec2 = test_support::ec2();
        ec2.sts_endpoint = Some("https://sts.us-east-1.amazonaws.com".into());
        other_sts.spec.connection.ec2 = Some(ec2);
        let mut other_region = provider();
        other_region.spec.connection.endpoint = "https://ec2.us-east-1.amazonaws.com".into();
        for p in [elsewhere, other_sts, other_region] {
            let err = config(&p).expect_err("refused");
            assert!(matches!(err, Error::Invalid { .. }), "{err}");
            assert_eq!(failure_reason(&err), reasons::CONNECTION_INVALID);
        }
    }

    #[test]
    fn a_missing_ec2_block_or_a_credentials_ref_is_refused() {
        let mut bare = provider();
        bare.spec.connection.ec2 = None;
        let mut with_secret = provider();
        with_secret.spec.connection.credentials_ref = Some(LocalObjectReference {
            name: "aws-keys".into(),
        });
        for p in [bare, with_secret] {
            let err = config(&p).expect_err("refused");
            assert_eq!(failure_reason(&err), reasons::CONNECTION_INVALID);
        }
    }

    #[test]
    fn invalid_extra_tags_are_refused() {
        let mut p = provider();
        let mut ec2 = test_support::ec2();
        ec2.tags.insert("banlieue-owner-uid".into(), "x".into());
        p.spec.connection.ec2 = Some(ec2);
        assert!(matches!(config(&p), Err(Error::Invalid { .. })));
    }

    /// The real factory builds a client without a request, and hands the
    /// same one (and so the same cached session) back for the same
    /// connection.
    #[tokio::test]
    async fn the_http_factory_caches_one_client_per_connection() {
        install_default_crypto_provider();
        let factory = HttpClientFactory::new(PathBuf::from(TOKEN_FILE));
        let a = factory.build(&provider()).await.expect("client");
        let b = factory.build(&provider()).await.expect("client");
        assert!(Arc::ptr_eq(&a, &b));
        let mut bad = provider();
        bad.spec.connection.endpoint = "https://ec2.example.com".into();
        assert!(factory.build(&bad).await.is_err());
    }

    #[tokio::test]
    async fn the_static_factory_still_checks_the_connection() {
        let factory = StaticClientFactory::new(Arc::new(FakeEc2::ca_central_1()));
        factory.build(&provider()).await.expect("valid");
        let mut bad = provider();
        bad.spec.connection.endpoint = ENDPOINT.replace("ca-central-1", "eu-west-1");
        assert!(factory.build(&bad).await.is_err());
    }
}
