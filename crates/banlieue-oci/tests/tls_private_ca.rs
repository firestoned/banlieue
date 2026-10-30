// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A registry served by a private CA (ADR-0110), against a real TLS handshake.
//!
//! The unit tests in `tls_tests.rs` prove the PEM handling. This proves the
//! thing that failed in practice: a client built without the CA rejects a
//! server whose certificate it signed (`UnknownIssuer`), and a client built
//! from the CA's bundle accepts it. The CA and the server certificate are
//! generated here and never written anywhere but a temp dir, so no key is
//! committed. Needs no network: the server listens on loopback.

use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::ring::default_provider;

const HOST: &str = "localhost";
const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
const READ_BUFFER_BYTES: usize = 4096;

struct Pki {
    /// The CA certificate, PEM: what the operator would put in the bundle.
    ca_pem: String,
    server_cert: CertificateDer<'static>,
    server_key: PrivateKeyDer<'static>,
}

fn pki() -> Pki {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);

    let server_key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec![HOST.to_string()]).unwrap();
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_cert = params.signed_by(&server_key, &issuer).unwrap();

    Pki {
        ca_pem: ca_cert.pem(),
        server_cert: server_cert.der().clone(),
        server_key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der())),
    }
}

/// Serve `RESPONSE` over TLS on a loopback port until the test ends.
async fn serve(pki: &Pki) -> u16 {
    let config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![pki.server_cert.clone()], pki.server_key.clone_key())
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind((HOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                // A handshake the client abandons is the expected case for
                // the client that does not trust us.
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut buf = [0u8; READ_BUFFER_BYTES];
                let _ = tls.read(&mut buf).await;
                let _ = tls.write_all(RESPONSE).await;
                let _ = tls.shutdown().await;
            });
        }
    });
    port
}

#[tokio::test]
async fn a_private_ca_is_rejected_without_its_bundle_and_trusted_with_it() {
    banlieue_oci::install_crypto_provider();
    let pki = pki();
    let port = serve(&pki).await;
    let url = format!("https://{HOST}:{port}/v2/");

    // The failure from the field: the issuer is not a public root.
    let untrusting = banlieue_oci::tls::http_client(None).unwrap();
    let err = untrusting.get(&url).send().await.unwrap_err();
    assert!(
        err.is_connect(),
        "expected a connect-time TLS failure: {err:?}"
    );

    // The same server, once the operator's CA is named.
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("ca.crt");
    std::fs::write(&bundle, &pki.ca_pem).unwrap();
    let trusting = banlieue_oci::tls::http_client(Some(&bundle)).unwrap();
    let resp = trusting.get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

/// Adding a CA adds trust for that CA only: a server signed by a different
/// private CA is still refused.
#[tokio::test]
async fn a_bundle_trusts_its_own_ca_and_no_other() {
    banlieue_oci::install_crypto_provider();
    let server_pki = pki();
    let other_pki = pki();
    let port = serve(&server_pki).await;
    let url = format!("https://{HOST}:{port}/v2/");

    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("other-ca.crt");
    std::fs::write(&bundle, &other_pki.ca_pem).unwrap();
    let client = banlieue_oci::tls::http_client(Some(&bundle)).unwrap();
    let err = client.get(&url).send().await.unwrap_err();
    assert!(
        err.is_connect(),
        "expected a connect-time TLS failure: {err:?}"
    );
}
