// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `serve.rs`.

#[cfg(test)]
mod tests {
    use crate::attest::test_support::*;
    use crate::attest::*;
    use rcgen::{PKCS_ECDSA_P256_SHA256, PKCS_ECDSA_P384_SHA384};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
    use x509_parser::prelude::*;

    const TTL: Duration = Duration::from_secs(3600);
    const WINDOW: Duration = Duration::from_secs(60);
    const PIPE: usize = 16 * 1024;

    /// Sends `request`, half-closes, runs the server, returns its reply bytes.
    async fn exchange(
        request: &[u8],
        ca: &Ca,
        limiter: &RateLimiter,
        counters: &Counters,
    ) -> (Result<Outcome, ServeError>, Vec<u8>) {
        let (mut client, mut server): (DuplexStream, DuplexStream) = tokio::io::duplex(PIPE);
        client.write_all(request).await.expect("send");
        client.shutdown().await.expect("half close");
        let outcome = serve_one(&mut server, ca, TEST_SUBJECT, limiter, counters, TTL).await;
        let mut reply = Vec::new();
        client
            .read_to_end(&mut reply)
            .await
            .expect("read to EOF: the server closed");
        (outcome, reply)
    }

    fn fresh() -> (tempfile::TempDir, Ca, RateLimiter, Counters) {
        let (tmp, ca) = test_ca();
        let limiter = RateLimiter::new(SystemClock, 100, WINDOW);
        (tmp, ca, limiter, Counters::default())
    }

    fn snap(issued: u64) -> CounterSnapshot {
        CounterSnapshot {
            issued,
            ..CounterSnapshot::default()
        }
    }

    #[tokio::test]
    async fn issues_one_certificate_and_closes() {
        let (_t, ca, limiter, counters) = fresh();
        let (key, csr) = guest_csr();
        let (outcome, reply) = exchange(csr.as_bytes(), &ca, &limiter, &counters).await;
        assert_eq!(outcome.expect("ok"), Outcome::Issued);
        let pem = String::from_utf8(reply).expect("utf8");
        assert_eq!(pem.matches("BEGIN CERTIFICATE").count(), 1);
        assert!(!pem.contains("REQUEST"));
        let der = cert_der(&pem);
        let (_, cert) = X509Certificate::from_der(&der).expect("parse");
        let ca_der = cert_der(ca.cert_pem());
        let (_, ca_cert) = X509Certificate::from_der(&ca_der).expect("ca");
        cert.verify_signature(Some(ca_cert.public_key()))
            .expect("chain");
        assert_eq!(
            cert.public_key().raw,
            rcgen::PublicKeyData::subject_public_key_info(&key).as_slice()
        );
        assert_eq!(counters.snapshot(), snap(1));
    }

    async fn refused(request: &[u8], want: Outcome, expect: CounterSnapshot) {
        let (_t, ca, limiter, counters) = fresh();
        let (outcome, reply) = exchange(request, &ca, &limiter, &counters).await;
        assert_eq!(outcome.expect("refusal is not an error"), want);
        assert!(reply.is_empty(), "a refusal gets no reply");
        assert_eq!(counters.snapshot(), expect);
    }

    #[tokio::test]
    async fn refuses_oversize_without_reply() {
        let big = vec![b'a'; MAX_CSR_BYTES + 1];
        refused(
            &big,
            Outcome::RefusedSize,
            CounterSnapshot {
                refused_size: 1,
                ..Default::default()
            },
        )
        .await;
    }

    #[tokio::test]
    async fn refuses_a_huge_request_without_buffering_it_all() {
        let huge = vec![b'a'; PIPE - 1];
        refused(
            &huge,
            Outcome::RefusedSize,
            CounterSnapshot {
                refused_size: 1,
                ..Default::default()
            },
        )
        .await;
    }

    #[tokio::test]
    async fn refuses_garbage_and_empty_as_parse() {
        let want = CounterSnapshot {
            refused_parse: 1,
            ..Default::default()
        };
        refused(b"hello", Outcome::RefusedParse, want).await;
        refused(b"", Outcome::RefusedParse, want).await;
    }

    #[tokio::test]
    async fn refuses_wrong_key_type() {
        let (_, der) = guest_csr_der_with(&PKCS_ECDSA_P384_SHA384);
        let pem = pem_wrap("CERTIFICATE REQUEST", &der);
        refused(
            pem.as_bytes(),
            Outcome::RefusedKeyType,
            CounterSnapshot {
                refused_key_type: 1,
                ..Default::default()
            },
        )
        .await;
    }

    #[tokio::test]
    async fn refuses_a_bad_csr_signature() {
        let (_, mut der) = guest_csr_der_with(&PKCS_ECDSA_P256_SHA256);
        let last = der.len() - 1;
        der[last] ^= 1;
        let pem = pem_wrap("CERTIFICATE REQUEST", &der);
        refused(
            pem.as_bytes(),
            Outcome::RefusedSignature,
            CounterSnapshot {
                refused_signature: 1,
                ..Default::default()
            },
        )
        .await;
    }

    #[tokio::test]
    async fn rate_limit_refuses_without_reply_and_counts() {
        let (_t, ca) = test_ca();
        let limiter = RateLimiter::new(SystemClock, 1, WINDOW);
        let counters = Counters::default();
        let (_, csr) = guest_csr();
        let (first, reply1) = exchange(csr.as_bytes(), &ca, &limiter, &counters).await;
        assert_eq!(first.expect("ok"), Outcome::Issued);
        assert!(!reply1.is_empty());
        let (second, reply2) = exchange(csr.as_bytes(), &ca, &limiter, &counters).await;
        assert_eq!(second.expect("ok"), Outcome::RefusedRate);
        assert!(reply2.is_empty());
        assert_eq!(
            counters.snapshot(),
            CounterSnapshot {
                issued: 1,
                refused_rate: 1,
                ..Default::default()
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_that_never_ends_times_out_without_reply() {
        let (_t, ca, limiter, counters) = fresh();
        let (mut client, mut server) = tokio::io::duplex(PIPE);
        // The client sends nothing and never closes.
        let outcome = serve_one(&mut server, &ca, TEST_SUBJECT, &limiter, &counters, TTL).await;
        assert_eq!(outcome.expect("ok"), Outcome::RefusedTimeout);
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).await.expect("server closed");
        assert!(reply.is_empty());
        assert_eq!(
            counters.snapshot(),
            CounterSnapshot {
                refused_timeout: 1,
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn an_out_of_range_validity_is_a_server_error_not_a_reply() {
        let (_t, ca, limiter, counters) = fresh();
        let (mut client, mut server) = tokio::io::duplex(PIPE);
        let (_, csr) = guest_csr();
        client.write_all(csr.as_bytes()).await.expect("send");
        client.shutdown().await.expect("close");
        let got = serve_one(
            &mut server,
            &ca,
            TEST_SUBJECT,
            &limiter,
            &counters,
            Duration::ZERO,
        )
        .await;
        assert!(matches!(got, Err(ServeError::Issue(IssueError::Validity))));
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).await.expect("closed");
        assert!(reply.is_empty());
        assert_eq!(counters.snapshot(), CounterSnapshot::default());
    }

    #[test]
    fn counters_start_at_zero() {
        assert_eq!(Counters::default().snapshot(), CounterSnapshot::default());
    }
}
