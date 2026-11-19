// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! An independent check of what the signer issues, by the reference
//! implementation. `#[ignore]`d: it needs an `openssl` binary on the PATH.
//! Run with `cargo test -p banlieue-attestation-ca -- --ignored`. The binary is
//! used by the test only; the crate never spawns a process.

use std::fs;
use std::process::Command;

use crate::testutil::*;

fn openssl(args: &[&str]) -> String {
    let out = Command::new("openssl")
        .args(args)
        .output()
        .expect("openssl must be installed to run this test");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "openssl {args:?} failed: {text}");
    text
}

#[test]
#[ignore = "needs an openssl binary"]
fn openssl_verifies_and_reads_the_issued_certificate() {
    let tmp = tempfile::tempdir().unwrap();
    let signer = signer_in(tmp.path());
    let (csr, _) = good_csr();
    let leaf = signer.sign(csr.as_bytes(), SUBJECT, now()).unwrap();
    let ca_path = tmp.path().join("ca.crt");
    let leaf_path = tmp.path().join("leaf.pem");
    fs::write(&ca_path, signer.ca().certificate_pem()).unwrap();
    fs::write(&leaf_path, &leaf).unwrap();
    let (ca, leaf) = (ca_path.to_str().unwrap(), leaf_path.to_str().unwrap());

    // The fixed clock is in 2027; verify at that time rather than today.
    let at = NOW_SECS.to_string();
    let verified = openssl(&["verify", "-attime", &at, "-CAfile", ca, leaf]);
    assert!(verified.contains("OK"), "{verified}");

    let text = openssl(&["x509", "-in", leaf, "-noout", "-text"]);
    assert!(text.contains(&format!("Subject: CN={SUBJECT}")), "{text}");
    assert!(text.contains("Issuer: CN=host-01"), "{text}");
    assert!(text.contains("CA:FALSE"), "{text}");
    assert!(text.contains("Digital Signature"), "{text}");
    assert!(
        !text.contains("Key Encipherment") && !text.contains("Certificate Sign"),
        "{text}"
    );
    assert!(!text.contains("Subject Alternative Name"), "{text}");
    assert!(!text.contains("Extended Key Usage"), "{text}");
    assert!(text.contains("ecdsa-with-SHA256"), "{text}");
    assert!(text.contains("prime256v1"), "{text}");

    let ca_text = openssl(&["x509", "-in", ca, "-noout", "-text"]);
    assert!(ca_text.contains("CA:TRUE"), "{ca_text}");
}
