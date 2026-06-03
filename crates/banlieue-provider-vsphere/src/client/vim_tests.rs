// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the BYOC HTTP-client helpers in `vim.rs` (ADR-0008).
//!
//! These cover the pure, side-effect-free pieces — PEM parsing and reqwest
//! client construction — without a vCenter. The `ClientBuilder::build().await`
//! path needs a live endpoint and is exercised by integration tests / vcsim.

#[cfg(test)]
mod tests {
    use super::super::*;

    // Two distinct self-signed test CAs (CN=banlieue-test-ca-a / -b), generated
    // with `openssl req -x509 -newkey rsa:2048 -nodes`. Used to verify a bundle
    // with multiple concatenated certs is fully parsed (from_pem_bundle, not
    // from_pem which takes only the first).
    const TEST_CA_A: &str = "-----BEGIN CERTIFICATE-----
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
-----END CERTIFICATE-----
";

    #[test]
    fn root_certs_from_pem_parses_single_cert() {
        let certs = root_certs_from_pem(TEST_CA_A).expect("valid PEM");
        assert_eq!(certs.len(), 1);
    }

    #[test]
    fn root_certs_from_pem_parses_multi_cert_bundle() {
        // A bundle of the same cert twice is still two PEM blocks: proves we
        // read every block, not just the first.
        let bundle = format!("{TEST_CA_A}{TEST_CA_A}");
        let certs = root_certs_from_pem(&bundle).expect("valid bundle");
        assert_eq!(certs.len(), 2);
    }

    #[test]
    fn root_certs_from_pem_rejects_garbage() {
        // Non-PEM input parses to zero certs; we must fail closed rather than
        // silently fall back to system roots.
        let err = root_certs_from_pem("not a pem").unwrap_err();
        assert!(err.to_string().contains("caBundle"), "got: {err}");
        assert!(err.to_string().contains("no certificates"), "got: {err}");
    }

    #[test]
    fn root_certs_from_pem_rejects_empty_string() {
        let err = root_certs_from_pem("").unwrap_err();
        assert!(err.to_string().contains("no certificates"), "got: {err}");
    }

    // Building a reqwest 0.13 client (rustls-no-provider) requires the process
    // crypto provider to be installed, exactly as production does at startup.
    // `install_default_crypto_provider` is idempotent, so every test that builds
    // a client calls it first.
    fn ensure_provider() {
        install_default_crypto_provider();
    }

    #[test]
    fn install_default_crypto_provider_is_idempotent() {
        // Calling twice must not panic (second install is a no-op).
        install_default_crypto_provider();
        install_default_crypto_provider();
    }

    #[test]
    fn build_http_client_succeeds_with_no_bundle() {
        ensure_provider();
        // None bundle, secure: uses system roots, must still build.
        assert!(build_http_client(None, false).is_ok());
    }

    #[test]
    fn build_http_client_succeeds_with_ca_bundle() {
        ensure_provider();
        assert!(build_http_client(Some(TEST_CA_A), false).is_ok());
    }

    #[test]
    fn build_http_client_succeeds_insecure() {
        ensure_provider();
        assert!(build_http_client(None, true).is_ok());
    }

    #[test]
    fn build_http_client_fails_on_invalid_pem() {
        let err = build_http_client(Some("garbage"), false).unwrap_err();
        assert!(err.to_string().contains("caBundle"), "got: {err}");
    }
}
