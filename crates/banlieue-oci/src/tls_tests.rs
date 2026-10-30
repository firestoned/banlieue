// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `tls.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::install_crypto_provider;

    // A self-signed test CA (CN=banlieue-test-ca-a), generated with
    // `openssl req -x509 -newkey rsa:2048 -nodes`. Not trusted by anything.
    const TEST_CA: &str = "-----BEGIN CERTIFICATE-----
MIIDGzCCAgOgAwIBAgIUJn/SQVpN4u/L3trC79FdyWOFKFEwDQYJKoZIhvcNAQEL
BQAwHTEbMBkGA1UEAwwSYmFubGlldWUtdGVzdC1jYS1hMB4XDTI2MDYwMjAwMzIz
MFoXDTM2MDUzMDAwMzIzMFowHTEbMBkGA1UEAwwSYmFubGlldWUtdGVzdC1jYS1h
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA1jNVEvlefenDRXIyyh4D
ZNNSvwQ3ZV9cZoPEqovHtjokkR+7fZdy9KzvbF+gvUdar0MBlREcLqB1NokAdY+6
+PpP94ij/2Hl58Iqrri5Dg2uBvESfY1lNoNVWmSODl93OmSKIvdHzEkYkOgLKBik
5tLV9LVcOUzBJd+BoIElk1fixg3qaiYg/L+nyg8R8c8KZzQxRonGzELy91lNsf8u
eKMCmBU7b+VOATvjG2r/ECyd1OxV4yklHgxV5zZmboytlLSp+pE8Iu/EWdKh1dD+
05kDSya3BwFuPhHXBM6Vo55a2bcCJOuDgfG78NsvaHSYpaW0Q9GX7Il37N2HiRrh
rwIDAQABo1MwUTAdBgNVHQ4EFgQUIsElFZH/WqwrRVnRF2HkX8c4kPcwHwYDVR0j
BBgwFoAUIsElFZH/WqwrRVnRF2HkX8c4kPcwDwYDVR0TAQH/BAUwAwEB/zANBgkq
hkiG9w0BAQsFAAOCAQEAmCC1c9t5jLUsdh2bSU/4M5owV0Fxpl4HnInwaHQQSIsD
Q38qbBtMnG5YoYptuff3QFx+d/juIKyHlPovdDwD0OYJU5UvMznOUpaCnDPofXNl
dybiqj7uF8BIlS41kyApMKPimH87twDjd9DjfzmzUaL2HeDbq3qeFi8EcWmsD+gn
8WYdiuy0yF9z5rfbQRz1DUnkXtaQEMR8avcOAQ4Jpf+nox6egSF5OhMg2HKznQKw
C0xw7FWQWSEAH+LcwRwo/8l0gqgJf7tZDvfbbZUa7Y48f5UxUNvlmgcHYoPKYoRr
n17Lktsw0jAZJp1tU1DJPZSYHZPPWLZlJhHftNtpKQ==
-----END CERTIFICATE-----\n";

    #[test]
    fn one_certificate_is_one_root() {
        let roots = root_certificates(TEST_CA.as_bytes()).expect("valid PEM");
        assert_eq!(roots.len(), 1);
    }

    /// `from_pem_bundle`, not `from_pem`: a bundle holding a chain adds every
    /// certificate, not just the first.
    #[test]
    fn every_certificate_in_a_bundle_is_a_root() {
        let bundle = format!("{TEST_CA}{TEST_CA}");
        let roots = root_certificates(bundle.as_bytes()).expect("valid bundle");
        assert_eq!(roots.len(), 2);
    }

    /// Input with no PEM block parses to zero certificates. Accepting it would
    /// leave the client on the public roots while the operator believes a CA
    /// is pinned, so it fails closed.
    #[test]
    fn a_bundle_with_no_certificate_is_refused() {
        for junk in ["", "not a pem", "\n\n"] {
            let err = root_certificates(junk.as_bytes()).unwrap_err();
            assert!(
                matches!(&err, Error::Tls(m) if m.contains("no certificates")),
                "{junk:?}: {err}"
            );
        }
    }

    #[test]
    fn no_bundle_builds_a_client_on_the_default_roots() {
        install_crypto_provider();
        assert!(http_client(None).is_ok());
    }

    #[test]
    fn a_bundle_file_builds_a_client() {
        install_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ca.crt");
        std::fs::write(&path, TEST_CA).unwrap();
        assert!(http_client(Some(&path)).is_ok());
    }

    /// The Job names the file on its command line; a typo or an unmounted
    /// volume must fail the Job with that path, never fall back silently.
    #[test]
    fn a_missing_bundle_file_names_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.crt");
        let err = http_client(Some(&path)).unwrap_err();
        assert!(
            matches!(&err, Error::Tls(m) if m.contains("absent.crt")),
            "{err}"
        );
    }

    #[test]
    fn an_empty_bundle_file_is_refused_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.crt");
        std::fs::write(&path, "").unwrap();
        let err = http_client(Some(&path)).unwrap_err();
        assert!(
            matches!(&err, Error::Tls(m) if m.contains("empty.crt") && m.contains("no certificates")),
            "{err}"
        );
    }
}
