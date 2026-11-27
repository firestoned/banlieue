// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `auth.rs`: the federated token source against a local
//! stand-in for Entra ID's token endpoint.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::super::*;
    use crate::error::Error;
    use crate::test_server::{install_provider, ok, respond, serve};

    const TENANT: &str = "00000000-0000-0000-0000-000000000001";
    const CLIENT: &str = "00000000-0000-0000-0000-000000000002";
    const SA_TOKEN_A: &str = "eyJ.sa-token-a.sig";
    const SA_TOKEN_B: &str = "eyJ.sa-token-b.sig";

    static NEXT_FILE: AtomicU32 = AtomicU32::new(0);

    fn token_file(contents: &str) -> PathBuf {
        let n = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("banlieue-azure-sa-{}-{n}", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn granted(token: &str, expires_in: u64) -> String {
        ok(&format!(
            r#"{{"token_type":"Bearer","expires_in":{expires_in},"access_token":"{token}"}}"#
        ))
    }

    fn source(authority: &str, file: PathBuf) -> FederatedToken {
        install_provider();
        FederatedToken::new(FederatedConfig {
            authority: authority.to_string(),
            tenant_id: TENANT.to_string(),
            client_id: CLIENT.to_string(),
            token_file: file,
            scope: crate::token::ARM_SCOPE.to_string(),
        })
        .expect("source")
    }

    #[tokio::test]
    async fn the_service_account_token_is_exchanged_at_the_tenant_endpoint() {
        let (authority, server) = serve(vec![granted("arm-1", 3600)]).await;
        let src = source(&authority, token_file(&format!("{SA_TOKEN_A}\n")));

        let token = src.token().await.expect("token");
        assert_eq!(token.secret(), "arm-1");

        let seen = server.await.unwrap();
        assert_eq!(
            seen[0].target(),
            format!("POST /{TENANT}/oauth2/v2.0/token")
        );
        assert_eq!(
            seen[0].header("content-type").as_deref(),
            Some("application/x-www-form-urlencoded")
        );
        let form: HashMap<String, String> =
            form_urlencoded::parse(&seen[0].body).into_owned().collect();
        assert_eq!(form["client_id"], CLIENT);
        assert_eq!(
            form["client_assertion"], SA_TOKEN_A,
            "trailing newline trimmed"
        );
        assert_eq!(form["scope"], crate::token::ARM_SCOPE);
        assert!(seen[0].header("authorization").is_none());
    }

    #[tokio::test]
    async fn a_fresh_token_is_reused_without_another_exchange() {
        // One response only: a second exchange would find the listener gone.
        let (authority, server) = serve(vec![granted("arm-1", 3600)]).await;
        let src = source(&authority, token_file(SA_TOKEN_A));
        assert_eq!(src.token().await.unwrap().secret(), "arm-1");
        assert_eq!(server.await.unwrap().len(), 1);
        assert_eq!(src.token().await.unwrap().secret(), "arm-1");
    }

    #[tokio::test]
    async fn a_token_near_expiry_is_exchanged_again_with_the_rotated_file() {
        // Expires inside the refresh margin, so the next call exchanges; the
        // kubelet has rotated the projected token in between.
        let (authority, server) = serve(vec![granted("arm-1", 60), granted("arm-2", 3600)]).await;
        let file = token_file(SA_TOKEN_A);
        let src = source(&authority, file.clone());
        assert_eq!(src.token().await.unwrap().secret(), "arm-1");
        std::fs::write(&file, SA_TOKEN_B).unwrap();
        assert_eq!(src.token().await.unwrap().secret(), "arm-2");

        let seen = server.await.unwrap();
        assert!(seen[1].body_str().contains(SA_TOKEN_B));
    }

    #[tokio::test]
    async fn invalidate_forces_the_next_call_to_exchange() {
        let (authority, server) = serve(vec![granted("arm-1", 3600), granted("arm-2", 3600)]).await;
        let src = source(&authority, token_file(SA_TOKEN_A));
        assert_eq!(src.token().await.unwrap().secret(), "arm-1");
        src.invalidate().await;
        assert_eq!(src.token().await.unwrap().secret(), "arm-2");
        assert_eq!(server.await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_refused_exchange_keeps_entra_s_code() {
        // Captured in the spike with a wrong subject.
        let body = r#"{"error":"invalid_request","error_description":"AADSTS700213: No matching federated identity record found for presented assertion subject.","error_codes":[700213]}"#;
        let (authority, _server) = serve(vec![respond(400, &[], body)]).await;
        let src = source(&authority, token_file(SA_TOKEN_A));
        let err = src.token().await.unwrap_err();
        assert!(
            matches!(err, Error::TokenExchange { ref code, .. } if code == "AADSTS700213"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn an_unreadable_or_empty_token_file_is_an_error_that_names_the_file_only() {
        install_provider();
        let missing = std::env::temp_dir().join("banlieue-azure-no-such-sa-token");
        let src = source("https://login.microsoftonline.com", missing.clone());
        let err = src.token().await.unwrap_err();
        assert!(matches!(err, Error::TokenExchange { .. }), "{err}");
        assert!(err.to_string().contains(&missing.display().to_string()));

        let src = source("https://login.microsoftonline.com", token_file("  \n"));
        assert!(src.token().await.is_err());
    }

    #[test]
    fn an_authority_off_https_is_refused_unless_loopback() {
        install_provider();
        let config = |authority: &str| FederatedConfig {
            authority: authority.to_string(),
            tenant_id: TENANT.to_string(),
            client_id: CLIENT.to_string(),
            token_file: PathBuf::from("/var/run/secrets/azure/tokens/azure-identity-token"),
            scope: crate::token::ARM_SCOPE.to_string(),
        };
        assert!(FederatedToken::new(config("http://login.microsoftonline.com")).is_err());
        assert!(FederatedToken::new(config("https://login.microsoftonline.com")).is_ok());
        assert!(FederatedToken::new(config("http://127.0.0.1:1")).is_ok());
        let mut bad_tenant = config("https://login.microsoftonline.com");
        bad_tenant.tenant_id = "x/../y".to_string();
        assert!(FederatedToken::new(bad_tenant).is_err());
    }

    #[test]
    fn the_config_reads_the_workload_identity_variables() {
        let env: HashMap<&str, &str> = HashMap::from([
            ("AZURE_TENANT_ID", TENANT),
            ("AZURE_CLIENT_ID", CLIENT),
            (
                "AZURE_FEDERATED_TOKEN_FILE",
                "/var/run/secrets/azure/tokens/t",
            ),
        ]);
        let config =
            FederatedConfig::from_lookup(|k| env.get(k).map(ToString::to_string)).expect("config");
        assert_eq!(config.authority, crate::token::DEFAULT_AUTHORITY);
        assert_eq!(config.tenant_id, TENANT);
        assert_eq!(config.client_id, CLIENT);
        assert_eq!(config.scope, crate::token::ARM_SCOPE);

        let with_host: HashMap<&str, &str> = env
            .iter()
            .map(|(k, v)| (*k, *v))
            .chain([("AZURE_AUTHORITY_HOST", "https://login.microsoftonline.us/")])
            .collect();
        let config = FederatedConfig::from_lookup(|k| with_host.get(k).map(ToString::to_string))
            .expect("config");
        assert_eq!(config.authority, "https://login.microsoftonline.us");

        let missing = FederatedConfig::from_lookup(|_| None).unwrap_err();
        assert!(missing.to_string().contains("AZURE_TENANT_ID"), "{missing}");
    }

    /// Counts how many assertions were asked for, and returns a fixed one.
    struct CountingAssertion {
        calls: std::sync::atomic::AtomicU32,
        value: &'static str,
    }

    #[async_trait::async_trait]
    impl crate::assertion::AssertionSource for CountingAssertion {
        async fn assertion(&self) -> crate::error::Result<String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.value.to_string())
        }
    }

    fn counting(value: &'static str) -> std::sync::Arc<CountingAssertion> {
        std::sync::Arc::new(CountingAssertion {
            calls: AtomicU32::new(0),
            value,
        })
    }

    fn source_with(
        authority: &str,
        assertions: std::sync::Arc<CountingAssertion>,
    ) -> FederatedToken {
        install_provider();
        FederatedToken::with_source(
            FederatedConfig {
                authority: authority.to_string(),
                tenant_id: TENANT.to_string(),
                client_id: CLIENT.to_string(),
                token_file: PathBuf::from("/unused"),
                scope: crate::token::ARM_SCOPE.to_string(),
            },
            assertions,
        )
        .expect("source")
    }

    #[tokio::test]
    async fn another_assertion_source_is_what_gets_exchanged() {
        let (authority, server) = serve(vec![granted("arm-1", 3600)]).await;
        let src = source_with(&authority, counting("eyJ.from-openbao.sig"));

        assert_eq!(src.token().await.expect("token").secret(), "arm-1");

        let seen = server.await.unwrap();
        let form: HashMap<String, String> =
            form_urlencoded::parse(&seen[0].body).into_owned().collect();
        assert_eq!(form["client_assertion"], "eyJ.from-openbao.sig");
    }

    #[tokio::test]
    async fn a_cached_token_does_not_ask_the_assertion_source_again() {
        // The source is where OpenBao is called: a cached ARM token must
        // not cost a login and a token read on every ARM request.
        let (authority, _server) = serve(vec![granted("arm-1", 3600)]).await;
        let assertions = counting("eyJ.from-openbao.sig");
        let src = source_with(&authority, assertions.clone());

        src.token().await.unwrap();
        src.token().await.unwrap();
        src.token().await.unwrap();

        assert_eq!(assertions.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn an_assertion_source_failure_is_the_tokens_failure() {
        struct Failing;
        #[async_trait::async_trait]
        impl crate::assertion::AssertionSource for Failing {
            async fn assertion(&self) -> crate::error::Result<String> {
                Err(Error::Decode("no assertion".to_string()))
            }
        }
        install_provider();
        let src = FederatedToken::with_source(
            FederatedConfig {
                authority: "https://login.microsoftonline.com".to_string(),
                tenant_id: TENANT.to_string(),
                client_id: CLIENT.to_string(),
                token_file: PathBuf::from("/unused"),
                scope: crate::token::ARM_SCOPE.to_string(),
            },
            std::sync::Arc::new(Failing),
        )
        .unwrap();
        let err = src.token().await.unwrap_err();
        assert!(matches!(err, Error::Decode(_)), "{err}");
    }

    #[test]
    fn debug_never_prints_a_token() {
        let src = source("https://login.microsoftonline.com", token_file(SA_TOKEN_A));
        let shown = format!("{src:?}");
        assert!(!shown.contains(SA_TOKEN_A));
    }
}
