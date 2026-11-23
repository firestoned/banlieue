// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use x509_cert::Certificate;
use x509_cert::der::{DecodePem, Encode};
use x509_cert::request::CertReq;

use super::*;
use crate::testutil::{UpstreamOpts, now, upstream_intermediate, upstream_root, upstream_sign};
use crate::{AttestationCa, CaError, RateLimit, SignError, Signer};

const NAME: &str = "host-01";
const ROOT_CN: &str = "attestation-root";

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

struct Rig {
    _t: tempfile::TempDir,
    pending: PathBuf,
    ca_dir: PathBuf,
}

fn rig() -> Rig {
    let t = tempfile::tempdir().unwrap();
    Rig {
        pending: t.path().join("attestation-ca-pending"),
        ca_dir: t.path().join("attestation-ca"),
        _t: t,
    }
}

fn pending(r: &Rig) -> PendingCa {
    PendingCa::create(&r.pending, NAME, None).unwrap()
}

fn nothing_installed(r: &Rig) {
    for f in ["ca.key", "ca.crt", "ca-chain.pem"] {
        assert!(!r.ca_dir.join(f).exists(), "{f} must not exist");
    }
    assert!(r.pending.join("ca.key").exists(), "pending key must stay");
    assert!(r.pending.join("ca.csr").exists(), "pending csr must stay");
}

fn certs(pem: &str) -> Vec<Certificate> {
    Certificate::load_pem_chain(pem.as_bytes()).unwrap()
}

// --------------------------------------------------------------- create

#[test]
fn create_writes_key_and_csr_and_no_certificate() {
    let r = rig();
    let p = pending(&r);
    assert_eq!(p.common_name(), NAME);
    assert!(r.pending.join("ca.key").exists());
    assert!(r.pending.join("ca.csr").exists());
    assert!(!r.pending.join("ca.crt").exists());
    assert!(!r.ca_dir.exists(), "the live directory is not touched");
}

#[test]
fn create_sets_modes_0700_0600_0644() {
    let r = rig();
    pending(&r);
    assert_eq!(mode(&r.pending), 0o700);
    assert_eq!(mode(&r.pending.join("ca.key")), 0o600);
    assert_eq!(mode(&r.pending.join("ca.csr")), 0o644);
}

#[test]
fn create_makes_a_csr_for_the_key_with_the_names_cn_and_a_valid_signature() {
    let r = rig();
    let p = pending(&r);
    assert_eq!(
        p.csr_pem(),
        fs::read_to_string(r.pending.join("ca.csr")).unwrap()
    );
    let req = CertReq::from_pem(p.csr_pem().as_bytes()).unwrap();
    let key =
        VerifyingKey::from_sec1_bytes(req.info.public_key.subject_public_key.raw_bytes()).unwrap();
    let sig = Signature::from_der(req.signature.raw_bytes()).unwrap();
    key.verify(&req.info.to_der().unwrap(), &sig)
        .expect("the request is signed by its own key");
    assert_eq!(req.info.subject.to_string(), "CN=host-01");
}

#[test]
fn create_refuses_a_bad_name_and_creates_nothing() {
    let r = rig();
    let err = PendingCa::create(&r.pending, "bad name", None)
        .err()
        .unwrap();
    assert!(matches!(err, CaError::InvalidName(_)), "{err}");
    assert!(!r.pending.exists());
}

#[test]
fn create_refuses_to_overwrite_a_pending_request() {
    let r = rig();
    pending(&r);
    let key = fs::read(r.pending.join("ca.key")).unwrap();
    let err = PendingCa::create(&r.pending, "host-02", None)
        .err()
        .unwrap();
    assert!(matches!(err, CaError::AlreadyExists), "{err}");
    assert_eq!(key, fs::read(r.pending.join("ca.key")).unwrap());
}

#[test]
fn create_refuses_a_symlinked_pending_directory() {
    let r = rig();
    let elsewhere = r._t.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    symlink(&elsewhere, &r.pending).unwrap();
    let err = PendingCa::create(&r.pending, NAME, None).err().unwrap();
    assert!(matches!(err, CaError::Insecure(_)), "{err}");
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
}

// ----------------------------------------------------------------- load

#[test]
fn load_finds_what_create_made() {
    let r = rig();
    let made = pending(&r);
    let loaded = PendingCa::load(&r.pending).unwrap();
    assert_eq!(loaded.common_name(), NAME);
    assert_eq!(loaded.csr_pem(), made.csr_pem());
}

#[test]
fn load_refuses_a_key_that_is_not_0600() {
    let r = rig();
    pending(&r);
    fs::set_permissions(r.pending.join("ca.key"), fs::Permissions::from_mode(0o640)).unwrap();
    assert!(matches!(
        PendingCa::load(&r.pending).err().unwrap(),
        CaError::Insecure(_)
    ));
}

#[test]
fn load_refuses_a_directory_open_to_group_or_world() {
    let r = rig();
    pending(&r);
    fs::set_permissions(&r.pending, fs::Permissions::from_mode(0o750)).unwrap();
    assert!(matches!(
        PendingCa::load(&r.pending).err().unwrap(),
        CaError::Insecure(_)
    ));
}

#[test]
fn load_of_a_missing_pending_directory_is_an_io_error() {
    let r = rig();
    assert!(matches!(
        PendingCa::load(&r.pending).err().unwrap(),
        CaError::Io { .. }
    ));
}

// --------------------------------------------------------------- import

fn signed(p: &PendingCa, root: &(p256::ecdsa::SigningKey, String), opts: &UpstreamOpts) -> String {
    upstream_sign(p.csr_pem(), &root.0, ROOT_CN, opts)
}

#[test]
fn import_installs_the_signed_ca_with_its_chain() {
    let r = rig();
    let p = pending(&r);
    let root = upstream_root(ROOT_CN);
    let host = signed(&p, &root, &UpstreamOpts::good());
    let bundle = format!("{host}{}", root.1);

    let ca = p.import(&r.ca_dir, &bundle, now(), false, None).unwrap();

    assert_eq!(ca.common_name(), NAME);
    assert_eq!(mode(&r.ca_dir), 0o700);
    assert_eq!(mode(&r.ca_dir.join("ca.key")), 0o600);
    assert_eq!(mode(&r.ca_dir.join("ca.crt")), 0o644);
    assert_eq!(mode(&r.ca_dir.join("ca-chain.pem")), 0o644);
    // ca.crt is the host CA alone, so hostCa stays one certificate.
    let crt = fs::read_to_string(r.ca_dir.join("ca.crt")).unwrap();
    assert_eq!(certs(&crt).len(), 1);
    assert_eq!(crt.trim(), host.trim());
    // The pending key moved; the request is gone with it.
    assert!(!r.pending.join("ca.key").exists());
    // And it loads as a CA, with the PKI's validity.
    let loaded = AttestationCa::load(&r.ca_dir).unwrap();
    assert_eq!(loaded.common_name(), NAME);
}

#[test]
fn import_keeps_the_host_ca_and_intermediates_in_the_chain_but_not_the_root() {
    let r = rig();
    let p = pending(&r);
    let root = upstream_root(ROOT_CN);
    let (mid_key, mid_pem) = upstream_intermediate("attestation-issuing", &root.0, ROOT_CN);
    let host = upstream_sign(
        p.csr_pem(),
        &mid_key,
        "attestation-issuing",
        &UpstreamOpts::good(),
    );
    let bundle = format!("{host}{mid_pem}{}", root.1);

    p.import(&r.ca_dir, &bundle, now(), false, None).unwrap();

    let chain = certs(&fs::read_to_string(r.ca_dir.join("ca-chain.pem")).unwrap());
    let subjects: Vec<String> = chain
        .iter()
        .map(|c| c.tbs_certificate().subject().to_string())
        .collect();
    assert_eq!(subjects, vec!["CN=host-01", "CN=attestation-issuing"]);
}

#[test]
fn import_of_a_self_signed_certificate_writes_no_chain_file() {
    let r = rig();
    let p = pending(&r);
    // The upstream "signs" with the host's own key: self-signed.
    let key_pem = fs::read_to_string(r.pending.join("ca.key")).unwrap();
    let own = <p256::ecdsa::SigningKey as p256::pkcs8::DecodePrivateKey>::from_pkcs8_pem(&key_pem)
        .unwrap();
    let cert = upstream_sign(p.csr_pem(), &own, NAME, &UpstreamOpts::good());
    p.import(&r.ca_dir, &cert, now(), false, None).unwrap();
    assert!(!r.ca_dir.join("ca-chain.pem").exists());
}

#[test]
fn a_signer_under_an_imported_ca_replies_with_the_leaf_then_the_chain() {
    let r = rig();
    let p = pending(&r);
    let root = upstream_root(ROOT_CN);
    let (mid_key, mid_pem) = upstream_intermediate("attestation-issuing", &root.0, ROOT_CN);
    let host = upstream_sign(
        p.csr_pem(),
        &mid_key,
        "attestation-issuing",
        &UpstreamOpts::good(),
    );
    let ca = p
        .import(
            &r.ca_dir,
            &format!("{host}{mid_pem}{}", root.1),
            now(),
            false,
            None,
        )
        .unwrap();
    let signer = Signer::new(ca, crate::testutil::TTL, RateLimit::default()).unwrap();
    let (csr, _key) = crate::testutil::good_csr();

    let reply = signer
        .sign(csr.as_bytes(), crate::testutil::SUBJECT, now())
        .unwrap();

    let got = certs(&reply);
    let subjects: Vec<String> = got
        .iter()
        .map(|c| c.tbs_certificate().subject().to_string())
        .collect();
    assert_eq!(
        subjects,
        vec![
            format!("CN={}", crate::testutil::SUBJECT),
            "CN=host-01".to_string(),
            "CN=attestation-issuing".to_string(),
        ]
    );
    let leaf_issuer = got[0].tbs_certificate().issuer().to_string();
    assert_eq!(leaf_issuer, "CN=host-01");
}

#[test]
fn a_signer_under_a_self_signed_ca_still_replies_with_the_leaf_alone() {
    let t = tempfile::tempdir().unwrap();
    let signer = crate::testutil::signer_in(t.path());
    let (csr, _key) = crate::testutil::good_csr();
    let reply = signer
        .sign(csr.as_bytes(), crate::testutil::SUBJECT, now())
        .unwrap();
    assert_eq!(certs(&reply).len(), 1);
}

fn rejected(opts: UpstreamOpts) {
    let r = rig();
    let p = pending(&r);
    let root = upstream_root(ROOT_CN);
    let host = signed(&p, &root, &opts);
    let err = p
        .import(&r.ca_dir, &host, now(), false, None)
        .err()
        .unwrap();
    assert!(matches!(err, CaError::Rejected(_)), "{err}");
    nothing_installed(&r);
}

#[test]
fn import_rejects_a_certificate_for_another_key() {
    rejected(UpstreamOpts {
        certify: Some(crate::testutil::new_key()),
        ..UpstreamOpts::good()
    });
}

#[test]
fn import_rejects_a_certificate_with_another_common_name() {
    rejected(UpstreamOpts {
        subject_cn: Some("host-02".into()),
        ..UpstreamOpts::good()
    });
}

#[test]
fn import_rejects_a_common_name_outside_the_alphabet() {
    rejected(UpstreamOpts {
        subject_cn: Some("host 01".into()),
        ..UpstreamOpts::good()
    });
}

#[test]
fn import_rejects_a_certificate_that_is_not_a_ca() {
    rejected(UpstreamOpts {
        ca: false,
        path_len: None,
        ..UpstreamOpts::good()
    });
}

#[test]
fn import_rejects_a_ca_with_no_path_length_limit() {
    rejected(UpstreamOpts {
        path_len: None,
        ..UpstreamOpts::good()
    });
}

#[test]
fn import_rejects_a_ca_that_may_sign_further_cas() {
    rejected(UpstreamOpts {
        path_len: Some(1),
        ..UpstreamOpts::good()
    });
}

#[test]
fn import_rejects_a_certificate_without_key_cert_sign() {
    rejected(UpstreamOpts {
        key_cert_sign: false,
        ..UpstreamOpts::good()
    });
}

#[test]
fn import_rejects_an_expired_certificate() {
    rejected(UpstreamOpts {
        not_before: now() - Duration::from_secs(7200),
        not_after: now() - Duration::from_secs(3600),
        ..UpstreamOpts::good()
    });
}

#[test]
fn import_rejects_a_certificate_not_yet_valid() {
    rejected(UpstreamOpts {
        not_before: now() + Duration::from_secs(86_400),
        not_after: now() + Duration::from_secs(2 * 86_400),
        ..UpstreamOpts::good()
    });
}

#[test]
fn import_rejects_a_validity_beyond_the_cap() {
    rejected(UpstreamOpts {
        not_after: now() + MAX_IMPORTED_CA_VALIDITY + Duration::from_secs(86_400),
        ..UpstreamOpts::good()
    });
}

#[test]
fn import_rejects_text_that_is_not_a_certificate() {
    for junk in [
        "",
        "hello",
        "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    ] {
        let r = rig();
        let p = pending(&r);
        let err = p.import(&r.ca_dir, junk, now(), false, None).err().unwrap();
        assert!(matches!(err, CaError::Rejected(_)), "{junk:?}: {err}");
        nothing_installed(&r);
    }
}

#[test]
fn import_rejects_a_chain_whose_links_do_not_join() {
    let r = rig();
    let p = pending(&r);
    let root = upstream_root(ROOT_CN);
    let stranger = upstream_root("some-other-root");
    // The host certificate names ROOT_CN as its issuer; the next one is not it.
    let host = signed(&p, &root, &UpstreamOpts::good());
    let err = p
        .import(
            &r.ca_dir,
            &format!("{host}{}", stranger.1),
            now(),
            false,
            None,
        )
        .err()
        .unwrap();
    assert!(matches!(err, CaError::Rejected(_)), "{err}");
    nothing_installed(&r);
}

#[test]
fn import_rejects_an_oversized_chain() {
    let r = rig();
    let p = pending(&r);
    let root = upstream_root(ROOT_CN);
    let host = signed(&p, &root, &UpstreamOpts::good());
    let bundle = format!("{host}{}", root.1.repeat(MAX_IMPORTED_CHAIN + 1));
    let err = p
        .import(&r.ca_dir, &bundle, now(), false, None)
        .err()
        .unwrap();
    assert!(matches!(err, CaError::Rejected(_)), "{err}");
    nothing_installed(&r);
}

#[test]
fn import_refuses_to_replace_a_live_ca() {
    let r = rig();
    let live = AttestationCa::create(&r.ca_dir, "old-host", None).unwrap();
    let old_cert = live.certificate_pem().to_string();
    let p = pending(&r);
    let root = upstream_root(ROOT_CN);
    let host = signed(&p, &root, &UpstreamOpts::good());
    let err = p
        .import(&r.ca_dir, &host, now(), false, None)
        .err()
        .unwrap();
    assert!(matches!(err, CaError::AlreadyExists), "{err}");
    assert_eq!(
        old_cert,
        fs::read_to_string(r.ca_dir.join("ca.crt")).unwrap()
    );
    assert!(r.pending.join("ca.key").exists());
}

#[test]
fn import_with_replace_swaps_a_live_ca_for_the_new_one() {
    let r = rig();
    AttestationCa::create(&r.ca_dir, "old-host", None).unwrap();
    let p = pending(&r);
    let root = upstream_root(ROOT_CN);
    let host = signed(&p, &root, &UpstreamOpts::good());
    let ca = p.import(&r.ca_dir, &host, now(), true, None).unwrap();
    assert_eq!(ca.common_name(), NAME);
    assert_eq!(AttestationCa::load(&r.ca_dir).unwrap().common_name(), NAME);
}

#[test]
fn a_failed_replace_leaves_the_live_ca_serving() {
    let r = rig();
    let live = AttestationCa::create(&r.ca_dir, "old-host", None).unwrap();
    let p = pending(&r);
    let root = upstream_root(ROOT_CN);
    let host = signed(
        &p,
        &root,
        &UpstreamOpts {
            ca: false,
            path_len: None,
            ..UpstreamOpts::good()
        },
    );
    assert!(p.import(&r.ca_dir, &host, now(), true, None).is_err());
    let still = AttestationCa::load(&r.ca_dir).unwrap();
    assert_eq!(still.certificate_pem(), live.certificate_pem());
}

// --------------------------------------------------------------- expiry

#[test]
fn a_self_signed_ca_reports_its_expiry_and_is_not_expired() {
    let t = tempfile::tempdir().unwrap();
    let ca = AttestationCa::create(&t.path().join("ca"), NAME, None).unwrap();
    let now = std::time::SystemTime::now();
    assert!(!ca.is_expired(now));
    assert!(ca.not_after() > now + Duration::from_secs(300 * 86_400));
}

#[test]
fn an_imported_ca_is_expired_after_its_not_after() {
    let r = rig();
    let p = pending(&r);
    let root = upstream_root(ROOT_CN);
    let host = signed(&p, &root, &UpstreamOpts::good());
    let ca = p.import(&r.ca_dir, &host, now(), false, None).unwrap();
    assert!(!ca.is_expired(now()));
    assert!(ca.is_expired(now() + Duration::from_secs(366 * 86_400)));
}

#[test]
fn a_signer_refuses_once_its_ca_has_expired_and_still_signs_before() {
    let r = rig();
    let p = pending(&r);
    let root = upstream_root(ROOT_CN);
    let host = signed(&p, &root, &UpstreamOpts::good());
    let ca = p.import(&r.ca_dir, &host, now(), false, None).unwrap();
    let signer = Signer::new(ca, crate::testutil::TTL, RateLimit::default()).unwrap();
    let (csr, _key) = crate::testutil::good_csr();

    assert!(
        signer
            .sign(csr.as_bytes(), crate::testutil::SUBJECT, now())
            .is_ok()
    );
    let later = now() + Duration::from_secs(366 * 86_400);
    let err = signer
        .sign(csr.as_bytes(), crate::testutil::SUBJECT, later)
        .unwrap_err();
    assert!(matches!(err, SignError::CaExpired), "{err}");
}
