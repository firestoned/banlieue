// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `ca.rs`.

#[cfg(test)]
mod tests {
    use crate::attest::test_support::*;
    use crate::attest::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::SystemTime;
    use x509_parser::prelude::*;

    const MODE_MASK: u32 = 0o777;

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).expect("stat").permissions().mode() & MODE_MASK
    }

    #[test]
    fn create_writes_key_0600_cert_0644_dir_0700() {
        let (tmp, ca) = test_ca();
        let dir = tmp.path().join("attestation-ca");
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("ca.key")), 0o600);
        assert_eq!(mode(&dir.join("ca.crt")), 0o644);
        assert_eq!(ca.name(), TEST_CA_NAME);
        assert_eq!(
            fs::read_to_string(dir.join("ca.crt")).expect("read"),
            ca.cert_pem()
        );
    }

    #[test]
    fn the_certificate_is_a_self_signed_p256_ca_named_as_asked() {
        let (_tmp, ca) = test_ca();
        let der = cert_der(ca.cert_pem());
        let (_, cert) = X509Certificate::from_der(&der).expect("parse");
        let cn = cert
            .subject()
            .iter_common_name()
            .next()
            .expect("cn")
            .as_str()
            .expect("str");
        assert_eq!(cn, TEST_CA_NAME);
        assert_eq!(cert.issuer(), cert.subject());
        assert!(
            cert.basic_constraints()
                .expect("bc")
                .expect("present")
                .value
                .ca
        );
        cert.verify_signature(None).expect("self-signed");
        let ec = x509_parser::der_parser::oid!(1.2.840.10045.2.1);
        assert_eq!(cert.public_key().algorithm.algorithm, ec);
    }

    #[test]
    fn the_ca_is_limited_to_path_length_zero_and_certificate_signing() {
        let (_tmp, ca) = test_ca();
        let der = cert_der(ca.cert_pem());
        let (_, cert) = X509Certificate::from_der(&der).expect("parse");

        // CA:TRUE, pathlen:0, critical: it signs leaves and nothing that signs.
        let bc = cert.basic_constraints().expect("bc").expect("present");
        assert!(bc.critical);
        assert!(bc.value.ca);
        assert_eq!(bc.value.path_len_constraint, Some(0));

        // keyCertSign alone, critical. No cRLSign: no revocation list is issued.
        let ku = cert.key_usage().expect("ku").expect("present");
        assert!(ku.critical);
        assert!(ku.value.key_cert_sign());
        assert!(!ku.value.crl_sign());
        assert!(!ku.value.digital_signature());
        assert!(!ku.value.non_repudiation());
        assert!(!ku.value.key_encipherment());
        assert!(!ku.value.data_encipherment());
        assert!(!ku.value.key_agreement());
        assert!(!ku.value.encipher_only());
        assert!(!ku.value.decipher_only());
        assert!(cert.extended_key_usage().expect("eku").is_none());
    }

    #[test]
    fn a_created_ca_reports_that_it_signs_leaves_only() {
        let (tmp, created) = test_ca();
        assert!(created.signs_leaves_only());
        let loaded = Ca::load(&tmp.path().join("attestation-ca")).expect("load");
        assert!(loaded.signs_leaves_only());
    }

    #[test]
    fn load_keeps_a_ca_made_before_the_constraint_and_reports_it() {
        // No rotation happens on load: the old CA still loads and still signs,
        // and says it is wider than a new one so the caller can warn.
        let (_tmp, dir) = legacy_unconstrained_ca();
        let loaded = Ca::load(&dir).expect("an unconstrained CA still loads");
        assert!(!loaded.signs_leaves_only());
        assert_eq!(loaded.name(), TEST_CA_NAME);

        let (_, csr) = guest_csr();
        let pk = parse_csr(csr.as_bytes()).expect("csr");
        let pem = issue(
            &loaded,
            TEST_SUBJECT,
            &pk,
            std::time::Duration::from_secs(3600),
            SystemTime::now(),
            Serial::random().expect("s"),
        )
        .expect("issue");
        let (der, ca_der) = (cert_der(&pem), cert_der(loaded.cert_pem()));
        let (_, leaf) = X509Certificate::from_der(&der).expect("leaf");
        let (_, ca_cert) = X509Certificate::from_der(&ca_der).expect("ca");
        leaf.verify_signature(Some(ca_cert.public_key()))
            .expect("chain");
    }

    #[test]
    fn create_refuses_an_invalid_name_and_creates_nothing() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("attestation-ca");
        for bad in ["", "a b", "..", "caf\u{e9}"] {
            let got = Ca::create(&dir, bad, SystemTime::now());
            assert!(matches!(got, Err(CaError::Name(_))), "{bad:?}");
            assert!(!dir.exists(), "{bad:?} left a directory");
        }
    }

    #[test]
    fn create_never_overwrites_an_existing_ca() {
        let (tmp, ca) = test_ca();
        let dir = tmp.path().join("attestation-ca");
        let key_before = fs::read(dir.join("ca.key")).expect("key");
        let again = Ca::create(&dir, "baz.foo.io", SystemTime::now());
        assert!(matches!(again, Err(CaError::AlreadyExists(_))));
        assert_eq!(fs::read(dir.join("ca.key")).expect("key"), key_before);
        assert_eq!(
            fs::read_to_string(dir.join("ca.crt")).expect("crt"),
            ca.cert_pem()
        );
    }

    #[test]
    fn create_refuses_when_only_one_of_the_two_files_exists() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("attestation-ca");
        fs::create_dir(&dir).expect("mkdir");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("chmod");
        fs::write(dir.join("ca.crt"), "stale").expect("write");
        let got = Ca::create(&dir, TEST_CA_NAME, SystemTime::now());
        assert!(matches!(got, Err(CaError::AlreadyExists(_))));
        assert_eq!(
            fs::read_to_string(dir.join("ca.crt")).expect("read"),
            "stale"
        );
        assert!(!dir.join("ca.key").exists());
    }

    #[test]
    fn create_refuses_an_existing_directory_that_is_not_0700() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("attestation-ca");
        fs::create_dir(&dir).expect("mkdir");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod");
        let got = Ca::create(&dir, TEST_CA_NAME, SystemTime::now());
        assert!(
            matches!(
                got,
                Err(CaError::Mode {
                    found: 0o755,
                    want: 0o700,
                    ..
                })
            ),
            "{got:?}"
        );
        assert!(!dir.join("ca.key").exists());
    }

    #[test]
    fn load_round_trips_and_can_sign() {
        let (tmp, created) = test_ca();
        let dir = tmp.path().join("attestation-ca");
        let loaded = Ca::load(&dir).expect("load");
        assert_eq!(loaded.name(), TEST_CA_NAME);
        assert_eq!(loaded.cert_pem(), created.cert_pem());
        let (_, csr) = guest_csr();
        let pk = parse_csr(csr.as_bytes()).expect("csr");
        let pem = issue(
            &loaded,
            TEST_SUBJECT,
            &pk,
            std::time::Duration::from_secs(3600),
            SystemTime::now(),
            Serial::random().expect("s"),
        )
        .expect("issue");
        let (der, ca_der) = (cert_der(&pem), cert_der(created.cert_pem()));
        let (_, leaf) = X509Certificate::from_der(&der).expect("leaf");
        let (_, ca_cert) = X509Certificate::from_der(&ca_der).expect("ca");
        leaf.verify_signature(Some(ca_cert.public_key()))
            .expect("chain");
    }

    #[test]
    fn load_refuses_a_key_that_is_not_0600() {
        for bad in [0o640, 0o644, 0o660, 0o400, 0o700] {
            let (tmp, _ca) = test_ca();
            let dir = tmp.path().join("attestation-ca");
            fs::set_permissions(dir.join("ca.key"), fs::Permissions::from_mode(bad))
                .expect("chmod");
            let got = Ca::load(&dir);
            assert!(
                matches!(got, Err(CaError::Mode { want: 0o600, .. })),
                "{bad:o}: {:?}",
                got.err()
            );
        }
    }

    #[test]
    fn load_refuses_a_directory_that_is_not_0700() {
        for bad in [0o755, 0o750, 0o770, 0o500] {
            let (tmp, _ca) = test_ca();
            let dir = tmp.path().join("attestation-ca");
            fs::set_permissions(&dir, fs::Permissions::from_mode(bad)).expect("chmod");
            let got = Ca::load(&dir);
            // Restore so the temp dir can be cleaned up.
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("chmod back");
            assert!(
                matches!(got, Err(CaError::Mode { want: 0o700, .. })),
                "{bad:o}: {:?}",
                got.err()
            );
        }
    }

    #[test]
    fn load_refuses_a_key_that_is_a_symlink() {
        let (tmp, _ca) = test_ca();
        let dir = tmp.path().join("attestation-ca");
        let real = tmp.path().join("real.key");
        fs::rename(dir.join("ca.key"), &real).expect("mv");
        std::os::unix::fs::symlink(&real, dir.join("ca.key")).expect("ln");
        assert!(matches!(Ca::load(&dir), Err(CaError::NotRegular(_))));
    }

    #[test]
    fn load_reports_a_missing_ca() {
        let tmp = tempfile::tempdir().expect("tmp");
        assert!(matches!(
            Ca::load(&tmp.path().join("nope")),
            Err(CaError::Io { .. })
        ));
    }

    #[test]
    fn load_refuses_a_key_that_does_not_match_the_certificate() {
        let (tmp, _a) = test_ca();
        let other = tmp.path().join("other-ca");
        let b = Ca::create(&other, "baz.foo.io", SystemTime::now()).expect("b");
        let dir = tmp.path().join("attestation-ca");
        fs::write(dir.join("ca.crt"), b.cert_pem()).expect("swap");
        assert!(matches!(Ca::load(&dir), Err(CaError::KeyMismatch)));
    }
}
