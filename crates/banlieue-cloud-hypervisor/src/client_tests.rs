// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `client.rs`, against a fake VMM on a real Unix socket.
//!
//! The fake speaks just enough HTTP/1.1 to answer one request per
//! connection, and records what it was sent, so these exercise the real
//! transport end to end without a hypervisor.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::Error;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    const TIMEOUT: Duration = Duration::from_secs(5);

    /// What the fake saw: method, path, body.
    type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

    /// Serve canned `(status, body)` replies, one per connection, in order.
    fn fake_vmm(dir: &tempfile::TempDir, replies: Vec<(u16, &'static str)>) -> (PathBuf, Seen) {
        let path = dir.path().join("api.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let seen: Seen = Arc::default();
        let log = seen.clone();
        tokio::spawn(async move {
            for (status, body) in replies {
                let (mut conn, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0_u8; 4096];
                // Read the head, then exactly content-length bytes of body.
                let head_end = loop {
                    let n = conn.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let len = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + len {
                    let n = conn.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                }
                let mut first = head.lines().next().unwrap().split(' ');
                let method = first.next().unwrap().to_string();
                let target = first.next().unwrap().to_string();
                let req_body = String::from_utf8_lossy(&buf[head_end..head_end + len]).to_string();
                log.lock().unwrap().push((method, target, req_body));

                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                conn.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        (path, seen)
    }

    fn plan() -> crate::types::GuestPlan {
        crate::types::GuestPlan {
            firmware: PathBuf::from("/fw/CLOUDHV.fd"),
            boot_vcpus: 1,
            max_vcpus: 1,
            memory_mib: 512,
            hugepages: false,
            disks: vec![],
            nics: vec![],
            tpm_socket: None,
            serial_file: PathBuf::from("/srv/serial.log"),
            landlock: false,
        }
    }

    #[tokio::test]
    async fn ping_decodes_a_real_reply() {
        let dir = tempfile::tempdir().unwrap();
        let (path, seen) = fake_vmm(
            &dir,
            vec![(200, include_str!("../tests/fixtures/vmm-ping-v53.0.json"))],
        );
        let ping = Client::new(&path, TIMEOUT).ping().await.unwrap();
        assert_eq!(ping.version, "53.0.0");
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, "GET");
        assert_eq!(seen[0].1, "/api/v1/vmm.ping");
    }

    #[tokio::test]
    async fn create_puts_the_planned_config() {
        let dir = tempfile::tempdir().unwrap();
        let (path, seen) = fake_vmm(&dir, vec![(204, "")]);
        Client::new(&path, TIMEOUT).create(&plan()).await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, "PUT");
        assert_eq!(seen[0].1, "/api/v1/vm.create");
        let body: serde_json::Value = serde_json::from_str(&seen[0].2).unwrap();
        assert_eq!(body["payload"]["firmware"], "/fw/CLOUDHV.fd");
        assert_eq!(body["cpus"]["nested"], false);
    }

    #[tokio::test]
    async fn info_decodes_a_real_reply() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = fake_vmm(
            &dir,
            vec![(
                200,
                include_str!("../tests/fixtures/vm-info-created-v53.0.json"),
            )],
        );
        let info = Client::new(&path, TIMEOUT).info().await.unwrap();
        assert_eq!(info.map(|i| i.state), Some(crate::types::VmState::Created));
    }

    /// A VMM with no VM answers vm.info with 404. That is "nothing created
    /// yet", not a failure.
    #[tokio::test]
    async fn info_on_a_vmm_with_no_vm_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = fake_vmm(
            &dir,
            vec![(
                404,
                include_str!("../tests/fixtures/error-vm-not-created-v53.0.json"),
            )],
        );
        assert!(Client::new(&path, TIMEOUT).info().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_api_error_carries_the_vmm_messages() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = fake_vmm(
            &dir,
            vec![(
                500,
                include_str!("../tests/fixtures/error-vm-already-created-v53.0.json"),
            )],
        );
        let err = Client::new(&path, TIMEOUT)
            .create(&plan())
            .await
            .unwrap_err();
        let Error::Api { status, messages } = err else {
            panic!("expected Error::Api, got {err:?}");
        };
        assert_eq!(status, 500);
        assert!(messages.iter().any(|m| m == "VM is already created"));
    }

    #[tokio::test]
    async fn the_lifecycle_calls_hit_their_endpoints() {
        let dir = tempfile::tempdir().unwrap();
        let (path, seen) = fake_vmm(
            &dir,
            vec![
                (204, ""),
                (204, ""),
                (204, ""),
                (204, ""),
                (204, ""),
                (200, ""),
            ],
        );
        let c = Client::new(&path, TIMEOUT);
        c.boot().await.unwrap();
        c.power_button().await.unwrap();
        c.shutdown().await.unwrap();
        c.remove_device("install").await.unwrap();
        c.delete().await.unwrap();
        c.shutdown_vmm().await.unwrap();
        let seen = seen.lock().unwrap();
        let paths: Vec<&str> = seen.iter().map(|s| s.1.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "/api/v1/vm.boot",
                "/api/v1/vm.power-button",
                "/api/v1/vm.shutdown",
                "/api/v1/vm.remove-device",
                "/api/v1/vm.delete",
                "/api/v1/vmm.shutdown",
            ]
        );
        assert_eq!(seen[3].2, r#"{"id":"install"}"#);
    }

    /// A VMM that accepts the connection and then says nothing must not hang
    /// the caller (the reason every call has a timeout).
    #[tokio::test]
    async fn a_silent_vmm_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (_conn, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let err = Client::new(&path, Duration::from_millis(200))
            .ping()
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Timeout(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_missing_socket_is_a_transport_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = Client::new(dir.path().join("absent.sock"), TIMEOUT)
            .ping()
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Io(_)), "{err:?}");
    }

    /// With an expected owner set, a socket that does not match is refused
    /// before a single byte is sent (ADR-0061 Decision 6).
    #[tokio::test]
    async fn a_socket_with_the_wrong_owner_is_refused_before_connecting() {
        let dir = tempfile::tempdir().unwrap();
        let (path, seen) = fake_vmm(&dir, vec![(200, "{}")]);
        let wrong = crate::socket::ExpectedSocket {
            uid: u32::MAX - 1,
            gid: u32::MAX - 1,
        };
        let err = Client::new(&path, TIMEOUT)
            .with_expected_owner(wrong)
            .ping()
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Socket(_)), "{err:?}");
        assert!(seen.lock().unwrap().is_empty(), "nothing may be sent");
    }
}
