// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A one-shot local HTTP server for the client and credential tests: each
//! canned response answers one connection, in order, and the requests that
//! arrived are handed back for assertions. Same shape as banlieue-azure's
//! and banlieue-proxmox's.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const READ_CHUNK: usize = 4096;
const HEAD_END: &[u8] = b"\r\n\r\n";

/// One request as the server saw it.
pub(crate) struct Recorded {
    pub request_line: String,
    pub headers: String,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    }

    pub fn body_str(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// `METHOD /path?query`.
    pub fn target(&self) -> String {
        self.request_line
            .rsplit_once(' ')
            .map_or_else(|| self.request_line.clone(), |(t, _)| t.to_string())
    }
}

pub(crate) fn install_provider() {
    let _ = banlieue_tls::install_default_crypto_provider();
}

/// A response with `status`, extra `headers` and an XML `body`.
pub(crate) fn respond(status: u16, headers: &[(&str, &str)], body: &str) -> String {
    let mut head = format!("HTTP/1.1 {status} X\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    format!(
        "{head}Content-Type: text/xml;charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub(crate) fn ok(body: &str) -> String {
    respond(200, &[], body)
}

/// Serves each response to one connection, in order; returns the base URL
/// (`http://127.0.0.1:<port>/`) and the requests that arrived. When the
/// responses run out the listener closes, so an unexpected extra request
/// fails to connect.
pub(crate) async fn serve(responses: Vec<String>) -> (String, JoinHandle<Vec<Recorded>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let mut seen = Vec::new();
        for response in responses {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; READ_CHUNK];
            let head_end = loop {
                let n = sock.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break buf.len();
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(HEAD_END.len()).position(|w| w == HEAD_END) {
                    break i + HEAD_END.len();
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
