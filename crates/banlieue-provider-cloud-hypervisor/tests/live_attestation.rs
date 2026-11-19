// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Live test of the guest certificate signer (ADR-0146): a real CA in a
//! temporary directory, the provider's own startup path, a real Unix socket,
//! and a CSR made the way a guest's helper would make one. The certificate
//! that comes back is parsed and checked, not just received.
//!
//! No cluster, no hypervisor and no root: the socket is handed to the test's
//! own uid.
//!
//! ```sh
//! cargo test -p banlieue-provider-cloud-hypervisor --test live_attestation -- --ignored --nocapture
//! ```

use std::str::FromStr;

use banlieue_attestation_ca::AttestationCa;
use banlieue_provider_cloud_hypervisor::attestation::{Signers, load_signer};
use banlieue_provider_cloud_hypervisor::host_config::AttestationSection;
use banlieue_provider_cloud_hypervisor::sys::{effective_gid, effective_uid};
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{DerSignature, Signature, SigningKey, VerifyingKey};
use p256::elliptic_curve::Generate;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use x509_cert::builder::{Builder, RequestBuilder};
use x509_cert::der::{EncodePem, pem::LineEnding};
use x509_cert::name::Name;

const CA_NAME: &str = "host-01";
const MACHINE_UID: &str = "0f3c9a1e-5b7d-4e2a-9c11-3f2b7a5d9e01";
const TTL_SECS: i64 = 3600;

#[tokio::test]
#[ignore = "talks to a real Unix socket and writes a CA to a temp dir; run with --ignored"]
async fn a_guest_csr_over_the_real_socket_returns_a_certificate_the_ca_signed() {
    let tmp = tempfile::tempdir().unwrap();
    let ca_dir = tmp.path().join("attestation-ca");
    AttestationCa::create(&ca_dir, CA_NAME, None).expect("create the CA");

    let section = AttestationSection {
        ca_dir: ca_dir.clone(),
        certificate_ttl_secs: TTL_SECS.unsigned_abs(),
    };
    let signer = load_signer(&section, effective_uid()).expect("the provider's startup path");
    let ca_pem = signer.ca().certificate_pem().to_string();
    let signers = Signers::new(signer);

    let socket = tmp.path().join("vsock.sock_1025");
    signers
        .ensure(MACHINE_UID, &socket, effective_uid(), effective_gid())
        .await
        .expect("start the signer");

    let key = SigningKey::generate();
    let csr = RequestBuilder::new(Name::from_str("CN=whatever-the-guest-says").unwrap())
        .unwrap()
        .build::<_, DerSignature>(&key)
        .unwrap()
        .to_pem(LineEnding::LF)
        .unwrap();
    let mut stream = UnixStream::connect(&socket).await.expect("connect");
    stream.write_all(csr.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();

    let (_, pem) = x509_parser::pem::parse_x509_pem(reply.as_bytes()).expect("a PEM reply");
    let cert = pem.parse_x509().expect("a certificate");
    assert_eq!(cert.subject().to_string(), format!("CN={MACHINE_UID}"));
    assert_eq!(cert.issuer().to_string(), format!("CN={CA_NAME}"));
    let basic = cert.basic_constraints().unwrap().expect("basicConstraints");
    assert!(!basic.value.ca, "a guest certificate is CA:FALSE");
    let validity = cert.validity();
    assert_eq!(
        validity.not_after.timestamp() - validity.not_before.timestamp(),
        TTL_SECS
    );

    let (_, ca_der) = x509_parser::pem::parse_x509_pem(ca_pem.as_bytes()).unwrap();
    let ca = ca_der.parse_x509().unwrap();
    let ca_key = VerifyingKey::from_sec1_bytes(&ca.public_key().subject_public_key.data)
        .expect("the CA's P-256 key");
    let signature = Signature::from_der(&cert.signature_value.data).expect("a DER signature");
    ca_key
        .verify(cert.tbs_certificate.as_ref(), &signature)
        .expect("signed by the host CA");

    // The subject came from the machine, not the CSR; the counters say so
    // without any request content.
    let counts = signers.counts(MACHINE_UID).expect("counters");
    assert_eq!(counts.issued, 1, "{counts:?}");

    signers.stop(MACHINE_UID, &socket);
    assert!(!socket.exists());
}
