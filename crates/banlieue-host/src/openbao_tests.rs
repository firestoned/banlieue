// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `openbao.rs`, against a throwaway HTTP server on loopback.
//! The real client refuses `http://` (see the last tests); the server tests
//! build it through the test-only constructor that does not.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const JWT: &str = "eyJ.header.payload.signature-never-printed";
    const CLIENT_TOKEN: &str = "s.client-token-never-printed";
    const CSR: &str =
        "-----BEGIN CERTIFICATE REQUEST-----\nAAAA\n-----END CERTIFICATE REQUEST-----\n";
    const CERT: &str = "-----BEGIN CERTIFICATE-----\nHOST\n-----END CERTIFICATE-----\n";
    const ISSUER: &str = "-----BEGIN CERTIFICATE-----\nISSUER\n-----END CERTIFICATE-----\n";
    const ROOT: &str = "-----BEGIN CERTIFICATE-----\nROOT\n-----END CERTIFICATE-----\n";

    /// One request the fake server saw.
    #[derive(Clone, Debug)]
    struct Seen {
        method: String,
        path: String,
        token: Option<String>,
        body: String,
    }

    /// A canned reply: status and body.
    type Reply = (u16, String);

    /// Serve `replies`, one per request in order, on a loopback port.
    async fn serve(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            for (status, body) in replies {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, content_length) = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break (0, 0);
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (i + 4, len);
                    }
                };
                while buf.len() < head_end + content_length {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let mut lines = head.lines();
                let first = lines.next().unwrap_or_default().to_string();
                let mut parts = first.split(' ');
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts.next().unwrap_or_default().to_string();
                let token = head.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("x-vault-token")
                        .then(|| v.trim().to_string())
                });
                let body_text = String::from_utf8_lossy(&buf[head_end..]).to_string();
                log.lock().unwrap().push(Seen {
                    method,
                    path,
                    token,
                    body: body_text,
                });
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(reply.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (addr, seen)
    }

    fn login_ok() -> Reply {
        (
            200,
            format!(r#"{{"auth":{{"client_token":"{CLIENT_TOKEN}"}}}}"#),
        )
    }

    fn sign_ok() -> Reply {
        let body = serde_json::json!({"data": {
            "certificate": CERT,
            "issuing_ca": ISSUER,
            "ca_chain": [ISSUER, ROOT],
        }});
        (200, body.to_string())
    }

    fn revoke_ok() -> Reply {
        (204, String::new())
    }

    fn jwt() -> Credential {
        Credential::Jwt {
            auth_mount: "computefabric-test-v2".into(),
            role: "ch-host-ca-bar".into(),
            jwt: JWT.into(),
        }
    }

    fn request(c: &Credential) -> SignRequest<'_> {
        SignRequest {
            csr_pem: CSR,
            common_name: "bar",
            ttl: "8760h",
            credential: c,
        }
    }

    fn client(addr: &str) -> OpenBao {
        OpenBao::new_plain_http_for_tests(addr, "pki-node-attestation").unwrap()
    }

    fn json(s: &Seen) -> serde_json::Value {
        serde_json::from_str(&s.body).unwrap_or(serde_json::Value::Null)
    }

    #[tokio::test]
    async fn a_service_account_login_then_one_signing_returns_the_certificate_and_its_chain() {
        let (addr, seen) = serve(vec![login_ok(), sign_ok(), revoke_ok()]).await;
        let c = jwt();
        let bundle = client(&addr).sign_intermediate(&request(&c)).await.unwrap();
        assert_eq!(bundle, format!("{CERT}{ISSUER}{ROOT}"));

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        // 1. The login: the role and the service account's token, nothing else.
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].path, "/v1/auth/computefabric-test-v2/login");
        assert_eq!(
            json(&seen[0]),
            serde_json::json!({"role": "ch-host-ca-bar", "jwt": JWT})
        );
        assert_eq!(seen[0].token, None, "the login carries no token");
        // 2. The signing: exactly the pinned parameters, with the new token.
        assert_eq!(
            seen[1].path,
            "/v1/pki-node-attestation/root/sign-intermediate"
        );
        assert_eq!(seen[1].token.as_deref(), Some(CLIENT_TOKEN));
        assert_eq!(
            json(&seen[1]),
            serde_json::json!({
                "csr": CSR,
                "common_name": "bar",
                "max_path_length": "0",
                "ttl": "8760h",
                "use_csr_values": false,
            }),
            "OpenBao's ACL compares a value and its JSON type: the path length is the string \"0\", as the policy pins it"
        );
        // 3. The token is revoked.
        assert_eq!(seen[2].path, "/v1/auth/token/revoke-self");
        assert_eq!(seen[2].token.as_deref(), Some(CLIENT_TOKEN));
    }

    #[tokio::test]
    async fn a_given_token_skips_the_login() {
        let (addr, seen) = serve(vec![sign_ok(), revoke_ok()]).await;
        let c = Credential::Token(CLIENT_TOKEN.into());
        client(&addr).sign_intermediate(&request(&c)).await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[0].path.ends_with("/root/sign-intermediate"));
        assert_eq!(seen[0].token.as_deref(), Some(CLIENT_TOKEN));
    }

    #[tokio::test]
    async fn the_chain_falls_back_to_the_issuing_ca_when_there_is_no_ca_chain() {
        let body = serde_json::json!({"data": {"certificate": CERT, "issuing_ca": ISSUER}});
        let (addr, _) = serve(vec![login_ok(), (200, body.to_string()), revoke_ok()]).await;
        let c = jwt();
        let bundle = client(&addr).sign_intermediate(&request(&c)).await.unwrap();
        assert_eq!(bundle, format!("{CERT}{ISSUER}"));
    }

    #[tokio::test]
    async fn a_refused_login_is_an_error_that_names_the_status_and_not_the_body() {
        let (addr, seen) = serve(vec![(403, r#"{"errors":["BODY-ECHOES-jwt"]}"#.into())]).await;
        let c = jwt();
        let err = client(&addr)
            .sign_intermediate(&request(&c))
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("403"), "{text}");
        assert!(text.contains("login"), "{text}");
        assert!(!text.contains("BODY-ECHOES"), "{text}");
        assert!(!text.contains(JWT), "{text}");
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "nothing is signed after a refused login"
        );
    }

    #[tokio::test]
    async fn a_refused_signing_is_an_error_and_the_token_is_still_revoked() {
        let (addr, seen) = serve(vec![
            login_ok(),
            (400, r#"{"errors":["BODY-ECHOES-csr"]}"#.into()),
            revoke_ok(),
        ])
        .await;
        let c = jwt();
        let err = client(&addr)
            .sign_intermediate(&request(&c))
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("400") && text.contains("sign"), "{text}");
        assert!(!text.contains("BODY-ECHOES"), "{text}");
        assert!(!text.contains(CLIENT_TOKEN), "{text}");
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_failed_revoke_is_ignored() {
        // A one-use token is already gone when the signing returns: 403 is expected.
        let (addr, _) = serve(vec![login_ok(), sign_ok(), (403, "{}".into())]).await;
        let c = jwt();
        assert!(client(&addr).sign_intermediate(&request(&c)).await.is_ok());
    }

    #[tokio::test]
    async fn a_response_without_a_certificate_is_an_error() {
        for body in [
            r#"{"data":{}}"#,
            r#"{"data":{"certificate":""}}"#,
            r#"{"data":{"certificate":7}}"#,
            "not json",
        ] {
            let (addr, _) = serve(vec![login_ok(), (200, body.into()), revoke_ok()]).await;
            let c = jwt();
            let r = client(&addr).sign_intermediate(&request(&c)).await;
            assert!(r.is_err(), "{body}");
        }
    }

    #[tokio::test]
    async fn a_login_without_a_client_token_is_an_error() {
        let (addr, seen) = serve(vec![(200, r#"{"auth":null}"#.into())]).await;
        let c = jwt();
        assert!(client(&addr).sign_intermediate(&request(&c)).await.is_err());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_oversized_response_is_refused_before_it_is_all_read() {
        let huge = "x".repeat(MAX_RESPONSE_BYTES + 1024);
        let (addr, _) = serve(vec![login_ok(), (200, huge), revoke_ok()]).await;
        let c = jwt();
        let err = client(&addr)
            .sign_intermediate(&request(&c))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err}");
    }

    #[test]
    fn the_real_client_refuses_http_credentials_in_the_url_a_query_and_a_missing_host() {
        for bad in [
            "http://bao.example.com:8200",
            "https://user:pass@bao.example.com",
            "https://bao.example.com/?x=1",
            "https://bao.example.com/#f",
            "https://",
            "not a url",
            "",
        ] {
            assert!(OpenBao::new(bad, "pki-node").is_err(), "{bad:?}");
        }
        assert!(OpenBao::new("https://bao.example.com:8200", "pki-node").is_ok());
        assert!(OpenBao::new("https://bao.example.com/", "pki-node").is_ok());
    }

    #[test]
    fn a_mount_or_auth_mount_that_could_leave_its_place_is_refused() {
        for bad in [
            "", "/pki", "pki/", "../x", "a/../b", "a b", "a?b", "a\nb", "a#b",
        ] {
            assert!(
                OpenBao::new("https://bao.example.com", bad).is_err(),
                "{bad:?}"
            );
        }
        assert!(OpenBao::new("https://bao.example.com", "pki-node-attestation").is_ok());
        assert!(OpenBao::new("https://bao.example.com", "team/pki-node").is_ok());
    }

    #[tokio::test]
    async fn a_credential_with_a_bad_auth_mount_or_role_is_refused_before_any_request() {
        let (addr, seen) = serve(vec![]).await;
        for (auth_mount, role) in [("../x", "r"), ("a", "r/../x"), ("", "r"), ("a", "")] {
            let c = Credential::Jwt {
                auth_mount: auth_mount.into(),
                role: role.into(),
                jwt: JWT.into(),
            };
            assert!(
                client(&addr).sign_intermediate(&request(&c)).await.is_err(),
                "{auth_mount:?} {role:?}"
            );
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_credential_never_prints_its_secret() {
        for c in [jwt(), Credential::Token(CLIENT_TOKEN.into())] {
            let shown = format!("{c:?}");
            assert!(
                !shown.contains(JWT) && !shown.contains(CLIENT_TOKEN),
                "{shown}"
            );
        }
    }

    #[tokio::test]
    async fn nothing_listening_is_an_error_not_a_hang() {
        let c = jwt();
        // A port nothing owns: bind and drop to find one.
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        assert!(client(&addr).sign_intermediate(&request(&c)).await.is_err());
    }
}
