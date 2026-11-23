// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use x509_parser::prelude::*;

use super::*;

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn ca_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("attestation-ca");
    (tmp, dir)
}

#[test]
fn create_sets_modes_0700_0600_0644() {
    let (_t, dir) = ca_dir();
    AttestationCa::create(&dir, "host-01", None).unwrap();
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("ca.key")), 0o600);
    assert_eq!(mode(&dir.join("ca.crt")), 0o644);
}

#[test]
fn create_makes_a_self_signed_p256_ca_certificate() {
    let (_t, dir) = ca_dir();
    let ca = AttestationCa::create(&dir, "host-01", None).unwrap();
    assert_eq!(ca.common_name(), "host-01");
    let pem = fs::read(dir.join("ca.crt")).unwrap();
    assert_eq!(pem, ca.certificate_pem().as_bytes());
    let (_, p) = parse_x509_pem(&pem).unwrap();
    let cert = p.parse_x509().unwrap();
    assert_eq!(cert.subject().to_string(), "CN=host-01");
    assert_eq!(cert.issuer(), cert.subject());
    let bc = cert.basic_constraints().unwrap().unwrap();
    assert!(bc.critical && bc.value.ca);
    let ku = cert.key_usage().unwrap().unwrap();
    assert!(ku.value.key_cert_sign());
    assert!(!ku.value.digital_signature());
    assert!(
        cert.public_key().algorithm.algorithm
            == x509_parser::oid_registry::OID_KEY_TYPE_EC_PUBLIC_KEY
    );
    let key =
        VerifyingKey::from_sec1_bytes(cert.public_key().subject_public_key.data.as_ref()).unwrap();
    let sig = Signature::from_der(cert.signature_value.data.as_ref()).unwrap();
    key.verify(cert.tbs_certificate.as_ref(), &sig)
        .expect("self-signed");
}

#[test]
fn create_refuses_to_overwrite_an_existing_ca() {
    let (_t, dir) = ca_dir();
    AttestationCa::create(&dir, "host-01", None).unwrap();
    let before = fs::read(dir.join("ca.key")).unwrap();
    let err = AttestationCa::create(&dir, "host-02", None).err().unwrap();
    assert!(matches!(err, CaError::AlreadyExists), "{err}");
    assert_eq!(before, fs::read(dir.join("ca.key")).unwrap());
}

#[test]
fn create_refuses_when_only_the_certificate_exists() {
    let (_t, dir) = ca_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ca.crt"), b"x").unwrap();
    assert!(matches!(
        AttestationCa::create(&dir, "host-01", None).err().unwrap(),
        CaError::AlreadyExists
    ));
    assert!(!dir.join("ca.key").exists());
}

#[test]
fn create_refuses_a_bad_name_and_creates_nothing() {
    let (_t, dir) = ca_dir();
    for bad in ["", "bad name", "a,b", "a=b", "é", "a/b"] {
        let err = AttestationCa::create(&dir, bad, None).err().unwrap();
        assert!(matches!(err, CaError::InvalidName(_)), "{bad:?}");
        assert!(!dir.exists(), "{bad:?} left a directory behind");
    }
}

#[test]
fn create_tightens_a_preexisting_loose_directory() {
    let (_t, dir) = ca_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    AttestationCa::create(&dir, "host-01", None).unwrap();
    assert_eq!(mode(&dir), 0o700);
}

#[test]
fn create_with_owner_chowns_directory_and_files() {
    let (_t, dir) = ca_dir();
    let me = fs::metadata(dir.parent().unwrap()).unwrap();
    AttestationCa::create(&dir, "host-01", Some((me.uid(), me.gid()))).unwrap();
    for p in [dir.clone(), dir.join("ca.key"), dir.join("ca.crt")] {
        let m = fs::metadata(p).unwrap();
        assert_eq!((m.uid(), m.gid()), (me.uid(), me.gid()));
    }
}

#[test]
fn load_round_trips_name_and_certificate() {
    let (_t, dir) = ca_dir();
    let made = AttestationCa::create(&dir, "host-01", None).unwrap();
    let loaded = AttestationCa::load(&dir).unwrap();
    assert_eq!(loaded.common_name(), "host-01");
    assert_eq!(loaded.certificate_pem(), made.certificate_pem());
}

#[test]
fn load_refuses_a_key_that_is_not_0600() {
    for bad in [0o644, 0o640, 0o604, 0o400, 0o660, 0o700] {
        let (_t, dir) = ca_dir();
        AttestationCa::create(&dir, "host-01", None).unwrap();
        fs::set_permissions(dir.join("ca.key"), fs::Permissions::from_mode(bad)).unwrap();
        let err = AttestationCa::load(&dir).err().unwrap();
        assert!(matches!(err, CaError::Insecure(_)), "{bad:o}: {err}");
    }
}

#[test]
fn load_refuses_a_group_or_world_accessible_directory() {
    for bad in [0o750, 0o705, 0o701, 0o710, 0o770, 0o777] {
        let (_t, dir) = ca_dir();
        AttestationCa::create(&dir, "host-01", None).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(bad)).unwrap();
        let err = AttestationCa::load(&dir).err().unwrap();
        assert!(matches!(err, CaError::Insecure(_)), "{bad:o}: {err}");
    }
}

#[test]
fn load_refuses_a_symlinked_key() {
    let (_t, dir) = ca_dir();
    AttestationCa::create(&dir, "host-01", None).unwrap();
    let real = dir.join("real.key");
    fs::rename(dir.join("ca.key"), &real).unwrap();
    symlink(&real, dir.join("ca.key")).unwrap();
    assert!(matches!(
        AttestationCa::load(&dir).err().unwrap(),
        CaError::Insecure(_)
    ));
}

#[test]
fn load_refuses_a_missing_ca() {
    let (_t, dir) = ca_dir();
    assert!(matches!(
        AttestationCa::load(&dir).err().unwrap(),
        CaError::Io { .. }
    ));
}

#[test]
fn load_refuses_a_key_that_does_not_match_the_certificate() {
    let (_t, dir) = ca_dir();
    let (_t2, dir2) = ca_dir();
    AttestationCa::create(&dir, "host-01", None).unwrap();
    AttestationCa::create(&dir2, "host-01", None).unwrap();
    fs::copy(dir2.join("ca.key"), dir.join("ca.key")).unwrap();
    assert!(matches!(
        AttestationCa::load(&dir).err().unwrap(),
        CaError::Corrupt(_)
    ));
}

#[test]
fn load_refuses_garbage_in_the_key_file() {
    let (_t, dir) = ca_dir();
    AttestationCa::create(&dir, "host-01", None).unwrap();
    fs::write(dir.join("ca.key"), b"not a key").unwrap();
    assert!(matches!(
        AttestationCa::load(&dir).err().unwrap(),
        CaError::Corrupt(_)
    ));
}

#[test]
fn debug_output_shows_the_name_and_no_key_or_pem() {
    let (_t, dir) = ca_dir();
    let ca = AttestationCa::create(&dir, "host-01", None).unwrap();
    let shown = format!("{ca:?}");
    assert!(shown.contains("host-01"));
    assert!(!shown.contains("-----"));
    let key_b64 = fs::read_to_string(dir.join("ca.key"))
        .unwrap()
        .lines()
        .nth(1)
        .unwrap()
        .to_owned();
    assert!(!shown.contains(&key_b64));
}

/// A decoy directory standing where the provider's user planted a symlink.
fn decoy_with_link() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let (t, dir) = ca_dir();
    let decoy = t.path().join("decoy");
    fs::create_dir(&decoy).unwrap();
    fs::set_permissions(&decoy, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(decoy.join("keep"), b"keep").unwrap();
    symlink(&decoy, &dir).unwrap();
    (t, dir, decoy)
}

#[test]
fn create_refuses_a_symlinked_directory_and_touches_nothing_behind_it() {
    let (t, dir, decoy) = decoy_with_link();
    let me = fs::metadata(t.path()).unwrap();
    let err = AttestationCa::create(&dir, "host-01", Some((me.uid(), me.gid())))
        .err()
        .unwrap();
    assert!(matches!(err, CaError::Insecure(_)), "{err}");
    assert_eq!(mode(&decoy), 0o755, "the link target was chmodded");
    let names: Vec<_> = fs::read_dir(&decoy)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("keep")]);
    assert_eq!(fs::read(decoy.join("keep")).unwrap(), b"keep");
}

#[test]
fn create_refuses_a_symlinked_key_or_certificate_and_writes_nothing_through_it() {
    for file in ["ca.key", "ca.crt"] {
        let (t, dir) = ca_dir();
        fs::create_dir_all(&dir).unwrap();
        let victim = t.path().join("victim");
        fs::write(&victim, b"victim").unwrap();
        symlink(&victim, dir.join(file)).unwrap();
        let err = AttestationCa::create(&dir, "host-01", None).err().unwrap();
        assert!(matches!(err, CaError::AlreadyExists), "{file}: {err}");
        assert_eq!(fs::read(&victim).unwrap(), b"victim", "{file}");
    }
}

#[test]
fn remove_files_unlinks_a_planted_symlink_without_following_it() {
    let (t, dir) = ca_dir();
    AttestationCa::create(&dir, "host-01", None).unwrap();
    let victim = t.path().join("victim");
    fs::write(&victim, b"victim").unwrap();
    fs::remove_file(dir.join("ca.key")).unwrap();
    symlink(&victim, dir.join("ca.key")).unwrap();
    AttestationCa::remove_files(&dir).unwrap();
    assert!(fs::symlink_metadata(dir.join("ca.key")).is_err());
    assert!(fs::symlink_metadata(dir.join("ca.crt")).is_err());
    assert_eq!(fs::read(&victim).unwrap(), b"victim");
}

#[test]
fn remove_files_refuses_a_symlinked_directory() {
    let (_t, dir, decoy) = decoy_with_link();
    fs::write(decoy.join("ca.key"), b"decoy key").unwrap();
    let err = AttestationCa::remove_files(&dir).err().unwrap();
    assert!(matches!(err, CaError::Insecure(_)), "{err}");
    assert_eq!(fs::read(decoy.join("ca.key")).unwrap(), b"decoy key");
}

#[test]
fn remove_files_with_nothing_there_is_success() {
    let (_t, dir) = ca_dir();
    fs::create_dir_all(&dir).unwrap();
    AttestationCa::remove_files(&dir).unwrap();
}

#[test]
fn certificate_not_after_reads_the_certificate_without_the_key() {
    let (_t, dir) = ca_dir();
    let ca = AttestationCa::create(&dir, "host-01", None).unwrap();
    fs::remove_file(dir.join("ca.key")).unwrap();
    assert_eq!(certificate_not_after(&dir), Some(ca.not_after()));
}

#[test]
fn certificate_not_after_is_none_for_nothing_garbage_or_a_symlink() {
    let (_t, dir) = ca_dir();
    assert_eq!(certificate_not_after(&dir), None);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ca.crt"), b"not a certificate").unwrap();
    assert_eq!(certificate_not_after(&dir), None);
    fs::remove_file(dir.join("ca.crt")).unwrap();
    let real = dir.join("real.crt");
    fs::write(&real, b"x").unwrap();
    symlink(&real, dir.join("ca.crt")).unwrap();
    assert_eq!(certificate_not_after(&dir), None);
}
