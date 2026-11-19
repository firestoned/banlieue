// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The attestation CA: an EC P-256 key and a self-signed certificate on disk
//! (ADR-0143 Decision 1).
//!
//! Layout under the CA directory: `ca.key` (mode `0600`, PKCS#8 PEM) and
//! `ca.crt` (mode `0644`); the directory is `0700` so a guest's uid cannot
//! traverse it. A CA is never overwritten, and never rotated here: a new CA
//! needs a new name, which is the caller's decision.

use std::fs::{self, DirBuilder, File};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use p256::ecdsa::{DerSignature, SigningKey};
use p256::elliptic_curve::Generate;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rustix::fs::{
    AtFlags, FileType, Gid, Mode, OFlags, Uid, fchmod, fchown, fstat, mkdirat, open, openat,
    statat, unlinkat,
};
use rustix::io::Errno;
use thiserror::Error;
use x509_cert::Certificate;
use x509_cert::builder::profile::BuilderProfile;
use x509_cert::builder::{Builder, CertificateBuilder, Result as BuildResult};
use x509_cert::certificate::TbsCertificate;
use x509_cert::der::{DecodePem, EncodePem, pem::LineEnding};
use x509_cert::ext::pkix::{BasicConstraints, KeyUsage, KeyUsages};
use x509_cert::ext::{Extension, ToExtension};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::{SubjectPublicKeyInfo, SubjectPublicKeyInfoRef};
use x509_cert::time::{Time, Validity};

use crate::name::{NameError, validate_name};

const KEY_FILE: &str = "ca.key";
const CERT_FILE: &str = "ca.crt";
const DIR_MODE: u32 = 0o700;
const KEY_MODE: u32 = 0o600;
const CERT_MODE: u32 = 0o644;
/// Group and world permission bits: none may be set on the directory.
const GROUP_WORLD_BITS: u32 = 0o077;
const PERMISSION_BITS: u32 = 0o7777;
const CN_PREFIX: &str = "CN=";
/// A CA lives until it is explicitly rotated; ten years is "long".
const CA_VALIDITY: Duration = Duration::from_secs(CA_VALIDITY_DAYS * SECONDS_PER_DAY);
const CA_VALIDITY_DAYS: u64 = 3650;
const SECONDS_PER_DAY: u64 = 86_400;
/// Tolerance for a verifier whose clock is behind the host's.
const CA_BACKDATE: Duration = Duration::from_secs(SECONDS_PER_HOUR);
const SECONDS_PER_HOUR: u64 = 3600;
/// Bytes of randomness in the CA's serial number.
const CA_SERIAL_BYTES: usize = 16;

/// Why creating or loading the CA failed. Never carries key material.
#[derive(Debug, Error)]
pub enum CaError {
    /// The common name is outside the allowed alphabet (refused, never replaced).
    #[error("invalid CA name: {0}")]
    InvalidName(#[from] NameError),
    /// A CA already exists in the directory; it is never overwritten.
    #[error("a CA already exists in this directory")]
    AlreadyExists,
    /// A filesystem operation failed.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: &'static str,
        /// The OS error.
        source: io::Error,
    },
    /// Permissions are too open, or a path is a symlink.
    #[error("insecure CA storage: {0}")]
    Insecure(&'static str),
    /// The stored material is unusable.
    #[error("corrupt CA material: {0}")]
    Corrupt(&'static str),
}

/// A rustix `Mode` from permission bits (the raw type is `u16` on macOS, `u32` on Linux).
fn mode_of(bits: u32) -> Mode {
    Mode::from_bits_truncate(bits as _)
}

fn io_err(context: &'static str) -> impl FnOnce(io::Error) -> CaError {
    move |source| CaError::Io { context, source }
}

/// A loaded attestation CA: the signing key, its certificate and the name.
pub struct AttestationCa {
    key: SigningKey,
    cert: Certificate,
    cert_pem: String,
    common_name: String,
}

impl std::fmt::Debug for AttestationCa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttestationCa")
            .field("common_name", &self.common_name)
            .finish_non_exhaustive()
    }
}

/// Profile of the self-signed CA certificate: `CA:TRUE`, `keyCertSign`, both critical.
struct CaProfile {
    subject: Name,
}

impl BuilderProfile for CaProfile {
    fn get_issuer(&self, subject: &Name) -> Name {
        subject.clone()
    }

    fn get_subject(&self) -> Name {
        self.subject.clone()
    }

    fn build_extensions(
        &self,
        _spk: SubjectPublicKeyInfoRef<'_>,
        _issuer_spk: SubjectPublicKeyInfoRef<'_>,
        tbs: &TbsCertificate,
    ) -> BuildResult<Vec<Extension>> {
        let mut exts: Vec<Extension> = Vec::new();
        let basic = BasicConstraints {
            ca: true,
            path_len_constraint: Some(0),
        };
        exts.push((true, &basic).to_extension(tbs.subject(), &exts)?);
        let usage = KeyUsage(KeyUsages::KeyCertSign.into());
        exts.push((true, &usage).to_extension(tbs.subject(), &exts)?);
        Ok(exts)
    }
}

fn random_serial(bytes: usize) -> Result<SerialNumber, getrandom::Error> {
    loop {
        let mut buf = vec![0u8; bytes];
        getrandom::fill(&mut buf)?;
        if buf.iter().any(|b| *b != 0)
            && let Ok(serial) = SerialNumber::new(&buf)
        {
            return Ok(serial);
        }
    }
}

/// Open `path` for reading, refusing a symlink, and return the handle with its
/// metadata taken from the handle (not from the path), so a swap after the check
/// cannot change what was inspected.
fn open_regular(path: &Path, what: &'static str) -> Result<(File, fs::Metadata), CaError> {
    let before = fs::symlink_metadata(path).map_err(io_err(what))?;
    if !before.file_type().is_file() {
        return Err(CaError::Insecure(
            "a CA file is a symlink or not a regular file",
        ));
    }
    let file = File::open(path).map_err(io_err(what))?;
    let meta = file.metadata().map_err(io_err(what))?;
    if (meta.dev(), meta.ino()) != (before.dev(), before.ino()) {
        return Err(CaError::Insecure("a CA file changed while it was opened"));
    }
    Ok((file, meta))
}

fn read_all(mut file: File, what: &'static str) -> Result<String, CaError> {
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(io_err(what))?;
    Ok(text)
}

impl AttestationCa {
    /// Create a new CA in `dir`: an EC P-256 key and a self-signed certificate
    /// whose common name is `common_name`.
    ///
    /// The directory is made `0700`, `ca.key` `0600`, `ca.crt` `0644`. When
    /// `owner` is given, the directory and both files are chowned to that
    /// (uid, gid) before the key is written, so the key is born with the right
    /// owner (the installer runs as root and the provider as its own user).
    ///
    /// # Errors
    /// [`CaError::InvalidName`] when the name is outside letters, digits, dots,
    /// dashes and underscores (nothing is created); [`CaError::AlreadyExists`]
    /// when `ca.key` or `ca.crt` is already there (nothing is touched);
    /// [`CaError::Io`] on a filesystem failure.
    pub fn create(
        dir: &Path,
        common_name: &str,
        owner: Option<(u32, u32)>,
    ) -> Result<Self, CaError> {
        validate_name(common_name)?;
        let dir_fd = open_ca_dir(dir, true)?;
        for file in [KEY_FILE, CERT_FILE] {
            match statat(&dir_fd, file, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => return Err(CaError::AlreadyExists),
                Err(Errno::NOENT) => {}
                Err(e) => return Err(io_err("checking for an existing CA")(e.into())),
            }
        }
        // Everything below acts on the handle, never on the path again.
        fchmod(&dir_fd, mode_of(DIR_MODE))
            .map_err(|e| io_err("securing the CA directory")(e.into()))?;
        if let Some((uid, gid)) = owner {
            fchown(&dir_fd, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
                .map_err(|e| io_err("chowning the CA directory")(e.into()))?;
        }

        let key = SigningKey::generate();
        let cert_pem = Self::self_signed(&key, common_name)?;
        let key_pem = key
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(|_| CaError::Corrupt("the new key could not be encoded"))?;

        let written = Self::write_files(&dir_fd, key_pem.as_bytes(), cert_pem.as_bytes(), owner);
        if let Err(e) = written {
            // Never leave half a CA: a lone key would block the next create.
            let _ = unlinkat(&dir_fd, KEY_FILE, AtFlags::empty());
            let _ = unlinkat(&dir_fd, CERT_FILE, AtFlags::empty());
            return Err(e);
        }
        Self::load(dir)
    }

    fn self_signed(key: &SigningKey, common_name: &str) -> Result<String, CaError> {
        fn corrupt<E>(_: E) -> CaError {
            CaError::Corrupt("the CA certificate could not be built")
        }
        let name = Name::from_str(&format!("{CN_PREFIX}{common_name}")).map_err(corrupt)?;
        let now = SystemTime::now();
        let validity = Validity::new(
            Time::try_from(now - CA_BACKDATE).map_err(corrupt)?,
            Time::try_from(now + CA_VALIDITY).map_err(corrupt)?,
        );
        let serial = random_serial(CA_SERIAL_BYTES).map_err(|e| CaError::Io {
            context: "reading randomness",
            source: io::Error::other(e),
        })?;
        let spki = SubjectPublicKeyInfo::from_key(key.verifying_key()).map_err(corrupt)?;
        let builder = CertificateBuilder::new(CaProfile { subject: name }, serial, validity, spki)
            .map_err(|_| CaError::Corrupt("the CA certificate could not be built"))?;
        let cert = builder
            .build::<_, DerSignature>(key)
            .map_err(|_| CaError::Corrupt("the CA certificate could not be signed"))?;
        cert.to_pem(LineEnding::LF).map_err(corrupt)
    }

    fn write_files(
        dir: &OwnedFd,
        key_pem: &[u8],
        cert_pem: &[u8],
        owner: Option<(u32, u32)>,
    ) -> Result<(), CaError> {
        write_new(dir, KEY_FILE, key_pem, KEY_MODE, owner, "writing ca.key")?;
        write_new(dir, CERT_FILE, cert_pem, CERT_MODE, owner, "writing ca.crt")
    }

    /// Remove `ca.key` and `ca.crt` from `dir` (rotation). Both are unlinked
    /// relative to a handle on the directory opened without following a
    /// symlink, so a link planted in either name is removed, never followed,
    /// and a symlinked directory is refused. A missing file is success.
    ///
    /// # Errors
    /// [`CaError::Insecure`] when `dir` is a symlink or not a directory;
    /// [`CaError::Io`] on any other filesystem failure.
    pub fn remove_files(dir: &Path) -> Result<(), CaError> {
        let dir_fd = open_ca_dir(dir, false)?;
        for file in [KEY_FILE, CERT_FILE] {
            match unlinkat(&dir_fd, file, AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => {}
                Err(e) => return Err(io_err("removing a CA file")(e.into())),
            }
        }
        Ok(())
    }

    /// Load the CA from `dir`.
    ///
    /// # Errors
    /// [`CaError::Insecure`] when the key is not mode `0600`, the directory is
    /// accessible to group or world, or a file is a symlink;
    /// [`CaError::Corrupt`] when the key or certificate does not parse, the
    /// certificate's name is outside the alphabet, or the key does not belong
    /// to the certificate; [`CaError::Io`] when something is missing.
    pub fn load(dir: &Path) -> Result<Self, CaError> {
        let dir_meta = fs::symlink_metadata(dir).map_err(io_err("reading the CA directory"))?;
        if !dir_meta.file_type().is_dir() {
            return Err(CaError::Insecure(
                "the CA directory is a symlink or not a directory",
            ));
        }
        if dir_meta.permissions().mode() & GROUP_WORLD_BITS != 0 {
            return Err(CaError::Insecure(
                "the CA directory is accessible to group or world",
            ));
        }
        let (key_file, key_meta) = open_regular(&dir.join(KEY_FILE), "opening ca.key")?;
        if key_meta.permissions().mode() & PERMISSION_BITS != KEY_MODE {
            return Err(CaError::Insecure("ca.key is not mode 0600"));
        }
        let key_pem = read_all(key_file, "reading ca.key")?;
        let (cert_file, _) = open_regular(&dir.join(CERT_FILE), "opening ca.crt")?;
        let cert_pem = read_all(cert_file, "reading ca.crt")?;

        let key = SigningKey::from_pkcs8_pem(&key_pem)
            .map_err(|_| CaError::Corrupt("ca.key is not an EC P-256 PKCS#8 key"))?;
        let cert = Certificate::from_pem(cert_pem.as_bytes())
            .map_err(|_| CaError::Corrupt("ca.crt is not a PEM certificate"))?;
        let own = SubjectPublicKeyInfo::from_key(key.verifying_key())
            .map_err(|_| CaError::Corrupt("ca.key public key"))?;
        if cert.tbs_certificate().subject_public_key_info() != &own {
            return Err(CaError::Corrupt("ca.key does not belong to ca.crt"));
        }
        let subject = cert.tbs_certificate().subject().to_string();
        let common_name = subject
            .strip_prefix(CN_PREFIX)
            .ok_or(CaError::Corrupt("ca.crt subject is not a bare common name"))?
            .to_owned();
        validate_name(&common_name)
            .map_err(|_| CaError::Corrupt("ca.crt common name is outside the allowed alphabet"))?;
        Ok(Self {
            key,
            cert,
            cert_pem,
            common_name,
        })
    }

    /// The CA certificate's common name: the issuer CN of every agent ID it signs.
    #[must_use]
    pub fn common_name(&self) -> &str {
        &self.common_name
    }

    /// The CA certificate, PEM (public; this is what the administrator asserts).
    #[must_use]
    pub fn certificate_pem(&self) -> &str {
        &self.cert_pem
    }

    pub(crate) fn signing_key(&self) -> &SigningKey {
        &self.key
    }

    pub(crate) fn issuer_name(&self) -> &Name {
        self.cert.tbs_certificate().subject()
    }
}

/// Create `path` exclusively with `mode`, chown it before any content is written,
/// then write. The mode is set explicitly afterwards because `open` applies the umask.
fn write_new(
    dir: &OwnedFd,
    name: &str,
    content: &[u8],
    mode: u32,
    owner: Option<(u32, u32)>,
    what: &'static str,
) -> Result<(), CaError> {
    let fd = openat(
        dir,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        mode_of(mode),
    )
    .map_err(|e| io_err(what)(e.into()))?;
    if let Some((uid, gid)) = owner {
        fchown(&fd, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
            .map_err(|e| io_err(what)(e.into()))?;
    }
    fchmod(&fd, mode_of(mode)).map_err(|e| io_err(what)(e.into()))?;
    let mut file = File::from(fd);
    file.write_all(content).map_err(io_err(what))?;
    file.sync_all().map_err(io_err(what))
}

/// Open the CA directory `O_DIRECTORY | O_NOFOLLOW`, creating it `0700` first
/// when `create` is set. The final component is never followed, so a symlink
/// planted by the provider's user is refused instead of being chmodded or
/// chowned by the root installer. The parent chain is resolved normally (it is
/// root's); the opened handle is then checked to be a directory.
fn open_ca_dir(dir: &Path, create: bool) -> Result<OwnedFd, CaError> {
    let name = dir
        .file_name()
        .ok_or(CaError::Insecure("the CA directory has no final component"))?;
    let parent = match dir.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if create {
        DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(parent)
            .map_err(io_err("creating the CA parent directory"))?;
    }
    let parent_fd = open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| io_err("opening the CA parent directory")(e.into()))?;
    if create {
        match mkdirat(&parent_fd, name, mode_of(DIR_MODE)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(e) => return Err(io_err("creating the CA directory")(e.into())),
        }
    }
    let fd = match openat(
        &parent_fd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        // Linux reports a symlink under O_DIRECTORY|O_NOFOLLOW as ENOTDIR, BSD as ELOOP.
        Err(Errno::LOOP | Errno::NOTDIR) => {
            return Err(CaError::Insecure(
                "the CA directory is a symlink or not a directory",
            ));
        }
        Err(e) => return Err(io_err("opening the CA directory")(e.into())),
    };
    let st = fstat(&fd).map_err(|e| io_err("inspecting the CA directory")(e.into()))?;
    if FileType::from_raw_mode(st.st_mode) != FileType::Directory {
        return Err(CaError::Insecure(
            "the CA directory is a symlink or not a directory",
        ));
    }
    Ok(fd)
}

#[cfg(test)]
#[path = "ca_tests.rs"]
mod ca_tests;
