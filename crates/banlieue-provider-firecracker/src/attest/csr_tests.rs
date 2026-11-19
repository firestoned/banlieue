// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `csr.rs`.

#[cfg(test)]
mod tests {
    use crate::attest::test_support::*;
    use crate::attest::*;
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose,
        PKCS_ECDSA_P256_SHA256, PKCS_ECDSA_P384_SHA384, PKCS_ED25519, PublicKeyData, SanType,
    };

    const LABEL: &str = "CERTIFICATE REQUEST";

    #[test]
    fn returns_exactly_the_guests_public_key() {
        let (key, pem) = guest_csr();
        let got = parse_csr(pem.as_bytes()).expect("parse");
        assert_eq!(got.as_der(), key.subject_public_key_info().as_slice());
    }

    #[test]
    fn discards_subject_and_extensions() {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params
            .distinguished_name
            .push(DnType::CommonName, "attacker");
        params.subject_alt_names = vec![SanType::DnsName(
            "evil.example.com".try_into().expect("ia5"),
        )];
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let csr = params.serialize_request(&key).expect("csr");
        let got = parse_csr(csr.pem().expect("pem").as_bytes()).expect("parse");
        // The type carries only SPKI DER: nothing else can leak out of it.
        assert_eq!(got.as_der(), key.subject_public_key_info().as_slice());
    }

    #[test]
    fn refuses_empty() {
        assert_eq!(parse_csr(b""), Err(CsrError::Parse));
        assert_eq!(parse_csr(b"\n\n"), Err(CsrError::Parse));
    }

    #[test]
    fn size_limit_is_inclusive() {
        let (_, pem) = guest_csr();
        let mut ok = pem.into_bytes();
        ok.resize(MAX_CSR_BYTES, b'\n');
        assert!(
            parse_csr(&ok).is_ok(),
            "exactly the limit, valid, is accepted"
        );
        ok.push(b'\n');
        assert_eq!(parse_csr(&ok), Err(CsrError::TooLarge));
    }

    #[test]
    fn refuses_garbage() {
        assert_eq!(parse_csr(b"not a pem at all"), Err(CsrError::Parse));
        assert_eq!(parse_csr(&[0xff, 0xfe, 0x00, 0x01]), Err(CsrError::Parse));
    }

    #[test]
    fn refuses_a_pem_that_is_not_a_request() {
        let (key, der) = guest_csr_der_with(&PKCS_ECDSA_P256_SHA256);
        drop(key);
        let wrong = pem_wrap("CERTIFICATE", &der);
        assert_eq!(parse_csr(wrong.as_bytes()), Err(CsrError::Parse));
    }

    #[test]
    fn refuses_valid_pem_with_non_csr_der() {
        let wrong = pem_wrap(LABEL, b"definitely not der");
        assert_eq!(parse_csr(wrong.as_bytes()), Err(CsrError::Parse));
    }

    #[test]
    fn refuses_two_requests() {
        let (_, a) = guest_csr();
        let (_, b) = guest_csr();
        assert_eq!(
            parse_csr(format!("{a}{b}").as_bytes()),
            Err(CsrError::Parse)
        );
    }

    #[test]
    fn refuses_trailing_junk_after_the_request() {
        let (_, a) = guest_csr();
        assert_eq!(
            parse_csr(format!("{a}junk junk").as_bytes()),
            Err(CsrError::Parse)
        );
    }

    #[test]
    fn refuses_trailing_bytes_inside_the_der() {
        let (_, mut der) = guest_csr_der_with(&PKCS_ECDSA_P256_SHA256);
        der.push(0);
        assert_eq!(
            parse_csr(pem_wrap(LABEL, &der).as_bytes()),
            Err(CsrError::Parse)
        );
    }

    #[test]
    fn refuses_p384() {
        let (_, der) = guest_csr_der_with(&PKCS_ECDSA_P384_SHA384);
        assert_eq!(
            parse_csr(pem_wrap(LABEL, &der).as_bytes()),
            Err(CsrError::KeyType)
        );
    }

    #[test]
    fn refuses_ed25519() {
        let (_, der) = guest_csr_der_with(&PKCS_ED25519);
        assert_eq!(
            parse_csr(pem_wrap(LABEL, &der).as_bytes()),
            Err(CsrError::KeyType)
        );
    }

    #[test]
    fn refuses_a_tampered_signature() {
        let (_, mut der) = guest_csr_der_with(&PKCS_ECDSA_P256_SHA256);
        let last = der.len() - 1;
        der[last] ^= 0x01;
        assert_eq!(
            parse_csr(pem_wrap(LABEL, &der).as_bytes()),
            Err(CsrError::Signature)
        );
    }

    #[test]
    fn refuses_a_request_whose_content_was_changed_after_signing() {
        let (_, mut der) = guest_csr_der_with(&PKCS_ECDSA_P256_SHA256);
        // Flip a byte inside the SPKI point, well inside the request body.
        let mid = der.len() / 2;
        der[mid] ^= 0x01;
        let got = parse_csr(pem_wrap(LABEL, &der).as_bytes());
        assert!(
            matches!(
                got,
                Err(CsrError::Signature | CsrError::Parse | CsrError::KeyType)
            ),
            "{got:?}"
        );
    }
}
