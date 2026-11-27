// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `assertion.rs`: the file source, and the OpenBao source
//! against a local stand-in for OpenBao's HTTP API (ADR-0176).

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::super::*;
    use crate::error::Error;
    use crate::test_server::{install_provider, ok, respond, serve};

    const SA_TOKEN: &str = "eyJ.sa-token-for-openbao.sig";
    const LOGIN_TOKEN: &str = "s.login-token-secret";
    const OIDC_JWT: &str = "eyJ.openbao-signed-assertion.sig";
    const NAMESPACE: &str = "banlieue-spike";
    const MOUNT: &str = "k8s-jwt";
    const LOGIN_ROLE: &str = "azure-prov";
    const TOKEN_ROLE: &str = "azure-prov";
    const REFUSAL_BODY: &str = "permission denied: secret-detail-from-openbao";

    static NEXT_FILE: AtomicU32 = AtomicU32::new(0);

    fn token_file(contents: &str) -> PathBuf {
        let n = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("banlieue-azure-bao-{}-{n}", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn login_ok() -> String {
        ok(&format!(r#"{{"auth":{{"client_token":"{LOGIN_TOKEN}"}}}}"#))
    }

    fn token_ok() -> String {
        ok(&format!(
            r#"{{"data":{{"token":"{OIDC_JWT}","client_id":"api://AzureADTokenExchange","ttl":3600}}}}"#
        ))
    }

    fn revoke_ok() -> String {
        respond(204, &[], "")
    }

    fn config(addr: &str, namespace: Option<&str>, file: PathBuf) -> OpenBaoConfig {
        OpenBaoConfig {
            addr: addr.to_string(),
            namespace: namespace.map(str::to_string),
            auth_mount: MOUNT.to_string(),
            login_role: LOGIN_ROLE.to_string(),
            token_role: TOKEN_ROLE.to_string(),
            service_account_token_file: file,
        }
    }

    fn source(addr: &str, namespace: Option<&str>) -> OpenBaoAssertion {
        install_provider();
        OpenBaoAssertion::new(config(
            addr,
            namespace,
            token_file(&format!("{SA_TOKEN}\n")),
        ))
        .expect("source")
    }

    // ---- FileAssertion ----

    #[tokio::test]
    async fn a_file_assertion_is_the_trimmed_file_contents() {
        let src = FileAssertion::new(token_file(&format!("{SA_TOKEN}\n")));
        assert_eq!(src.assertion().await.expect("assertion"), SA_TOKEN);
    }

    #[tokio::test]
    async fn a_file_assertion_is_read_again_every_time() {
        let path = token_file("first");
        let src = FileAssertion::new(path.clone());
        assert_eq!(src.assertion().await.unwrap(), "first");
        std::fs::write(&path, "second").unwrap();
        assert_eq!(src.assertion().await.unwrap(), "second");
    }

    #[tokio::test]
    async fn an_empty_or_missing_assertion_file_is_refused_without_its_contents() {
        let empty = FileAssertion::new(token_file("  \n"));
        let err = empty.assertion().await.unwrap_err();
        assert_eq!(err.code(), Some("ServiceAccountTokenUnreadable"));

        let missing = FileAssertion::new(PathBuf::from("/nonexistent/banlieue-azure/token"));
        let err = missing.assertion().await.unwrap_err();
        assert_eq!(err.code(), Some("ServiceAccountTokenUnreadable"));
    }

    // ---- OpenBaoAssertion: the exchange ----

    #[tokio::test]
    async fn the_assertion_is_a_login_then_a_token_read_then_a_revoke() {
        let (addr, server) = serve(vec![login_ok(), token_ok(), revoke_ok()]).await;
        let src = source(&addr, Some(NAMESPACE));

        let jwt = src.assertion().await.expect("assertion");
        assert_eq!(jwt, OIDC_JWT);

        let seen = server.await.unwrap();
        assert_eq!(seen.len(), 3);

        assert_eq!(seen[0].target(), format!("POST /v1/auth/{MOUNT}/login"));
        let body: HashMap<String, String> = serde_json::from_slice(&seen[0].body).unwrap();
        assert_eq!(body["role"], LOGIN_ROLE);
        assert_eq!(body["jwt"], SA_TOKEN, "trailing newline trimmed");
        assert!(seen[0].header("x-vault-token").is_none());

        assert_eq!(
            seen[1].target(),
            format!("GET /v1/identity/oidc/token/{TOKEN_ROLE}")
        );
        assert_eq!(
            seen[1].header("x-vault-token").as_deref(),
            Some(LOGIN_TOKEN)
        );

        assert_eq!(seen[2].target(), "POST /v1/auth/token/revoke-self");
        assert_eq!(
            seen[2].header("x-vault-token").as_deref(),
            Some(LOGIN_TOKEN)
        );
    }

    #[tokio::test]
    async fn the_namespace_travels_in_a_header_on_every_request() {
        let (addr, server) = serve(vec![login_ok(), token_ok(), revoke_ok()]).await;
        source(&addr, Some(NAMESPACE)).assertion().await.unwrap();
        for r in server.await.unwrap() {
            assert_eq!(r.header("x-vault-namespace").as_deref(), Some(NAMESPACE));
        }
    }

    #[tokio::test]
    async fn no_namespace_means_no_namespace_header() {
        let (addr, server) = serve(vec![login_ok(), token_ok(), revoke_ok()]).await;
        source(&addr, None).assertion().await.unwrap();
        for r in server.await.unwrap() {
            assert!(r.header("x-vault-namespace").is_none());
        }
    }

    #[tokio::test]
    async fn a_failed_revoke_does_not_fail_the_assertion() {
        let (addr, _server) = serve(vec![
            login_ok(),
            token_ok(),
            respond(500, &[], r#"{"errors":["boom"]}"#),
        ])
        .await;
        let jwt = source(&addr, None).assertion().await.expect("assertion");
        assert_eq!(jwt, OIDC_JWT);
    }

    // ---- OpenBaoAssertion: failures never leak ----

    #[tokio::test]
    async fn a_refused_login_names_the_step_and_status_and_nothing_else() {
        let (addr, server) = serve(vec![respond(403, &[], REFUSAL_BODY)]).await;
        let err = source(&addr, None).assertion().await.unwrap_err();

        let shown = err.to_string();
        assert!(shown.contains("login"), "{shown}");
        assert!(shown.contains("403"), "{shown}");
        assert!(!shown.contains("secret-detail"), "body leaked: {shown}");
        assert!(!shown.contains(SA_TOKEN), "token leaked: {shown}");
        assert_eq!(
            server.await.unwrap().len(),
            1,
            "no token read after a refusal"
        );
    }

    #[tokio::test]
    async fn a_refused_token_read_still_revokes_the_login_token() {
        let (addr, server) = serve(vec![
            login_ok(),
            respond(403, &[], REFUSAL_BODY),
            revoke_ok(),
        ])
        .await;
        let err = source(&addr, None).assertion().await.unwrap_err();

        let shown = err.to_string();
        assert!(shown.contains("token read"), "{shown}");
        assert!(shown.contains("403"), "{shown}");
        assert!(!shown.contains("secret-detail"), "{shown}");
        assert!(!shown.contains(LOGIN_TOKEN), "{shown}");

        let seen = server.await.unwrap();
        assert_eq!(seen[2].target(), "POST /v1/auth/token/revoke-self");
    }

    #[tokio::test]
    async fn a_login_answer_without_a_client_token_is_a_decode_error() {
        let (addr, _s) = serve(vec![ok(r#"{"auth":null,"warnings":["w"]}"#)]).await;
        let err = source(&addr, None).assertion().await.unwrap_err();
        assert!(matches!(err, Error::Decode(_)), "{err}");
        assert!(!err.to_string().contains("warnings"), "{err}");
    }

    #[tokio::test]
    async fn a_token_answer_without_a_token_is_a_decode_error() {
        let (addr, _s) = serve(vec![login_ok(), ok(r#"{"data":{}}"#), revoke_ok()]).await;
        let err = source(&addr, None).assertion().await.unwrap_err();
        assert!(matches!(err, Error::Decode(_)), "{err}");
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed_and_is_an_error() {
        let (addr, server) = serve(vec![respond(
            307,
            &[("Location", "https://attacker.invalid/steal")],
            "",
        )])
        .await;
        let err = source(&addr, None).assertion().await.unwrap_err();
        assert!(err.to_string().contains("307"), "{err}");
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_empty_service_account_token_never_reaches_openbao() {
        install_provider();
        let src =
            OpenBaoAssertion::new(config("http://127.0.0.1:1", None, token_file("\n"))).unwrap();
        let err = src.assertion().await.unwrap_err();
        assert_eq!(err.code(), Some("ServiceAccountTokenUnreadable"));
    }

    // ---- OpenBaoAssertion: what it will talk to ----

    #[test]
    fn only_https_or_loopback_http_may_receive_a_service_account_token() {
        install_provider();
        for bad in [
            "http://openbao.example.com",
            "ftp://x",
            "openbao.example.com",
            "",
        ] {
            let err = OpenBaoAssertion::new(config(bad, None, token_file("t"))).unwrap_err();
            assert_eq!(err.code(), Some("InvalidConfiguration"), "{bad}");
        }
        for good in [
            "https://openbao.example.com",
            "https://openbao.example.com:8200",
            "http://127.0.0.1:8200",
        ] {
            assert!(
                OpenBaoAssertion::new(config(good, None, token_file("t"))).is_ok(),
                "{good}"
            );
        }
    }

    #[test]
    fn names_that_could_change_the_request_path_are_refused() {
        install_provider();
        let base = "https://openbao.example.com";
        for bad in [
            "",
            "..",
            "a/../b",
            "a b",
            "a?b",
            "a#b",
            "a%2fb",
            "/leading",
            "trailing/",
        ] {
            let mut c = config(base, None, token_file("t"));
            c.auth_mount = bad.to_string();
            assert!(OpenBaoAssertion::new(c).is_err(), "mount {bad:?}");

            let mut c = config(base, None, token_file("t"));
            c.login_role = bad.to_string();
            assert!(OpenBaoAssertion::new(c).is_err(), "login role {bad:?}");

            let mut c = config(base, None, token_file("t"));
            c.token_role = bad.to_string();
            assert!(OpenBaoAssertion::new(c).is_err(), "token role {bad:?}");

            let mut c = config(base, Some(bad), token_file("t"));
            c.namespace = Some(bad.to_string());
            assert!(OpenBaoAssertion::new(c).is_err(), "namespace {bad:?}");
        }
    }

    #[test]
    fn dotted_and_nested_names_that_openbao_allows_are_accepted() {
        install_provider();
        let mut c = config(
            "https://openbao.example.com",
            Some("team/sub-ns"),
            token_file("t"),
        );
        c.auth_mount = "computefabric-cluster.example.com-v2".to_string();
        assert!(OpenBaoAssertion::new(c).is_ok());
    }

    // ---- OpenBaoConfig::from_lookup ----

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    const FULL: [(&str, &str); 5] = [
        (ENV_OPENBAO_ADDR, "https://openbao.example.com"),
        (ENV_OPENBAO_AUTH_MOUNT, MOUNT),
        (ENV_OPENBAO_LOGIN_ROLE, LOGIN_ROLE),
        (ENV_OPENBAO_TOKEN_ROLE, TOKEN_ROLE),
        (ENV_OPENBAO_SA_TOKEN_FILE, "/var/run/secrets/openbao/token"),
    ];

    #[test]
    fn the_config_reads_every_variable_and_the_namespace_is_optional() {
        let c = OpenBaoConfig::from_lookup(lookup(&FULL)).expect("config");
        assert_eq!(c.addr, "https://openbao.example.com");
        assert_eq!(c.namespace, None);
        assert_eq!(c.auth_mount, MOUNT);
        assert_eq!(
            c.service_account_token_file,
            PathBuf::from("/var/run/secrets/openbao/token")
        );

        let mut with_ns = FULL.to_vec();
        with_ns.push((ENV_OPENBAO_NAMESPACE, NAMESPACE));
        let c = OpenBaoConfig::from_lookup(lookup(&with_ns)).unwrap();
        assert_eq!(c.namespace.as_deref(), Some(NAMESPACE));
    }

    #[test]
    fn a_missing_variable_is_named() {
        for (name, _) in FULL {
            let rest: Vec<_> = FULL.iter().copied().filter(|(k, _)| *k != name).collect();
            let err = OpenBaoConfig::from_lookup(lookup(&rest)).unwrap_err();
            assert!(err.to_string().contains(name), "{name}: {err}");
        }
    }

    #[test]
    fn a_blank_namespace_is_no_namespace() {
        let mut v = FULL.to_vec();
        v.push((ENV_OPENBAO_NAMESPACE, "  "));
        assert_eq!(
            OpenBaoConfig::from_lookup(lookup(&v)).unwrap().namespace,
            None
        );
    }
}
