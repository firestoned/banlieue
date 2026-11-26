// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Live probe: which key-exchange group does a real TLS peer accept from
//! banlieue's provider (ADR-0170)?
//!
//! Connects to `PQ_PROBE_HOST` (`host` or `host:port`, port 443 by default)
//! with the process provider and the OS trust store, completes the handshake
//! and prints the negotiated group. With `PQ_PROBE_EXPECT` set (for example
//! `X25519MLKEM768`), the test fails unless that group was negotiated.
//!
//! ```sh
//! PQ_PROBE_HOST=bar.foo.io PQ_PROBE_EXPECT=X25519MLKEM768 \
//!   cargo test -p banlieue-tls --test live_pq_probe -- --ignored --nocapture
//! ```
//!
//! An unset `PQ_PROBE_HOST` is a failure, not a skip (`rules/testing.md`).

use std::io::Write as _;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection};
use rustls_platform_verifier::ConfigVerifierExt as _;

const DEFAULT_TLS_PORT: &str = "443";
const IO_TIMEOUT: Duration = Duration::from_secs(15);

#[test]
#[ignore = "live: needs PQ_PROBE_HOST and network access"]
fn negotiated_key_exchange_group() {
    let target = std::env::var("PQ_PROBE_HOST")
        .expect("set PQ_PROBE_HOST, e.g. PQ_PROBE_HOST=bar.foo.io:443");
    let (host, addr) = match target.rsplit_once(':') {
        Some((h, _)) => (h.to_string(), target.clone()),
        None => (target.clone(), format!("{target}:{DEFAULT_TLS_PORT}")),
    };

    let provider = banlieue_tls::install_default_crypto_provider();
    let offered: Vec<String> = provider
        .kx_groups
        .iter()
        .map(|g| format!("{:?}", g.name()))
        .collect();

    let config = ClientConfig::with_platform_verifier().expect("platform verifier");
    let name = ServerName::try_from(host.clone()).expect("a valid server name");
    let mut conn = ClientConnection::new(Arc::new(config), name).expect("client connection");
    let mut sock = TcpStream::connect(&addr).unwrap_or_else(|e| panic!("connect {addr}: {e}"));
    sock.set_read_timeout(Some(IO_TIMEOUT))
        .expect("read timeout");
    sock.set_write_timeout(Some(IO_TIMEOUT))
        .expect("write timeout");

    while conn.is_handshaking() {
        conn.complete_io(&mut sock)
            .unwrap_or_else(|e| panic!("TLS handshake with {addr}: {e}"));
    }
    let _ = sock.flush();

    let group = conn
        .negotiated_key_exchange_group()
        .map(|g| format!("{:?}", g.name()))
        .expect("a negotiated key-exchange group after the handshake");
    let version = conn.protocol_version().map(|v| format!("{v:?}"));
    println!("host={host} offered={offered:?} version={version:?} negotiated={group}");

    if let Ok(expect) = std::env::var("PQ_PROBE_EXPECT") {
        assert_eq!(
            group, expect,
            "{host} negotiated {group}, expected {expect}"
        );
    }
}
