// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `auth.rs`: the refresh decision, and the web identity
//! source against a local stand-in for STS.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use jiff::{SignedDuration, Timestamp};

    use super::super::*;
    use crate::error::Error;
    use crate::sigv4::uri_encode;
    use crate::test_server::{install_provider, ok, respond, serve};

    const REGION: &str = "us-east-1";
    const ROLE_ARN: &str = "arn:aws:iam::111122223333:role/banlieue-ec2";
    const SESSION: &str = "banlieue-system.ec2";
    const SA_TOKEN_A: &str = "eyJ.sa-token-a.sig";
    const SA_TOKEN_B: &str = "eyJ.sa-token-b.sig";
    const SECRET: &str = "secret-EXAMPLEKEY";
    const SESSION_TOKEN: &str = "session-token-EXAMPLE";
    const STS_ERROR: &str = include_str!("../tests/fixtures/ec2/error-sts.xml");

    /// A fresh token file. `tempfile` creates it exclusively under a random
    /// name, so no other process can pre-create or guess the path; `keep`
    /// stops the delete-on-drop so the source can read it for the whole test.
    fn token_file(contents: &str) -> PathBuf {
        let mut file = tempfile::Builder::new()
            .prefix("banlieue-aws-sa-")
            .tempfile()
            .unwrap();
        std::io::Write::write_all(&mut file, contents.as_bytes()).unwrap();
        let (_, path) = file.keep().unwrap();
        path
    }

    fn granted(access_key: &str, expiration: Timestamp) -> String {
        ok(&format!(
            r#"<AssumeRoleWithWebIdentityResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <AssumeRoleWithWebIdentityResult>
    <Credentials>
      <SessionToken>{SESSION_TOKEN}</SessionToken>
      <SecretAccessKey>{SECRET}</SecretAccessKey>
      <Expiration>{expiration}</Expiration>
      <AccessKeyId>{access_key}</AccessKeyId>
    </Credentials>
  </AssumeRoleWithWebIdentityResult>
</AssumeRoleWithWebIdentityResponse>"#
        ))
    }

    fn in_minutes(minutes: i64) -> Timestamp {
        Timestamp::now() + SignedDuration::from_mins(minutes)
    }

    fn config(endpoint: &str, file: PathBuf) -> WebIdentityConfig {
        let mut config = WebIdentityConfig::new(REGION, ROLE_ARN, SESSION, file);
        config.sts_endpoint = Some(endpoint.to_string());
        config
    }

    fn source(endpoint: &str, file: PathBuf) -> WebIdentitySource {
        install_provider();
        WebIdentitySource::new(config(endpoint, file)).expect("source")
    }

    fn form_value(body: &str, key: &str) -> Option<String> {
        body.split('&')
            .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
            .map(ToString::to_string)
    }

    // ----- needs_refresh ---------------------------------------------------

    #[test]
    fn a_session_is_refreshed_at_ten_minutes_of_fifteen() {
        let issued = Timestamp::UNIX_EPOCH;
        let expiration = issued + SignedDuration::from_mins(15);
        let ten = issued + SignedDuration::from_mins(10);
        assert!(needs_refresh(expiration, ten), "at exactly ten minutes");
        assert!(
            !needs_refresh(expiration, ten - SignedDuration::from_secs(1)),
            "one second before"
        );
    }

    #[test]
    fn a_session_with_more_than_the_margin_left_is_kept() {
        let now = Timestamp::UNIX_EPOCH;
        let margin = SignedDuration::from_secs(300);
        assert!(!needs_refresh(
            now + margin + SignedDuration::from_secs(1),
            now
        ));
        assert!(needs_refresh(now + margin, now));
        assert!(needs_refresh(now, now));
        assert!(
            needs_refresh(now - SignedDuration::from_mins(1), now),
            "past"
        );
        assert_eq!(REFRESH_BEFORE_EXPIRY.as_secs(), 300);
    }

    // ----- the exchange ----------------------------------------------------

    #[tokio::test]
    async fn the_token_is_exchanged_by_an_unsigned_form_post() {
        let (endpoint, server) = serve(vec![granted("ASIAEXAMPLE1", in_minutes(15))]).await;
        let src = source(&endpoint, token_file(&format!("{SA_TOKEN_A}\n")));

        let creds = src.credentials().await.expect("credentials");
        assert_eq!(creds.access_key_id, "ASIAEXAMPLE1");
        assert_eq!(creds.secret_access_key, SECRET);
        assert_eq!(creds.session_token.as_deref(), Some(SESSION_TOKEN));

        let seen = server.await.unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].target(), "POST /");
        assert!(
            seen[0].header("authorization").is_none(),
            "the STS call is unsigned"
        );
        assert!(seen[0].header("x-amz-security-token").is_none());
        assert_eq!(
            seen[0].header("content-type").as_deref(),
            Some(crate::query::FORM_CONTENT_TYPE)
        );
        let body = seen[0].body_str();
        assert_eq!(
            form_value(&body, "Action").as_deref(),
            Some("AssumeRoleWithWebIdentity")
        );
        assert_eq!(form_value(&body, "Version").as_deref(), Some("2011-06-15"));
        assert_eq!(form_value(&body, "DurationSeconds").as_deref(), Some("900"));
        assert_eq!(
            form_value(&body, "RoleArn"),
            Some(uri_encode(ROLE_ARN, true))
        );
        assert_eq!(
            form_value(&body, "RoleSessionName").as_deref(),
            Some(SESSION)
        );
        assert_eq!(
            form_value(&body, "WebIdentityToken").as_deref(),
            Some(SA_TOKEN_A),
            "trailing newline trimmed"
        );
    }

    #[tokio::test]
    async fn a_fresh_session_is_reused_without_another_exchange() {
        // One response only: a second exchange would find the listener gone.
        let (endpoint, server) = serve(vec![granted("ASIAEXAMPLE1", in_minutes(15))]).await;
        let src = source(&endpoint, token_file(SA_TOKEN_A));
        let first = src.credentials().await.expect("first");
        let second = src.credentials().await.expect("cached");
        assert_eq!(first.access_key_id, second.access_key_id);
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_expiring_cached_session_triggers_a_new_exchange() {
        let (endpoint, server) = serve(vec![
            // Four minutes left: inside the five-minute margin.
            granted("ASIAEXAMPLE1", in_minutes(4)),
            granted("ASIAEXAMPLE2", in_minutes(15)),
        ])
        .await;
        let src = source(&endpoint, token_file(SA_TOKEN_A));
        assert_eq!(
            src.credentials().await.unwrap().access_key_id,
            "ASIAEXAMPLE1"
        );
        assert_eq!(
            src.credentials().await.unwrap().access_key_id,
            "ASIAEXAMPLE2"
        );
        assert_eq!(server.await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_rotated_token_file_is_read_again_on_the_next_exchange() {
        let (endpoint, server) = serve(vec![
            granted("ASIAEXAMPLE1", in_minutes(-1)),
            granted("ASIAEXAMPLE2", in_minutes(15)),
        ])
        .await;
        let file = token_file(SA_TOKEN_A);
        let src = source(&endpoint, file.clone());
        src.credentials().await.expect("first");
        // The kubelet rotates the projected token.
        std::fs::write(&file, SA_TOKEN_B).unwrap();
        src.credentials().await.expect("second");

        let seen = server.await.unwrap();
        assert_eq!(
            form_value(&seen[0].body_str(), "WebIdentityToken").as_deref(),
            Some(SA_TOKEN_A)
        );
        assert_eq!(
            form_value(&seen[1].body_str(), "WebIdentityToken").as_deref(),
            Some(SA_TOKEN_B)
        );
    }

    #[tokio::test]
    async fn an_empty_token_file_is_refused_before_any_request() {
        // No responses: any request would fail to connect, as Transport.
        let (endpoint, server) = serve(vec![]).await;
        let src = source(&endpoint, token_file(" \n"));
        let err = src.credentials().await.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
        assert!(err.to_string().contains("empty"));
        assert!(server.await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_missing_token_file_is_named_without_contents() {
        let src = source(
            "http://127.0.0.1:9/",
            PathBuf::from("/nonexistent/banlieue-sa"),
        );
        let err = src.credentials().await.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
        assert!(err.to_string().contains("/nonexistent/banlieue-sa"));
    }

    #[tokio::test]
    async fn a_refused_exchange_is_typed_and_does_not_quote_the_token() {
        let (endpoint, _server) = serve(vec![respond(400, &[], STS_ERROR)]).await;
        let src = source(&endpoint, token_file(SA_TOKEN_A));
        let err = src.credentials().await.unwrap_err();
        assert!(matches!(err, Error::Unauthenticated(_)), "{err}");
        assert_eq!(err.fault().unwrap().code, "InvalidIdentityToken");
        assert!(!err.to_string().contains(SA_TOKEN_A));
    }

    // ----- configuration ---------------------------------------------------

    #[test]
    fn a_session_shorter_than_stss_floor_is_refused() {
        install_provider();
        let mut config = config("http://127.0.0.1:9/", token_file(SA_TOKEN_A));
        config.session_duration = crate::sts::SESSION_DURATION - std::time::Duration::from_secs(1);
        let err = WebIdentitySource::new(config).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
    }

    #[test]
    fn the_default_session_is_the_floor_and_the_endpoint_is_regional() {
        install_provider();
        let config = WebIdentityConfig::new(REGION, ROLE_ARN, SESSION, "/t");
        assert_eq!(config.session_duration, crate::sts::SESSION_DURATION);
        let src = WebIdentitySource::new(config).expect("source");
        assert_eq!(src.endpoint(), "https://sts.us-east-1.amazonaws.com/");
    }

    #[test]
    fn a_plain_http_sts_endpoint_off_loopback_is_refused() {
        install_provider();
        let err = WebIdentitySource::new(config("http://sts.example.com/", PathBuf::from("/t")))
            .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
    }

    #[test]
    fn a_bad_session_name_or_region_is_refused() {
        install_provider();
        let mut bad_name = WebIdentityConfig::new(REGION, ROLE_ARN, "ns/name", "/t");
        assert!(WebIdentitySource::new(bad_name.clone()).is_err());
        bad_name.role_session_name = SESSION.to_string();
        bad_name.region = "not a region".to_string();
        assert!(WebIdentitySource::new(bad_name).is_err());
    }

    #[test]
    fn web_identity_configuration_comes_from_the_environment() {
        let env = |k: &str| match k {
            ENV_REGION => Some(REGION.to_string()),
            ENV_ROLE_ARN => Some(ROLE_ARN.to_string()),
            ENV_WEB_IDENTITY_TOKEN_FILE => Some("/var/run/secrets/token".to_string()),
            _ => None,
        };
        let config = WebIdentityConfig::from_lookup(env, "default-session").expect("config");
        assert_eq!(config.role_session_name, "default-session");
        assert_eq!(config.token_file, PathBuf::from("/var/run/secrets/token"));

        let missing = WebIdentityConfig::from_lookup(|_| None, "s").unwrap_err();
        assert!(missing.to_string().contains(ENV_REGION), "{missing}");
    }

    #[test]
    fn debug_renderings_redact_credentials_and_the_role() {
        install_provider();
        let src = source("http://127.0.0.1:9/", token_file(SA_TOKEN_A));
        let shown = format!("{src:?}");
        assert!(!shown.contains(SA_TOKEN_A));
        assert!(!shown.contains("111122223333"), "{shown}");

        let fixed = StaticSource::new(crate::sigv4::Credentials::new(
            "AKIDEXAMPLE",
            SECRET,
            Some(SESSION_TOKEN.to_string()),
        ));
        let shown = format!("{fixed:?}");
        assert!(!shown.contains(SECRET), "{shown}");
        assert!(!shown.contains(SESSION_TOKEN));
    }

    #[tokio::test]
    async fn a_static_source_returns_its_credentials() {
        let env = |k: &str| match k {
            ENV_ACCESS_KEY_ID => Some("AKIDEXAMPLE".to_string()),
            ENV_SECRET_ACCESS_KEY => Some(SECRET.to_string()),
            _ => None,
        };
        let creds = StaticSource::from_lookup(env)
            .unwrap()
            .credentials()
            .await
            .unwrap();
        assert_eq!(creds.access_key_id, "AKIDEXAMPLE");
        assert_eq!(creds.session_token, None);
        let err = StaticSource::from_lookup(|_| None).unwrap_err();
        assert!(err.to_string().contains(ENV_ACCESS_KEY_ID));
    }
}
