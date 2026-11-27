// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The per-host attestation CA (ADR-0143 Decision 1).

use std::fs::{self, DirBuilder};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, PKCS_ECDSA_P256_SHA256, PublicKeyData, SerialNumber,
};
use thiserror::Error;
use time::OffsetDateTime;
use x509_parser::pem::parse_x509_pem;

use super::issue::Serial;
use super::name::{NameError, validate_name};

/// The permission bits of a mode.
const MODE_MASK: u32 = 0o777;
/// `notBefore` of the CA certificate, before now, for clock skew.
const CA_BACKDATE: Duration = Duration::from_secs(300);

/// Mode of the CA directory.
pub const CA_DIR_MODE: u32 = 0o700;
/// Mode of the CA key.
pub const CA_KEY_MODE: u32 = 0o600;
/// Mode of the CA certificate.
pub const CA_CERT_MODE: u32 = 0o644;
/// Key file name.
pub const CA_KEY_FILE: &str = "ca.key";
/// Certificate file name.
pub const CA_CERT_FILE: &str = "ca.crt";
/// Path length of the CA certificate: it signs leaves, and no CA below it.
pub const CA_PATH_LEN: u8 = 0;
/// Lifetime of the CA certificate, in seconds (ten years).
pub const CA_VALIDITY_SECS: u64 = 10 * 365 * 24 * 3600;

/// Why a CA could not be created or loaded.
#[derive(Debug, Error)]
pub enum CaError {
    /// The name is invalid.
    #[error("CA name: {0}")]
    Name(#[from] NameError),
    /// A CA file already exists; nothing is overwritten.
    #[error("{0} already exists")]
    AlreadyExists(PathBuf),
    /// A mode is not the required one.
    #[error("{path} has mode {found:o}, must be {want:o}")]
    Mode {
        /// Path.
        path: PathBuf,
        /// Mode found.
        found: u32,
        /// Mode required.
        want: u32,
    },
    /// A path is not a plain file or directory as required.
    #[error("{0} is not a regular file or directory")]
    NotRegular(PathBuf),
    /// The key does not belong to the certificate.
    #[error("CA key does not match the CA certificate")]
    KeyMismatch,
    /// The certificate has no usable common name.
    #[error("CA certificate has no common name")]
    NoCommonName,
    /// Filesystem error.
    #[error("{path}: {source}")]
    Io {
        /// Path.
        path: PathBuf,
        /// Cause.
        source: std::io::Error,
    },
    /// X.509 library error.
    #[error("CA crypto: {0}")]
    Crypto(String),
}

/// Maps a library error into [`CaError::Crypto`].
fn crypto(e: impl std::fmt::Display) -> CaError {
    CaError::Crypto(e.to_string())
}

/// Wraps an I/O error with its path.
fn io_at(path: &Path) -> impl FnOnce(std::io::Error) -> CaError + '_ {
    move |source| CaError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The permission bits of `meta`.
fn mode_of(meta: &fs::Metadata) -> u32 {
    meta.permissions().mode() & MODE_MASK
}

/// Creates `dir` with mode 0700, or checks that an existing one has it.
fn prepare_dir(dir: &Path) -> Result<(), CaError> {
    if dir.symlink_metadata().is_ok() {
        return check_dir(dir);
    }
    DirBuilder::new()
        .mode(CA_DIR_MODE)
        .create(dir)
        .map_err(io_at(dir))?;
    // The umask can only remove bits, but the contract is the exact mode.
    fs::set_permissions(dir, fs::Permissions::from_mode(CA_DIR_MODE)).map_err(io_at(dir))
}

/// `dir` must be a real directory (not a link) with exactly mode 0700.
fn check_dir(dir: &Path) -> Result<(), CaError> {
    let meta = dir.symlink_metadata().map_err(io_at(dir))?;
    if !meta.file_type().is_dir() {
        return Err(CaError::NotRegular(dir.to_path_buf()));
    }
    let found = mode_of(&meta);
    if found != CA_DIR_MODE {
        return Err(CaError::Mode {
            path: dir.to_path_buf(),
            found,
            want: CA_DIR_MODE,
        });
    }
    Ok(())
}

/// Reads a regular file, first requiring `want` as its exact mode if given.
fn read_checked(path: &Path, want: Option<u32>) -> Result<String, CaError> {
    let meta = path.symlink_metadata().map_err(io_at(path))?;
    if !meta.file_type().is_file() {
        return Err(CaError::NotRegular(path.to_path_buf()));
    }
    if let Some(want) = want {
        let found = mode_of(&meta);
        if found != want {
            return Err(CaError::Mode {
                path: path.to_path_buf(),
                found,
                want,
            });
        }
    }
    fs::read_to_string(path).map_err(io_at(path))
}

/// Writes a new file that must not exist, with exactly `mode`.
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<(), CaError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|e| match e.kind() {
            ErrorKind::AlreadyExists => CaError::AlreadyExists(path.to_path_buf()),
            _ => CaError::Io {
                path: path.to_path_buf(),
                source: e,
            },
        })?;
    file.write_all(bytes).map_err(io_at(path))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(io_at(path))
}

/// What loading reads out of the CA certificate.
struct CertFacts {
    /// The subject's common name.
    name: String,
    /// The SubjectPublicKeyInfo, DER.
    spki_der: Vec<u8>,
    /// Whether it is `CA:TRUE` with a path length of zero.
    leaves_only: bool,
}

/// The common name, the SPKI DER and the path length rule of a PEM certificate.
fn describe_cert(cert_pem: &str) -> Result<CertFacts, CaError> {
    let (_, pem) = parse_x509_pem(cert_pem.as_bytes()).map_err(crypto)?;
    let cert = pem.parse_x509().map_err(crypto)?;
    let name = cert
        .subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .ok_or(CaError::NoCommonName)?
        .to_owned();
    validate_name(&name)?;
    let leaves_only = cert.basic_constraints().map_err(crypto)?.is_some_and(|bc| {
        bc.value.ca && bc.value.path_len_constraint == Some(u32::from(CA_PATH_LEN))
    });
    Ok(CertFacts {
        name,
        spki_der: cert.public_key().raw.to_vec(),
        leaves_only,
    })
}

/// The loaded CA.
pub struct Ca {
    pub(super) issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
    name: String,
    cert_pem: String,
    leaves_only: bool,
}

impl std::fmt::Debug for Ca {
    /// Shows the name only: the key never reaches a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ca")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Ca {
    /// Creates a new CA in `dir`: self-signed, `CA:TRUE` with a path length of
    /// zero and `keyCertSign` alone, both critical.
    ///
    /// # Errors
    /// [`CaError`]; never overwrites.
    pub fn create(dir: &Path, name: &str, now: SystemTime) -> Result<Self, CaError> {
        validate_name(name)?;
        prepare_dir(dir)?;
        let key_path = dir.join(CA_KEY_FILE);
        let cert_path = dir.join(CA_CERT_FILE);
        for path in [&key_path, &cert_path] {
            if path.symlink_metadata().is_ok() {
                return Err(CaError::AlreadyExists(path.clone()));
            }
        }

        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(crypto)?;
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params.distinguished_name.push(DnType::CommonName, name);
        // It signs guests' leaves and nothing that signs: a verifier that
        // requires `CA:TRUE, pathlen:0` (mediatore's x509pop bundle) refuses
        // anything wider.
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(CA_PATH_LEN));
        // No `cRLSign`: no revocation list is issued (removal from the bundle
        // is the revocation, ADR-0143 Decision 2).
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        params.serial_number = Some(SerialNumber::from_slice(
            Serial::random()
                .map_err(|e| CaError::Crypto(e.to_string()))?
                .as_bytes(),
        ));
        params.not_before = OffsetDateTime::from(now - CA_BACKDATE);
        params.not_after = OffsetDateTime::from(now + Duration::from_secs(CA_VALIDITY_SECS));
        let cert = params.self_signed(&key).map_err(crypto)?;

        write_new(&key_path, key.serialize_pem().as_bytes(), CA_KEY_MODE)?;
        if let Err(e) = write_new(&cert_path, cert.pem().as_bytes(), CA_CERT_MODE) {
            // Never leave a key with no certificate beside it.
            let _ = fs::remove_file(&key_path);
            return Err(e);
        }
        Self::load(dir)
    }

    /// Loads the CA in `dir`, refusing wrong modes.
    ///
    /// A certificate without the path length constraint is not refused and is
    /// never replaced here; [`Ca::signs_leaves_only`] reports it.
    ///
    /// # Errors
    /// [`CaError`].
    pub fn load(dir: &Path) -> Result<Self, CaError> {
        check_dir(dir)?;
        let key_path = dir.join(CA_KEY_FILE);
        let cert_path = dir.join(CA_CERT_FILE);
        let key_pem = read_checked(&key_path, Some(CA_KEY_MODE))?;
        let cert_pem = read_checked(&cert_path, None)?;

        let key = KeyPair::from_pem(&key_pem).map_err(crypto)?;
        let facts = describe_cert(&cert_pem)?;
        if facts.spki_der != key.subject_public_key_info() {
            return Err(CaError::KeyMismatch);
        }
        let issuer = Issuer::from_ca_cert_pem(&cert_pem, key).map_err(crypto)?;
        Ok(Self {
            issuer,
            name: facts.name,
            cert_pem,
            leaves_only: facts.leaves_only,
        })
    }

    /// The CA's common name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The CA certificate, PEM.
    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// Whether the certificate is `CA:TRUE` with a path length of zero, as
    /// every CA [`Ca::create`] makes is.
    ///
    /// `false` for a CA made before the constraint (or brought from elsewhere):
    /// it still loads and still signs, so the caller should warn that it is
    /// wider than it needs to be, and that a verifier requiring a path length
    /// of zero refuses it, until it is rotated.
    pub fn signs_leaves_only(&self) -> bool {
        self.leaves_only
    }
}

#[cfg(test)]
#[path = "ca_tests.rs"]
mod ca_tests;
