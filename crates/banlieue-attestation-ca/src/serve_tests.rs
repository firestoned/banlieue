// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
use std::path::PathBuf;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use x509_parser::prelude::*;

use super::*;
use crate::testutil::*;
use crate::{MAX_CSR_BYTES, RateLimit};

const WAIT: Duration = Duration::from_secs(5);

struct Rig {
    _tmp: tempfile::TempDir,
    sock: PathBuf,
    outcomes: Arc<Outcomes>,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

fn rig_with(limit: RateLimit, config: ServeConfig) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let ca = create_ca(&tmp.path().join("ca"), "host-01");
    let signer = Arc::new(Signer::new(ca, TTL, limit).unwrap());
    let sock = tmp.path().join("vsock.sock_1025");
    let listener = UnixListener::bind(&sock).unwrap();
    let outcomes = Arc::new(Outcomes::new());
    let cancel = CancellationToken::new();
    let task = tokio::spawn(serve(
        listener,
        signer,
        SUBJECT.to_owned(),
        outcomes.clone(),
        config,
        cancel.clone(),
    ));
    Rig {
        _tmp: tmp,
        sock,
        outcomes,
        cancel,
        task,
    }
}

fn rig() -> Rig {
    rig_with(RateLimit::default(), ServeConfig::default())
}

/// One request the way the guest does it: write, half-close, read to EOF.
async fn ask(sock: &PathBuf, request: &[u8]) -> Vec<u8> {
    let mut s = UnixStream::connect(sock).await.unwrap();
    // The server may close early on an oversize request; ignore a broken pipe.
    let _ = s.write_all(request).await;
    let _ = s.shutdown().await;
    let mut out = Vec::new();
    tokio::time::timeout(WAIT, s.read_to_end(&mut out))
        .await
        .unwrap()
        .ok();
    out
}

async fn wait_for(rig: &Rig, pred: impl Fn(OutcomeCounts) -> bool) {
    tokio::time::timeout(WAIT, async {
        while !pred(rig.outcomes.snapshot()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("counter never reached the expected value");
}

#[tokio::test]
async fn answers_one_pem_certificate_for_the_configured_subject() {
    let r = rig();
    let (csr, key) = good_csr();
    let reply = ask(&r.sock, csr.as_bytes()).await;
    let (_, pem) = parse_x509_pem(&reply).expect("one PEM certificate");
    let cert = pem.parse_x509().unwrap();
    assert_eq!(cert.subject().to_string(), format!("CN={SUBJECT}"));
    assert_eq!(
        cert.public_key().subject_public_key.data.as_ref(),
        key.verifying_key().to_sec1_point(false).as_bytes()
    );
    assert_eq!(reply.iter().filter(|b| **b == b'-').count() % 2, 0);
    wait_for(&r, |c| c.issued == 1).await;
    assert_eq!(
        r.outcomes.snapshot(),
        OutcomeCounts {
            issued: 1,
            ..OutcomeCounts::default()
        }
    );
}

#[tokio::test]
async fn refusals_get_no_reply_and_are_counted() {
    let r = rig();
    assert!(ask(&r.sock, b"garbage").await.is_empty());
    wait_for(&r, |c| c.malformed == 1).await;

    assert!(ask(&r.sock, RSA_CSR.as_bytes()).await.is_empty());
    wait_for(&r, |c| c.bad_key_type == 1).await;

    let key = new_key();
    assert!(ask(&r.sock, tampered_csr(&key).as_bytes()).await.is_empty());
    wait_for(&r, |c| c.bad_signature == 1).await;

    assert!(
        ask(&r.sock, &vec![b'A'; MAX_CSR_BYTES + 1])
            .await
            .is_empty()
    );
    wait_for(&r, |c| c.too_large == 1).await;

    assert!(ask(&r.sock, b"").await.is_empty());
    wait_for(&r, |c| c.malformed == 2).await;
    assert_eq!(r.outcomes.snapshot().issued, 0);
}

#[tokio::test]
async fn a_huge_request_is_cut_off_not_buffered() {
    let r = rig();
    let mut s = UnixStream::connect(&r.sock).await.unwrap();
    let chunk = vec![b'A'; 64 * 1024];
    // The server stops reading at the cap + 1 and closes; a writer that keeps
    // going must hit a closed pipe rather than succeed forever.
    let mut failed = false;
    for _ in 0..256 {
        if s.write_all(&chunk).await.is_err() {
            failed = true;
            break;
        }
    }
    assert!(failed, "server kept reading an unbounded request");
    wait_for(&r, |c| c.too_large == 1).await;
}

#[tokio::test]
async fn rate_limit_refuses_with_no_reply_and_counts() {
    let r = rig_with(
        RateLimit {
            max_requests: 1,
            window: Duration::from_secs(3600),
        },
        ServeConfig::default(),
    );
    let (csr, _) = good_csr();
    assert!(!ask(&r.sock, csr.as_bytes()).await.is_empty());
    assert!(ask(&r.sock, csr.as_bytes()).await.is_empty());
    wait_for(&r, |c| c.rate_limited == 1).await;
    assert_eq!(r.outcomes.snapshot().issued, 1);
}

#[tokio::test]
async fn a_client_that_never_finishes_times_out_and_does_not_block_others() {
    let r = rig_with(
        RateLimit::default(),
        ServeConfig {
            read_timeout: Duration::from_millis(200),
            ..ServeConfig::default()
        },
    );
    let mut stalled = UnixStream::connect(&r.sock).await.unwrap();
    stalled.write_all(b"-----BEGIN").await.unwrap();
    // Another guest connection is served while the first dawdles.
    let (csr, _) = good_csr();
    assert!(!ask(&r.sock, csr.as_bytes()).await.is_empty());
    // The stalled one is closed with no reply and counted as malformed.
    let mut out = Vec::new();
    tokio::time::timeout(WAIT, stalled.read_to_end(&mut out))
        .await
        .unwrap()
        .ok();
    assert!(out.is_empty());
    wait_for(&r, |c| c.malformed == 1).await;
}

#[tokio::test]
async fn cancellation_stops_the_loop() {
    let r = rig();
    r.cancel.cancel();
    tokio::time::timeout(WAIT, r.task)
        .await
        .expect("serve returns after cancel")
        .unwrap();
}

#[tokio::test]
async fn nothing_in_the_counters_or_errors_is_a_pem_body() {
    let r = rig();
    let (csr, _) = good_csr();
    let _ = ask(&r.sock, csr.as_bytes()).await;
    let _ = ask(&r.sock, b"garbage").await;
    wait_for(&r, |c| c.issued == 1 && c.malformed == 1).await;
    let shown = format!("{:?}", r.outcomes.snapshot());
    let shown_outcomes = format!("{:?}", r.outcomes);
    for s in [shown, shown_outcomes] {
        assert!(!s.contains("-----") && !s.contains("MII"), "{s}");
    }
}

#[test]
fn default_config_is_bounded() {
    let c = ServeConfig::default();
    assert!(c.max_concurrent >= 1 && !c.read_timeout.is_zero());
}
