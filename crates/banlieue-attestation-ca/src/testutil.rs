// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Test fixtures: CSRs the way a guest would make them, and ones a hostile
//! guest would. Test-only, and the `testing` feature's, which uses only a
//! part of it.
#![allow(dead_code)]

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

/// What an upstream PKI put in the intermediate it signed for a host: the
/// good defaults, and the knobs a test turns to make one wrong.
pub(crate) struct UpstreamOpts {
    pub ca: bool,
    pub path_len: Option<u8>,
    pub key_cert_sign: bool,
    pub not_before: SystemTime,
    pub not_after: SystemTime,
    /// The subject CN, when it is not the request's.
    pub subject_cn: Option<String>,
    /// A key to certify instead of the request's.
    pub certify: Option<SigningKey>,
}

impl UpstreamOpts {
    pub(crate) fn good() -> Self {
        Self {
            ca: true,
            path_len: Some(0),
            key_cert_sign: true,
            not_before: now() - Duration::from_secs(3600),
            not_after: now() + Duration::from_secs(365 * 86_400),
            subject_cn: None,
            certify: None,
        }
    }
}

struct UpstreamProfile {
    issuer: Name,
    subject: Name,
    opts_ca: bool,
    path_len: Option<u8>,
    key_cert_sign: bool,
}

impl x509_cert::builder::profile::BuilderProfile for UpstreamProfile {
    fn get_issuer(&self, _subject: &Name) -> Name {
        self.issuer.clone()
    }

    fn get_subject(&self) -> Name {
        self.subject.clone()
    }

    fn build_extensions(
        &self,
        _spk: x509_cert::spki::SubjectPublicKeyInfoRef<'_>,
        _issuer_spk: x509_cert::spki::SubjectPublicKeyInfoRef<'_>,
        tbs: &x509_cert::certificate::TbsCertificate,
    ) -> x509_cert::builder::Result<Vec<x509_cert::ext::Extension>> {
        use x509_cert::ext::ToExtension;
        use x509_cert::ext::pkix::{KeyUsage, KeyUsages};
        let mut exts = Vec::new();
        let basic = BasicConstraints {
            ca: self.opts_ca,
            path_len_constraint: self.path_len,
        };
        exts.push((true, &basic).to_extension(tbs.subject(), &exts)?);
        let usage = if self.key_cert_sign {
            KeyUsage(KeyUsages::KeyCertSign.into())
        } else {
            KeyUsage(KeyUsages::DigitalSignature.into())
        };
        exts.push((true, &usage).to_extension(tbs.subject(), &exts)?);
        Ok(exts)
    }
}

/// Sign `csr_pem` the way an upstream PKI's `sign-intermediate` would:
/// issuer `issuer_cn` under `issuer_key`, shaped by `opts`. Returns one PEM
/// certificate.
pub(crate) fn upstream_sign(
    csr_pem: &str,
    issuer_key: &SigningKey,
    issuer_cn: &str,
    opts: &UpstreamOpts,
) -> String {
    use x509_cert::builder::CertificateBuilder;
    use x509_cert::serial_number::SerialNumber;
    use x509_cert::time::{Time, Validity};
    let req = CertReq::from_pem(csr_pem.as_bytes()).unwrap();
    let spki = match &opts.certify {
        Some(k) => x509_cert::spki::SubjectPublicKeyInfo::from_key(k.verifying_key()).unwrap(),
        None => req.info.public_key.clone(),
    };
    let subject = match &opts.subject_cn {
        Some(cn) => Name::from_str(&format!("CN={cn}")).unwrap(),
        None => req.info.subject.clone(),
    };
    let profile = UpstreamProfile {
        issuer: Name::from_str(&format!("CN={issuer_cn}")).unwrap(),
        subject,
        opts_ca: opts.ca,
        path_len: opts.path_len,
        key_cert_sign: opts.key_cert_sign,
    };
    let validity = Validity::new(
        Time::try_from(opts.not_before).unwrap(),
        Time::try_from(opts.not_after).unwrap(),
    );
    let serial = SerialNumber::new(&[0x42, 0x17]).unwrap();
    CertificateBuilder::new(profile, serial, validity, spki)
        .unwrap()
        .build::<_, DerSignature>(issuer_key)
        .unwrap()
        .to_pem(LineEnding::LF)
        .unwrap()
}

/// A self-signed upstream root: its key and its PEM certificate.
pub(crate) fn upstream_root(cn: &str) -> (SigningKey, String) {
    let key = new_key();
    let csr = csr_for(&key, &format!("CN={cn}"));
    let opts = UpstreamOpts {
        path_len: None,
        ..UpstreamOpts::good()
    };
    let pem = upstream_sign(&csr, &key, cn, &opts);
    (key, pem)
}

/// A subordinate upstream CA signed by `parent`: its key and PEM certificate.
pub(crate) fn upstream_intermediate(
    cn: &str,
    parent_key: &SigningKey,
    parent_cn: &str,
) -> (SigningKey, String) {
    let key = new_key();
    let csr = csr_for(&key, &format!("CN={cn}"));
    let opts = UpstreamOpts {
        path_len: Some(1),
        ..UpstreamOpts::good()
    };
    (key, upstream_sign(&csr, parent_key, parent_cn, &opts))
}
