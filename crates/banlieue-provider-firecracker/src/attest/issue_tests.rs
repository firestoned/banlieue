// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `issue.rs`.

#[cfg(test)]
mod tests {
    use crate::attest::test_support::*;
    use crate::attest::*;
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose,
        PKCS_ECDSA_P256_SHA256, SanType,
    };
    use std::time::{Duration, SystemTime};
    use x509_parser::prelude::*;

    const TTL: Duration = Duration::from_secs(3600);

    fn issue_for(key: &KeyPair, csr_pem: &str, ca: &Ca, serial: Serial, now: SystemTime) -> String {
        let _ = key;
        let pk = parse_csr(csr_pem.as_bytes()).expect("csr");
        issue(ca, TEST_SUBJECT, &pk, TTL, now, serial).expect("issue")
    }

    #[test]
    fn round_trip_chains_to_the_ca_and_proves_the_spire_constraints() {
        let (_tmp, ca) = test_ca();
        let (key, csr) = guest_csr();
        let now = SystemTime::now();
        let pem = issue_for(&key, &csr, &ca, Serial::random().expect("serial"), now);

        let der = cert_der(&pem);
        let (rest, cert) = X509Certificate::from_der(&der).expect("leaf parses");
        assert!(rest.is_empty());
        let ca_der = cert_der(ca.cert_pem());
        let (_, ca_cert) = X509Certificate::from_der(&ca_der).expect("ca parses");

        // The signature chain, checked by a crate that did not produce it.
        cert.verify_signature(Some(ca_cert.public_key()))
            .expect("chains to the CA");
        assert_eq!(cert.issuer(), ca_cert.subject());
        // The certificate carries the guest's own key.
        assert_eq!(
            cert.public_key().raw,
            rcgen::PublicKeyData::subject_public_key_info(&key).as_slice()
        );

        // SPIRE constraint 1: keyUsage present, critical, digitalSignature.
        let ku = cert.key_usage().expect("ku ext").expect("ku present");
        assert!(ku.critical);
        assert!(ku.value.digital_signature());
        assert!(!ku.value.key_cert_sign());
        // SPIRE constraint 2: CA:FALSE, critical.
        let bc = cert
            .basic_constraints()
            .expect("bc ext")
            .expect("bc present");
        assert!(bc.critical);
        assert!(!bc.value.ca);
        // SPIRE constraint 3: the subject is the signer's.
        let cn = cert
            .subject()
            .iter_common_name()
            .next()
            .expect("cn")
            .as_str()
            .expect("str");
        assert_eq!(cn, TEST_SUBJECT);
        // No SAN, no EKU.
        assert!(cert.subject_alternative_name().expect("san ext").is_none());
        assert!(cert.extended_key_usage().expect("eku ext").is_none());
    }

    #[test]
    fn a_leaf_under_the_path_length_zero_ca_verifies_and_cannot_sign() {
        let (_tmp, ca) = test_ca();
        let (key, csr) = guest_csr();
        let pem = issue_for(
            &key,
            &csr,
            &ca,
            Serial::random().expect("serial"),
            SystemTime::now(),
        );

        let ca_der = cert_der(ca.cert_pem());
        let (_, ca_cert) = X509Certificate::from_der(&ca_der).expect("ca parses");
        let der = cert_der(&pem);
        let (_, leaf) = X509Certificate::from_der(&der).expect("leaf parses");

        // The issuer allows no CA below it.
        let ca_bc = ca_cert.basic_constraints().expect("bc").expect("present");
        assert!(ca_bc.value.ca);
        assert_eq!(ca_bc.value.path_len_constraint, Some(0));

        // A leaf is the one thing such a CA may sign, and it does verify.
        leaf.verify_signature(Some(ca_cert.public_key()))
            .expect("chains to the path length zero CA");
        assert_eq!(leaf.issuer(), ca_cert.subject());

        // The leaf is not a CA, so the chain is exactly one certificate deep.
        let bc = leaf.basic_constraints().expect("bc").expect("present");
        assert!(!bc.value.ca);
        assert_eq!(bc.value.path_len_constraint, None);
        let ku = leaf.key_usage().expect("ku").expect("present");
        assert!(!ku.value.key_cert_sign());
    }

    #[test]
    fn the_guest_cannot_choose_the_subject_or_extensions() {
        let (_tmp, ca) = test_ca();
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
        let csr = params
            .serialize_request(&key)
            .expect("csr")
            .pem()
            .expect("pem");

        let pem = issue_for(
            &key,
            &csr,
            &ca,
            Serial::random().expect("serial"),
            SystemTime::now(),
        );
        let der = cert_der(&pem);
        let (_, cert) = X509Certificate::from_der(&der).expect("parse");
        let cn = cert
            .subject()
            .iter_common_name()
            .next()
            .expect("cn")
            .as_str()
            .expect("str");
        assert_eq!(cn, TEST_SUBJECT);
        assert!(
            !cert
                .basic_constraints()
                .expect("bc")
                .expect("present")
                .value
                .ca
        );
        assert!(cert.subject_alternative_name().expect("san").is_none());
        let ku = cert.key_usage().expect("ku").expect("present");
        assert!(ku.value.digital_signature());
        assert!(!ku.value.key_cert_sign());
    }

    #[test]
    fn serial_is_positive_128_bit_and_taken_from_the_argument() {
        let (_tmp, ca) = test_ca();
        let (key, csr) = guest_csr();
        let bytes = [0xff; 16];
        let pem = issue_for(
            &key,
            &csr,
            &ca,
            Serial::from_bytes(bytes),
            SystemTime::now(),
        );
        let der = cert_der(&pem);
        let (_, cert) = X509Certificate::from_der(&der).expect("parse");
        assert_eq!(cert.raw_serial()[0] & 0x80, 0, "positive: top bit clear");
        assert_eq!(cert.raw_serial().len(), 16);
        let mut want = bytes;
        want[0] &= 0x7f;
        assert_eq!(cert.raw_serial(), want.as_slice());
    }

    #[test]
    fn random_serials_differ() {
        let a = Serial::random().expect("a");
        let b = Serial::random().expect("b");
        assert_ne!(a, b);
        assert_eq!(a.as_bytes()[0] & 0x80, 0, "top bit clear");
    }

    #[test]
    fn validity_is_now_plus_ttl_with_a_small_backdate() {
        let (_tmp, ca) = test_ca();
        let (key, csr) = guest_csr();
        let now = SystemTime::now();
        let pem = issue_for(&key, &csr, &ca, Serial::random().expect("s"), now);
        let der = cert_der(&pem);
        let (_, cert) = X509Certificate::from_der(&der).expect("parse");
        let now_s = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("epoch")
            .as_secs() as i64;
        let v = cert.validity();
        assert_eq!(v.not_after.timestamp(), now_s + TTL.as_secs() as i64);
        let backdate = now_s - v.not_before.timestamp();
        assert!(backdate > 0 && backdate <= 600, "backdate {backdate}");
    }

    #[test]
    fn validity_bounds_are_enforced_inclusively() {
        let (_tmp, ca) = test_ca();
        let (_, csr) = guest_csr();
        let pk = parse_csr(csr.as_bytes()).expect("csr");
        let now = SystemTime::now();
        let s = || Serial::random().expect("s");
        assert!(issue(&ca, TEST_SUBJECT, &pk, MIN_VALIDITY, now, s()).is_ok());
        assert!(issue(&ca, TEST_SUBJECT, &pk, MAX_VALIDITY, now, s()).is_ok());
        let under = MIN_VALIDITY - Duration::from_secs(1);
        let over = MAX_VALIDITY + Duration::from_secs(1);
        assert!(matches!(
            issue(&ca, TEST_SUBJECT, &pk, under, now, s()),
            Err(IssueError::Validity)
        ));
        assert!(matches!(
            issue(&ca, TEST_SUBJECT, &pk, over, now, s()),
            Err(IssueError::Validity)
        ));
        assert!(matches!(
            issue(&ca, TEST_SUBJECT, &pk, Duration::ZERO, now, s()),
            Err(IssueError::Validity)
        ));
    }

    #[test]
    fn refuses_an_invalid_subject() {
        let (_tmp, ca) = test_ca();
        let (_, csr) = guest_csr();
        let pk = parse_csr(csr.as_bytes()).expect("csr");
        for bad in ["", "a b", "..", "x/y"] {
            let got = issue(
                &ca,
                bad,
                &pk,
                TTL,
                SystemTime::now(),
                Serial::from_bytes([1; 16]),
            );
            assert!(matches!(got, Err(IssueError::Subject(_))), "{bad:?}");
        }
    }
}
