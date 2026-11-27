// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared fixtures for the unit tests of this module.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::SystemTime;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose,
    PKCS_ECDSA_P256_SHA256, SignatureAlgorithm,
};

use super::ca::Ca;

/// Name of the CA the tests create.
pub const TEST_CA_NAME: &str = "bar.foo.io";
/// Subject the tests issue for.
pub const TEST_SUBJECT: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
/// PEM line width.
const PEM_LINE: usize = 64;

/// A CA in a fresh temporary directory; keep the guard alive.
pub fn test_ca() -> (tempfile::TempDir, Ca) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("attestation-ca");
    let ca = Ca::create(&dir, TEST_CA_NAME, SystemTime::now()).expect("create CA");
    (tmp, ca)
}

/// A CA directory as `Ca::create` wrote it before the path length constraint:
/// no path length, `keyCertSign` and `cRLSign`. Returns the guard and the
/// directory; keep the guard alive.
pub fn legacy_unconstrained_ca() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("attestation-ca");
    fs::create_dir(&dir).expect("mkdir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("chmod dir");

    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("key");
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, TEST_CA_NAME);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let cert = params.self_signed(&key).expect("self-sign");

    let key_path = dir.join("ca.key");
    fs::write(&key_path, key.serialize_pem()).expect("write key");
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).expect("chmod key");
    fs::write(dir.join("ca.crt"), cert.pem()).expect("write cert");
    (tmp, dir)
}

/// Wraps DER in PEM with `label`.
pub fn pem_wrap(label: &str, der: &[u8]) -> String {
    let b64 = STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in b64.as_bytes().chunks(PEM_LINE) {
        out.push_str(std::str::from_utf8(chunk).expect("ascii"));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// A guest key of `alg` and its CSR (DER).
pub fn guest_csr_der_with(alg: &'static SignatureAlgorithm) -> (KeyPair, Vec<u8>) {
    let key = KeyPair::generate_for(alg).expect("key");
    let csr = CertificateParams::new(Vec::<String>::new())
        .expect("params")
        .serialize_request(&key)
        .expect("csr");
    (key, csr.der().to_vec())
}

/// A P-256 guest key and its CSR as PEM.
pub fn guest_csr() -> (KeyPair, String) {
    let (key, der) = guest_csr_der_with(&PKCS_ECDSA_P256_SHA256);
    (key, pem_wrap("CERTIFICATE REQUEST", &der))
}

/// The DER of the first certificate in `pem`.
pub fn cert_der(pem: &str) -> Vec<u8> {
    let (_, p) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).expect("pem");
    p.contents
}
