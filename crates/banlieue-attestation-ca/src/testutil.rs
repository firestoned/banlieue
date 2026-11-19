// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Test fixtures: CSRs the way a guest would make them, and ones a hostile
//! guest would. Test-only.

use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use p256::ecdsa::{DerSignature, SigningKey};
use p256::elliptic_curve::Generate;
use x509_cert::builder::{Builder, RequestBuilder};
use x509_cert::der::asn1::Ia5String;
use x509_cert::der::{DecodePem, Encode, EncodePem, pem::LineEnding};
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{BasicConstraints, ExtendedKeyUsage, SubjectAltName};
use x509_cert::name::Name;
use x509_cert::request::CertReq;

use crate::{AttestationCa, RateLimit, Signer};

pub(crate) const RSA_CSR: &str = include_str!("testdata/rsa2048.csr.pem");
pub(crate) const P384_CSR: &str = include_str!("testdata/p384.csr.pem");
pub(crate) const ED25519_CSR: &str = include_str!("testdata/ed25519.csr.pem");

/// A fixed "now" well inside the validity of anything we issue.
pub(crate) const NOW_SECS: u64 = 1_800_000_000;
pub(crate) const TTL: Duration = Duration::from_secs(3600);
pub(crate) const SUBJECT: &str = "0b6f9f0e-2c64-4c61-9f43-0a1b2c3d4e5f";

pub(crate) fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS)
}

pub(crate) fn new_key() -> SigningKey {
    SigningKey::generate()
}

/// A well-formed CSR for `key` with the given subject string.
pub(crate) fn csr_for(key: &SigningKey, subject: &str) -> String {
    let builder = RequestBuilder::new(Name::from_str(subject).unwrap()).unwrap();
    builder
        .build::<_, DerSignature>(key)
        .unwrap()
        .to_pem(LineEnding::LF)
        .unwrap()
}

pub(crate) fn good_csr() -> (String, SigningKey) {
    let key = new_key();
    (csr_for(&key, "CN=guest"), key)
}

/// A CSR that asks for everything: a hostile subject, a SAN, CA:TRUE, an EKU.
pub(crate) fn greedy_csr(key: &SigningKey) -> String {
    let mut b = RequestBuilder::new(Name::from_str("CN=root,O=Evil Corp,C=US").unwrap()).unwrap();
    b.add_extension(&SubjectAltName(vec![GeneralName::DnsName(
        Ia5String::new("evil.example.com").unwrap(),
    )]))
    .unwrap();
    b.add_extension(&BasicConstraints {
        ca: true,
        path_len_constraint: None,
    })
    .unwrap();
    b.add_extension(&ExtendedKeyUsage(vec![const_oid_server_auth()]))
        .unwrap();
    b.build::<_, DerSignature>(key)
        .unwrap()
        .to_pem(LineEnding::LF)
        .unwrap()
}

fn const_oid_server_auth() -> x509_cert::der::oid::ObjectIdentifier {
    x509_cert::der::oid::ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.1")
}

/// A CSR whose signature is valid for a different key: tamper the request body.
pub(crate) fn tampered_csr(key: &SigningKey) -> String {
    let pem = csr_for(key, "CN=guest");
    let mut req = CertReq::from_pem(pem.as_bytes()).unwrap();
    // Swap in another key's public key; the signature no longer matches.
    let other = new_key();
    req.info.public_key =
        x509_cert::spki::SubjectPublicKeyInfo::from_key(other.verifying_key()).unwrap();
    let der = req.to_der().unwrap();
    CertReq::try_from(der.as_slice())
        .unwrap()
        .to_pem(LineEnding::LF)
        .unwrap()
}

pub(crate) fn create_ca(dir: &Path, name: &str) -> AttestationCa {
    AttestationCa::create(dir, name, None).unwrap()
}

pub(crate) fn signer_in(dir: &Path) -> Signer {
    let ca = create_ca(&dir.join("ca"), "host-01");
    Signer::new(ca, TTL, RateLimit::default()).unwrap()
}

/// A CSR for a P-256 point labelled as a P-384 key: only the curve parameter is wrong.
pub(crate) fn wrong_curve_csr(key: &SigningKey) -> String {
    let pem = csr_for(key, "CN=guest");
    let mut req = CertReq::from_pem(pem.as_bytes()).unwrap();
    let p384 = x509_cert::der::oid::ObjectIdentifier::new_unwrap("1.3.132.0.34");
    req.info.public_key.algorithm.parameters =
        Some(x509_cert::der::Any::encode_from(&p384).unwrap());
    req.to_pem(LineEnding::LF).unwrap()
}
