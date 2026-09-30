// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `client.rs`, against a one-shot local HTTP server that can
//! speak Proxmox's habit of putting the error in the reason phrase.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    use super::super::*;
    use crate::{ApiToken, CloneParams, Error, Params, ProxmoxApi};

    const UPID: &str = "UPID:pve1:0004A2B1:03C1D2E3:6512F0A0:qmclone:9000:banlieue@pve!provider:";

    struct Recorded {
        request_line: String,
        headers: String,
        body: Vec<u8>,
    }

    impl Recorded {
        fn header(&self, name: &str) -> Option<String> {
            self.headers.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
            })
        }
        fn body_str(&self) -> String {
            String::from_utf8_lossy(&self.body).into_owned()
        }
    }

    fn install_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    fn ok(json: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
            json.len()
        )
    }

    fn fail(status: u16, reason: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// Serve each response to one connection, in order; return what arrived.
    async fn serve(responses: Vec<String>) -> (String, JoinHandle<Vec<Recorded>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut seen = Vec::new();
            for response in responses {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let head_end = loop {
                    let n = sock.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break buf.len();
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let len = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + len {
                    let n = sock.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                }
                let (request_line, headers) = head.split_once("\r\n").unwrap();
                seen.push(Recorded {
                    request_line: request_line.to_string(),
                    headers: headers.to_string(),
                    body: buf[head_end..head_end + len].to_vec(),
                });
                sock.write_all(response.as_bytes()).await.unwrap();
                sock.shutdown().await.ok();
            }
            seen
        });
        (endpoint, handle)
    }

    fn client(endpoint: &str) -> Client {
        install_provider();
        let token = ApiToken::new("banlieue@pve!provider", "s3cret").unwrap();
        Client::new(ClientConfig::new(endpoint, token)).unwrap()
    }

    // ---- configuration -------------------------------------------------

    #[test]
    fn endpoint_without_a_scheme_is_rejected() {
        install_provider();
        let token = ApiToken::new("a@pve!t", "s").unwrap();
        let e = Client::new(ClientConfig::new("bar.foo.io:8006", token)).unwrap_err();
        assert!(matches!(e, Error::Config(_)), "{e}");
    }

    #[test]
    fn a_non_http_scheme_is_rejected() {
        install_provider();
        let token = ApiToken::new("a@pve!t", "s").unwrap();
        assert!(Client::new(ClientConfig::new("ftp://bar.foo.io", token)).is_err());
    }

    #[test]
    fn a_bad_ca_bundle_is_rejected_not_ignored() {
        install_provider();
        let token = ApiToken::new("a@pve!t", "s").unwrap();
        let mut cfg = ClientConfig::new("https://bar.foo.io:8006", token);
        cfg.ca_bundle_pem = Some("not a certificate".to_string());
        assert!(matches!(Client::new(cfg), Err(Error::Config(_))));
    }

    #[test]
    fn trailing_slashes_on_the_endpoint_are_normalised() {
        install_provider();
        let token = ApiToken::new("a@pve!t", "s").unwrap();
        let c = Client::new(ClientConfig::new("https://bar.foo.io:8006//", token)).unwrap();
        assert_eq!(c.base_url(), "https://bar.foo.io:8006/api2/json");
    }

    // ---- requests ------------------------------------------------------

    #[tokio::test]
    async fn get_sends_the_token_and_decodes_data() {
        let (ep, seen) = serve(vec![ok(r#"{"data":{"version":"9.0.3"}}"#)]).await;
        let v = client(&ep).version().await.unwrap();
        assert_eq!(v.version, "9.0.3");
        let r = &seen.await.unwrap()[0];
        assert_eq!(r.request_line, "GET /api2/json/version HTTP/1.1");
        assert_eq!(
            r.header("authorization").unwrap(),
            "PVEAPIToken=banlieue@pve!provider=s3cret"
        );
    }

    #[tokio::test]
    async fn get_with_parameters_uses_a_query_string() {
        let (ep, seen) = serve(vec![ok(r#"{"data":[]}"#)]).await;
        client(&ep).cluster_vms().await.unwrap();
        assert_eq!(
            seen.await.unwrap()[0].request_line,
            "GET /api2/json/cluster/resources?type=vm HTTP/1.1"
        );
    }

    #[tokio::test]
    async fn clone_posts_a_form_body_and_returns_the_upid() {
        let (ep, seen) = serve(vec![ok(&format!(r#"{{"data":"{UPID}"}}"#))]).await;
        let upid = client(&ep)
            .clone_vm("pve1", 9000, &CloneParams::new(101).name("vm-a"))
            .await
            .unwrap();
        assert_eq!(upid.as_str(), UPID);
        let r = &seen.await.unwrap()[0];
        assert_eq!(
            r.request_line,
            "POST /api2/json/nodes/pve1/qemu/9000/clone HTTP/1.1"
        );
        assert_eq!(
            r.header("content-type").unwrap(),
            "application/x-www-form-urlencoded"
        );
        assert_eq!(r.body_str(), "newid=101&name=vm-a&full=1");
    }

    #[tokio::test]
    async fn set_config_puts_a_form_body() {
        let (ep, seen) = serve(vec![ok(r#"{"data":null}"#)]).await;
        client(&ep)
            .set_vm_config(
                "pve1",
                100,
                &Params::new()
                    .set("cores", 2)
                    .set("net0", "virtio,bridge=vmbr0"),
            )
            .await
            .unwrap();
        let r = &seen.await.unwrap()[0];
        assert_eq!(
            r.request_line,
            "PUT /api2/json/nodes/pve1/qemu/100/config HTTP/1.1"
        );
        assert_eq!(r.body_str(), "cores=2&net0=virtio%2Cbridge%3Dvmbr0");
    }

    #[tokio::test]
    async fn delete_vm_purges_through_the_query_string() {
        let (ep, seen) = serve(vec![ok(&format!(r#"{{"data":"{UPID}"}}"#))]).await;
        client(&ep).delete_vm("pve1", 100).await.unwrap();
        assert_eq!(
            seen.await.unwrap()[0].request_line,
            "DELETE /api2/json/nodes/pve1/qemu/100?purge=1&destroy-unreferenced-disks=1 HTTP/1.1"
        );
    }

    #[tokio::test]
    async fn task_status_polls_the_node_named_in_the_upid() {
        let (ep, seen) = serve(vec![ok(
            r#"{"data":{"status":"stopped","exitstatus":"OK"}}"#,
        )])
        .await;
        let upid = crate::Upid::parse(UPID).unwrap();
        assert!(client(&ep).task_status(&upid).await.unwrap().succeeded());
        assert_eq!(
            seen.await.unwrap()[0].request_line,
            "GET /api2/json/nodes/pve1/tasks/UPID:pve1:0004A2B1:03C1D2E3:6512F0A0:qmclone:9000:banlieue%40pve%21provider:/status HTTP/1.1"
        );
    }

    #[tokio::test]
    async fn a_volid_is_escaped_into_one_path_segment() {
        let (ep, seen) = serve(vec![ok(&format!(r#"{{"data":"{UPID}"}}"#))]).await;
        client(&ep)
            .delete_volume("pve1", "local", "local:iso/seed-100.iso")
            .await
            .unwrap();
        assert_eq!(
            seen.await.unwrap()[0].request_line,
            "DELETE /api2/json/nodes/pve1/storage/local/content/local:iso%2Fseed-100.iso HTTP/1.1"
        );
    }

    #[tokio::test]
    async fn agent_interfaces_unwrap_the_result_key() {
        let (ep, _seen) = serve(vec![ok(
            r#"{"data":{"result":[{"name":"eth0","ip-addresses":[{"ip-address":"192.0.2.10","ip-address-type":"ipv4","prefix":24}]}]}}"#,
        )])
        .await;
        let i = client(&ep).agent_interfaces("pve1", 100).await.unwrap();
        assert_eq!(i[0].ip_addresses[0].ip_address, "192.0.2.10");
    }

    #[tokio::test]
    async fn next_id_decodes_the_string_form() {
        let (ep, _s) = serve(vec![ok(r#"{"data":"105"}"#)]).await;
        assert_eq!(client(&ep).next_id().await.unwrap().0, 105);
    }

    #[tokio::test]
    async fn upload_sends_a_multipart_body_with_the_iso() {
        let (ep, seen) = serve(vec![ok(&format!(r#"{{"data":"{UPID}"}}"#))]).await;
        client(&ep)
            .upload_iso("pve1", "local", "seed-100.iso", b"ISODATA".to_vec())
            .await
            .unwrap();
        let r = &seen.await.unwrap()[0];
        assert_eq!(
            r.request_line,
            "POST /api2/json/nodes/pve1/storage/local/upload HTTP/1.1"
        );
        let ct = r.header("content-type").unwrap();
        let boundary = ct.strip_prefix("multipart/form-data; boundary=").unwrap();
        let body = r.body_str();
        assert!(body.starts_with(&format!("--{boundary}\r\n")), "{body}");
        assert!(body.contains("name=\"content\"\r\n\r\niso\r\n"), "{body}");
        assert!(
            body.contains("name=\"filename\"; filename=\"seed-100.iso\""),
            "{body}"
        );
        assert!(body.contains("\r\n\r\nISODATA\r\n"), "{body}");
        assert!(body.ends_with(&format!("--{boundary}--\r\n")), "{body}");
    }

    #[tokio::test]
    async fn upload_refuses_a_filename_that_could_inject_headers() {
        let (ep, _s) = serve(vec![]).await;
        let c = client(&ep);
        for bad in ["a\".iso", "a\r\nb.iso", "../a.iso", "a/b.iso", ""] {
            let e = c
                .upload_iso("pve1", "local", bad, vec![])
                .await
                .unwrap_err();
            assert!(matches!(e, Error::Config(_)), "{bad:?}: {e}");
        }
    }

    // ---- errors ----------------------------------------------------------

    #[tokio::test]
    async fn the_reason_phrase_becomes_the_error_message() {
        let (ep, _s) = serve(vec![fail(401, "Authentication failed!", "")]).await;
        let e = client(&ep).version().await.unwrap_err();
        match &e {
            Error::Api { status, message } => {
                assert_eq!(*status, 401);
                assert_eq!(message, "Authentication failed!");
            }
            other => panic!("{other}"),
        }
        assert!(e.is_unauthorized());
    }

    #[tokio::test]
    async fn a_missing_vm_is_recognised_from_the_500_reason() {
        let (ep, _s) = serve(vec![fail(
            500,
            "Configuration file 'nodes/pve1/qemu-server/9.conf' does not exist",
            "",
        )])
        .await;
        assert!(
            client(&ep)
                .vm_status("pve1", 9)
                .await
                .unwrap_err()
                .is_not_found()
        );
    }

    #[tokio::test]
    async fn validation_errors_in_the_body_are_appended() {
        let (ep, _s) = serve(vec![fail(
            400,
            "Parameter verification failed.",
            r#"{"data":null,"errors":{"cores":"value must be an integer"}}"#,
        )])
        .await;
        let e = client(&ep)
            .set_vm_config("pve1", 1, &Params::new().set("cores", "x"))
            .await
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "Proxmox returned 400: Parameter verification failed.: cores: value must be an integer"
        );
    }

    #[tokio::test]
    async fn a_canonical_reason_still_yields_a_message() {
        let (ep, _s) = serve(vec![fail(404, "Not Found", "")]).await;
        let e = client(&ep).version().await.unwrap_err();
        assert!(e.is_not_found());
        assert!(e.to_string().contains("Not Found"), "{e}");
    }

    #[tokio::test]
    async fn a_2xx_that_is_not_json_is_a_decode_error() {
        let (ep, _s) = serve(vec![ok("<html>")]).await;
        assert!(matches!(client(&ep).version().await, Err(Error::Decode(_))));
    }

    #[tokio::test]
    async fn a_refused_connection_is_a_transport_error() {
        let ep = {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            format!("http://{}", l.local_addr().unwrap())
        };
        let e = client(&ep).version().await.unwrap_err();
        assert!(matches!(e, Error::Transport(_)), "{e}");
    }

    #[tokio::test]
    async fn a_server_that_never_answers_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ep = format!("http://{}", listener.local_addr().unwrap());
        let _hold = tokio::spawn(async move {
            let (_sock, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        install_provider();
        let token = ApiToken::new("a@pve!t", "s").unwrap();
        let mut cfg = ClientConfig::new(&ep, token);
        cfg.request_timeout = Duration::from_millis(100);
        let e = Client::new(cfg).unwrap().version().await.unwrap_err();
        assert!(matches!(e, Error::Timeout(_)), "{e}");
    }

    #[tokio::test]
    async fn redirects_are_not_followed_so_the_token_stays_put() {
        let redirect = "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string();
        let (ep, _s) = serve(vec![redirect]).await;
        let e = client(&ep).version().await.unwrap_err();
        assert!(matches!(e, Error::Api { status: 302, .. }), "{e}");
    }

    #[test]
    fn debug_output_of_the_config_does_not_leak_the_secret() {
        let token = ApiToken::new("a@pve!t", "topsecretvalue").unwrap();
        let cfg = ClientConfig::new("https://bar.foo.io:8006", token);
        assert!(!format!("{cfg:?}").contains("topsecretvalue"));
    }

    // ---- response size -------------------------------------------------

    fn client_with_cap(endpoint: &str, cap: usize) -> Client {
        install_provider();
        let token = ApiToken::new("a@pve!t", "s").unwrap();
        let mut cfg = ClientConfig::new(endpoint, token);
        cfg.max_response_bytes = cap;
        Client::new(cfg).unwrap()
    }

    #[tokio::test]
    async fn an_oversized_response_is_refused() {
        let big = format!(r#"{{"data":"{}"}}"#, "x".repeat(4096));
        let (ep, _s) = serve(vec![ok(&big)]).await;
        let e = client_with_cap(&ep, 1024).version().await.unwrap_err();
        assert!(matches!(e, Error::ResponseTooLarge { limit: 1024 }), "{e}");
    }

    #[tokio::test]
    async fn an_error_body_over_the_cap_is_refused_too() {
        let big = "y".repeat(4096);
        let (ep, _s) = serve(vec![fail(500, "Boom", &big)]).await;
        let e = client_with_cap(&ep, 1024).version().await.unwrap_err();
        assert!(matches!(e, Error::ResponseTooLarge { .. }), "{e}");
    }

    #[tokio::test]
    async fn a_response_at_the_cap_is_accepted() {
        let body = r#"{"data":{"version":"9.0.3"}}"#;
        let (ep, _s) = serve(vec![ok(body)]).await;
        let v = client_with_cap(&ep, body.len()).version().await.unwrap();
        assert_eq!(v.version, "9.0.3");
    }

    #[test]
    fn the_default_cap_is_sixteen_mebibytes() {
        install_provider();
        let token = ApiToken::new("a@pve!t", "s").unwrap();
        assert_eq!(
            ClientConfig::new("https://bar.foo.io", token).max_response_bytes,
            16 * 1024 * 1024
        );
    }

    // ---- resize: a task on PVE 9, synchronous before --------------------

    #[tokio::test]
    async fn resize_returns_the_task_pve_9_starts() {
        let upid = "UPID:pve1:0011A2D9:0264C368:6ABD159F:resize:101:banlieue@pve!provider:";
        let (ep, seen) = serve(vec![ok(&format!(r#"{{"data":"{upid}"}}"#))]).await;
        let got = client(&ep)
            .resize_disk("pve1", 101, "scsi0", "4G")
            .await
            .unwrap();
        assert_eq!(got.map(|u| u.as_str().to_string()).as_deref(), Some(upid));
        let r = &seen.await.unwrap()[0];
        assert_eq!(
            r.request_line,
            "PUT /api2/json/nodes/pve1/qemu/101/resize HTTP/1.1"
        );
        assert_eq!(r.body_str(), "disk=scsi0&size=4G");
    }

    #[tokio::test]
    async fn resize_accepts_the_synchronous_null_older_releases_return() {
        let (ep, _s) = serve(vec![ok(r#"{"data":null}"#)]).await;
        assert!(
            client(&ep)
                .resize_disk("pve1", 101, "scsi0", "4G")
                .await
                .unwrap()
                .is_none()
        );
    }
}
