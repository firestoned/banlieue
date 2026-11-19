// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use x509_parser::oid_registry::{OID_X509_EXT_BASIC_CONSTRAINTS, OID_X509_EXT_KEY_USAGE};
use x509_parser::prelude::*;

use super::*;
use crate::testutil::*;

fn issue(signer: &Signer, csr: &str) -> String {
    signer.sign(csr.as_bytes(), SUBJECT, now()).unwrap()
}

fn err(signer: &Signer, csr: &[u8]) -> SignError {
    signer.sign(csr, SUBJECT, now()).err().unwrap()
}

fn with_cert<R>(pem: &str, f: impl FnOnce(&X509Certificate<'_>) -> R) -> R {
    let (_, p) = parse_x509_pem(pem.as_bytes()).unwrap();
    f(&p.parse_x509().unwrap())
}

/// Verify the issued certificate's signature with the CA key, independently of the builder.
fn assert_signed_by(signer: &Signer, issued: &str) {
    let ca_pem = signer.ca().certificate_pem().to_owned();
    let ca_key = with_cert(&ca_pem, |ca| {
        VerifyingKey::from_sec1_bytes(ca.public_key().subject_public_key.data.as_ref()).unwrap()
    });
    let (_, p) = parse_x509_pem(issued.as_bytes()).unwrap();
    let cert = p.parse_x509().unwrap();
    let sig = Signature::from_der(cert.signature_value.data.as_ref()).unwrap();
    ca_key.verify(cert.tbs_certificate.as_ref(), &sig).unwrap();
}

#[test]
fn issues_a_certificate_with_exactly_the_specified_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let signer = signer_in(tmp.path());
    let (csr, key) = good_csr();
    let pem = issue(&signer, &csr);
    with_cert(&pem, |c| {
        assert_eq!(c.version(), X509Version::V3);
        assert_eq!(c.subject().to_string(), format!("CN={SUBJECT}"));
        assert_eq!(c.issuer().to_string(), "CN=host-01");
        let bc = c.basic_constraints().unwrap().unwrap();
        assert!(bc.critical, "basicConstraints must be critical");
        assert!(!bc.value.ca, "CA:FALSE");
        let ku = c.key_usage().unwrap().unwrap();
        assert!(ku.critical);
        assert_eq!(ku.value.flags, 1, "digitalSignature only");
        assert!(c.subject_alternative_name().unwrap().is_none());
        assert!(c.extended_key_usage().unwrap().is_none());
        let oids: Vec<_> = c.extensions().iter().map(|e| e.oid.clone()).collect();
        assert_eq!(oids.len(), 2, "{oids:?}");
        assert!(oids.contains(&OID_X509_EXT_BASIC_CONSTRAINTS));
        assert!(oids.contains(&OID_X509_EXT_KEY_USAGE));
        let nb = c.validity().not_before.timestamp();
        let na = c.validity().not_after.timestamp();
        assert_eq!(nb, NOW_SECS as i64);
        assert_eq!(na - nb, TTL.as_secs() as i64);
        let expected_spki = key.verifying_key().to_sec1_point(false);
        assert_eq!(
            c.public_key().subject_public_key.data.as_ref(),
            expected_spki.as_bytes()
        );
    });
    assert_signed_by(&signer, &pem);
}

#[test]
fn serial_is_random_positive_and_distinct() {
    let tmp = tempfile::tempdir().unwrap();
    let signer = signer_in(tmp.path());
    let (csr, _) = good_csr();
    let a = issue(&signer, &csr);
    let b = issue(&signer, &csr);
    let (sa, sb) = (
        with_cert(&a, |c| c.raw_serial().to_vec()),
        with_cert(&b, |c| c.raw_serial().to_vec()),
    );
    assert_ne!(sa, sb);
    for s in [&sa, &sb] {
        assert!(s.iter().any(|b| *b != 0));
        assert!(s.len() >= 15 && s.len() <= 17, "{} bytes", s.len());
    }
    // Positive: the DER integer's first content byte never has the high bit without a pad.
    with_cert(&a, |c| {
        assert!(c.serial > x509_parser::num_bigint::BigUint::from(0u8))
    });
}

#[test]
fn csr_subject_and_extensions_are_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let signer = signer_in(tmp.path());
    let key = new_key();
    let pem = issue(&signer, &greedy_csr(&key));
    with_cert(&pem, |c| {
        assert_eq!(c.subject().to_string(), format!("CN={SUBJECT}"));
        assert!(!c.basic_constraints().unwrap().unwrap().value.ca);
        assert!(c.subject_alternative_name().unwrap().is_none());
        assert!(c.extended_key_usage().unwrap().is_none());
        assert_eq!(c.extensions().len(), 2);
        assert!(!pem.contains("evil"));
    });
}

#[test]
fn subject_comes_from_the_argument_not_the_csr() {
    let tmp = tempfile::tempdir().unwrap();
    let signer = signer_in(tmp.path());
    let (csr, _) = good_csr();
    let pem = signer.sign(csr.as_bytes(), "another-uid", now()).unwrap();
    with_cert(&pem, |c| {
        assert_eq!(c.subject().to_string(), "CN=another-uid")
    });
}

#[test]
fn refuses_a_subject_outside_the_alphabet() {
    let tmp = tempfile::tempdir().unwrap();
    let signer = signer_in(tmp.path());
    let (csr, _) = good_csr();
    for bad in ["", "a,b", "CN=x", "a b", "x+y", "é"] {
        assert_eq!(
            signer.sign(csr.as_bytes(), bad, now()).err().unwrap(),
            SignError::InvalidSubject,
            "{bad:?}"
        );
    }
}

#[test]
fn size_cap_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let signer = signer_in(tmp.path());
    let (csr, _) = good_csr();
    // A real CSR is far below the cap and is signed.
    assert!(csr.len() < MAX_CSR_BYTES / 2);
    assert!(signer.sign(csr.as_bytes(), SUBJECT, now()).is_ok());
    // Exactly at the cap is not "too large": it is read (and here is not a CSR).
    assert_eq!(
        err(&signer, &vec![b'A'; MAX_CSR_BYTES]),
        SignError::Malformed
    );
    // One byte more is refused for size, before anything is parsed.
    assert_eq!(
        err(&signer, &vec![b'A'; MAX_CSR_BYTES + 1]),
        SignError::TooLarge
    );
    assert_eq!(
        err(&signer, &vec![b'A'; MAX_CSR_BYTES * 4]),
        SignError::TooLarge
    );
}

#[test]
fn refuses_rsa_p384_and_ed25519_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let signer = signer_in(tmp.path());
    assert_eq!(err(&signer, RSA_CSR.as_bytes()), SignError::BadKeyType);
    assert_eq!(err(&signer, P384_CSR.as_bytes()), SignError::BadKeyType);
    assert_eq!(err(&signer, ED25519_CSR.as_bytes()), SignError::BadKeyType);
}

#[test]
fn refuses_an_ec_key_whose_curve_is_not_p256() {
    let tmp = tempfile::tempdir().unwrap();
    let signer = signer_in(tmp.path());
    let key = new_key();
    assert_eq!(
        err(&signer, wrong_curve_csr(&key).as_bytes()),
        SignError::BadKeyType
    );
}

#[test]
fn accepts_p256_where_the_other_key_types_fail() {
    let tmp = tempfile::tempdir().unwrap();
    let signer = signer_in(tmp.path());
    let (csr, _) = good_csr();
    assert!(signer.sign(csr.as_bytes(), SUBJECT, now()).is_ok());
}

#[test]
fn refuses_a_csr_whose_self_signature_does_not_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let signer = signer_in(tmp.path());
    let key = new_key();
    assert_eq!(
        err(&signer, tampered_csr(&key).as_bytes()),
        SignError::BadSignature
    );
}

#[test]
fn refuses_malformed_input() {
    let tmp = tempfile::tempdir().unwrap();
    let signer = signer_in(tmp.path());
    let (csr, _) = good_csr();
    let truncated = &csr.as_bytes()[..csr.len() / 2];
    for bad in [
        &b""[..],
        b"hello",
        b"-----BEGIN CERTIFICATE REQUEST-----\nAAAA\n-----END CERTIFICATE REQUEST-----\n",
        truncated,
        &[0xff, 0xfe, 0x00][..],
    ] {
        assert_eq!(err(&signer, bad), SignError::Malformed, "{bad:?}");
    }
    // A certificate is not a CSR.
    let cert = signer.ca().certificate_pem().as_bytes().to_vec();
    assert_eq!(err(&signer, &cert), SignError::Malformed);
}

#[test]
fn rate_limit_is_per_subject_with_an_injected_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let ca = create_ca(&tmp.path().join("ca"), "host-01");
    let limit = RateLimit {
        max_requests: 2,
        window: Duration::from_secs(60),
    };
    let signer = Signer::new(ca, TTL, limit).unwrap();
    let (csr, _) = good_csr();
    let t = now();
    assert!(signer.sign(csr.as_bytes(), "a", t).is_ok());
    assert!(signer.sign(csr.as_bytes(), "a", t).is_ok());
    assert_eq!(
        signer.sign(csr.as_bytes(), "a", t).err().unwrap(),
        SignError::RateLimited
    );
    assert!(signer.sign(csr.as_bytes(), "b", t).is_ok());
    assert!(
        signer
            .sign(csr.as_bytes(), "a", t + Duration::from_secs(61))
            .is_ok()
    );
}

#[test]
fn ttl_bounds_are_enforced() {
    let tmp = tempfile::tempdir().unwrap();
    for (i, (ttl, ok)) in [
        (MIN_CERTIFICATE_TTL - Duration::from_secs(1), false),
        (MIN_CERTIFICATE_TTL, true),
        (MAX_CERTIFICATE_TTL, true),
        (MAX_CERTIFICATE_TTL + Duration::from_secs(1), false),
        (Duration::ZERO, false),
    ]
    .into_iter()
    .enumerate()
    {
        let ca = create_ca(&tmp.path().join(format!("ca{i}")), "host-01");
        let r = Signer::new(ca, ttl, RateLimit::default());
        assert_eq!(r.is_ok(), ok, "{ttl:?}");
        if !ok {
            assert_eq!(r.err().unwrap(), SignerConfigError::TtlOutOfRange);
        }
    }
}

#[test]
fn a_degenerate_rate_limit_is_refused_at_construction() {
    let tmp = tempfile::tempdir().unwrap();
    let ca = create_ca(&tmp.path().join("ca"), "host-01");
    let r = Signer::new(
        ca,
        TTL,
        RateLimit {
            max_requests: 0,
            window: Duration::from_secs(1),
        },
    );
    assert_eq!(r.err().unwrap(), SignerConfigError::RateLimitInvalid);
}

#[test]
fn errors_and_signer_debug_never_contain_pem() {
    let tmp = tempfile::tempdir().unwrap();
    let signer = signer_in(tmp.path());
    let all = [
        SignError::TooLarge,
        SignError::Malformed,
        SignError::BadKeyType,
        SignError::BadSignature,
        SignError::RateLimited,
        SignError::InvalidSubject,
        SignError::Internal,
    ];
    for e in all {
        for s in [format!("{e}"), format!("{e:?}")] {
            assert!(!s.contains("-----") && !s.contains("MII"), "{s}");
        }
    }
    let shown = format!("{signer:?}");
    assert!(
        !shown.contains("-----") && !shown.contains("MII"),
        "{shown}"
    );
}
