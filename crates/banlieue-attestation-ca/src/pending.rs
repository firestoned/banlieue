// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A host CA signed by an upstream PKI, by CSR (ADR-0146 Decision 7).
//!
//! The key is born on the host, as the provider's user, exactly as the
//! self-signed CA's is, and never leaves it. What leaves is a PKCS#10 request,
//! which is public. An administrator has it signed on a PKI mount of their
//! own; [`PendingCa::import`] checks what comes back and installs it.
//!
//! A pending CA is a key and a request in a directory of its own
//! (`ca.key` `0600`, `ca.csr` `0644`, directory `0700`), never in the live CA
//! directory, so an unsigned request cannot disturb a CA that is serving.
//! Nothing is installed until every check has passed; a refusal leaves the
//! pending directory and any live CA exactly as they were.

use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use p256::ecdsa::{DerSignature, SigningKey};
use p256::elliptic_curve::Generate;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rustix::fs::{AtFlags, Gid, Uid, fchmod, fchown, renameat, statat, unlinkat};
use rustix::io::Errno;
use x509_cert::Certificate;
use x509_cert::builder::{Builder, RequestBuilder};
use x509_cert::der::{DecodePem, EncodePem, pem::LineEnding};
use x509_cert::ext::pkix::{BasicConstraints, KeyUsage};
use x509_cert::name::Name;
use x509_cert::request::CertReq;
use x509_cert::spki::SubjectPublicKeyInfo;

use crate::ca::{
    AttestationCa, CA_VALIDITY, CERT_FILE, CERT_MODE, CHAIN_FILE, CN_PREFIX, CaError, DIR_MODE,
    KEY_FILE, KEY_MODE, check_secure_dir, io_err, mode_of, open_ca_dir, open_regular, read_all,
    write_new,
};
use crate::name::validate_name;

/// The request's file name, in the pending directory.
const CSR_FILE: &str = "ca.csr";
/// The furthest out an imported CA's `notAfter` may be: what the self-signed
/// CA lives. An upstream that signs for longer is asking the host to trust a
/// key for longer than the host would trust one it made itself.
pub const MAX_IMPORTED_CA_VALIDITY: Duration = CA_VALIDITY;
/// The most certificates an imported bundle may hold (the host CA, its
/// intermediates and the root): a real PKI has two or three.
pub const MAX_IMPORTED_CHAIN: usize = 8;
/// A path-length constraint of zero: the CA may sign leaves and nothing else.
const NO_FURTHER_CAS: u8 = 0;

/// A key with no certificate yet, and the request for one.
pub struct PendingCa {
    dir: PathBuf,
    common_name: String,
    csr_pem: String,
    spki: SubjectPublicKeyInfo<x509_cert::der::Any, x509_cert::der::asn1::BitString>,
}

impl std::fmt::Debug for PendingCa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingCa")
            .field("common_name", &self.common_name)
            .finish_non_exhaustive()
    }
}

impl PendingCa {
    /// Make the key and the request in `dir`: EC P-256, subject CN `common_name`.
    ///
    /// `dir` is made `0700`, `ca.key` `0600`, `ca.csr` `0644`; with `owner`
    /// they are chowned before the key is written, so it is born owned by the
    /// provider's user. No certificate is written.
    ///
    /// # Errors
    /// [`CaError::InvalidName`] (nothing is created); [`CaError::AlreadyExists`]
    /// when a key or request is already there (nothing is touched);
    /// [`CaError::Insecure`] when `dir` is a symlink; [`CaError::Io`] on a
    /// filesystem failure.
    pub fn create(
        dir: &Path,
        common_name: &str,
        owner: Option<(u32, u32)>,
    ) -> Result<Self, CaError> {
        validate_name(common_name)?;
        let dir_fd = open_ca_dir(dir, true)?;
        for file in [KEY_FILE, CSR_FILE] {
            match statat(&dir_fd, file, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => return Err(CaError::AlreadyExists),
                Err(Errno::NOENT) => {}
                Err(e) => return Err(io_err("checking for a pending request")(e.into())),
            }
        }
        fchmod(&dir_fd, mode_of(DIR_MODE))
            .map_err(|e| io_err("securing the pending directory")(e.into()))?;
        if let Some((uid, gid)) = owner {
            fchown(&dir_fd, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
                .map_err(|e| io_err("chowning the pending directory")(e.into()))?;
        }

        let key = SigningKey::generate();
        let csr_pem = build_csr(&key, common_name)?;
        let key_pem = key
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(|_| CaError::Corrupt("the new key could not be encoded"))?;
        let written = write_new(
            &dir_fd,
            KEY_FILE,
            key_pem.as_bytes(),
            KEY_MODE,
            owner,
            "writing the pending key",
        )
        .and_then(|()| {
            write_new(
                &dir_fd,
                CSR_FILE,
                csr_pem.as_bytes(),
                CERT_MODE,
                owner,
                "writing the request",
            )
        });
        if let Err(e) = written {
            let _ = unlinkat(&dir_fd, KEY_FILE, AtFlags::empty());
            let _ = unlinkat(&dir_fd, CSR_FILE, AtFlags::empty());
            return Err(e);
        }
        Self::load(dir)
    }

    /// Load the pending request in `dir`, with the checks the live CA gets:
    /// a directory closed to group and world, a `0600` key, no symlinks, a
    /// request that belongs to the key and names a valid common name.
    ///
    /// # Errors
    /// [`CaError::Insecure`], [`CaError::Corrupt`], or [`CaError::Io`] when
    /// something is missing.
    pub fn load(dir: &Path) -> Result<Self, CaError> {
        check_secure_dir(dir)?;
        let (key_file, key_meta) = open_regular(&dir.join(KEY_FILE), "opening the pending key")?;
        if std::os::unix::fs::PermissionsExt::mode(&key_meta.permissions())
            & crate::ca::PERMISSION_BITS
            != KEY_MODE
        {
            return Err(CaError::Insecure("the pending ca.key is not mode 0600"));
        }
        let key_pem = read_all(key_file, "reading the pending key")?;
        let (csr_file, _) = open_regular(&dir.join(CSR_FILE), "opening the request")?;
        let csr_pem = read_all(csr_file, "reading the request")?;

        let key = SigningKey::from_pkcs8_pem(&key_pem)
            .map_err(|_| CaError::Corrupt("the pending ca.key is not an EC P-256 PKCS#8 key"))?;
        let req = CertReq::from_pem(csr_pem.as_bytes())
            .map_err(|_| CaError::Corrupt("ca.csr is not a PEM certificate request"))?;
        let spki = SubjectPublicKeyInfo::from_key(key.verifying_key())
            .map_err(|_| CaError::Corrupt("the pending key's public key"))?;
        if req.info.public_key != spki {
            return Err(CaError::Corrupt("ca.key does not belong to ca.csr"));
        }
        let common_name = common_name_of(&req.info.subject)
            .ok_or(CaError::Corrupt("ca.csr subject is not a bare common name"))?;
        validate_name(&common_name)
            .map_err(|_| CaError::Corrupt("ca.csr common name is outside the allowed alphabet"))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            common_name,
            csr_pem,
            spki,
        })
    }

    /// The CA's common name: the issuer CN of every agent ID it will sign.
    #[must_use]
    pub fn common_name(&self) -> &str {
        &self.common_name
    }

    /// The request, PEM. Public: this is what the administrator has signed.
    #[must_use]
    pub fn csr_pem(&self) -> &str {
        &self.csr_pem
    }

    /// Check the signed certificate and install it as the live CA in `ca_dir`.
    ///
    /// `signed_pem` is the host CA certificate first, then any chain above it
    /// (PEM, as an upstream PKI returns it). Before anything is written, the
    /// first certificate must: carry the pending key's public key; be named
    /// exactly by the request's common name; be `CA:TRUE` with a path length of
    /// zero and `keyCertSign`, so it signs leaves and nothing else; be valid at
    /// `now` and expire no later than [`MAX_IMPORTED_CA_VALIDITY`] from it; and
    /// each certificate after it must be the previous one's issuer. A bundle of
    /// more than [`MAX_IMPORTED_CHAIN`] certificates is refused.
    ///
    /// The host CA alone is written as `ca.crt`; when it has an issuer above it,
    /// `ca-chain.pem` holds it and the intermediates (never a self-signed root),
    /// and the signer answers with that chain after each leaf. The pending key
    /// is then moved (not copied) into `ca_dir`. With `replace`, a live CA in
    /// `ca_dir` is swapped for the new one, after every check has passed; without
    /// it, a live CA is refused.
    ///
    /// # Errors
    /// [`CaError::Rejected`] for any failed check; [`CaError::AlreadyExists`]
    /// when a CA is live and `replace` is not set; [`CaError::Insecure`] or
    /// [`CaError::Io`] on filesystem trouble. On any error the pending directory
    /// is as it was, and a live CA is untouched.
    pub fn import(
        &self,
        ca_dir: &Path,
        signed_pem: &str,
        now: SystemTime,
        replace: bool,
        owner: Option<(u32, u32)>,
    ) -> Result<AttestationCa, CaError> {
        let chain_pem = self.check_signed(signed_pem, now)?;
        let host_pem = first_certificate_pem(signed_pem)?;

        let ca_fd = open_ca_dir(ca_dir, true)?;
        let live = [KEY_FILE, CERT_FILE]
            .iter()
            .any(|f| statat(&ca_fd, *f, AtFlags::SYMLINK_NOFOLLOW).is_ok());
        if live && !replace {
            return Err(CaError::AlreadyExists);
        }
        if live {
            AttestationCa::remove_files(ca_dir)?;
        }
        fchmod(&ca_fd, mode_of(DIR_MODE))
            .map_err(|e| io_err("securing the CA directory")(e.into()))?;
        if let Some((uid, gid)) = owner {
            fchown(&ca_fd, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
                .map_err(|e| io_err("chowning the CA directory")(e.into()))?;
        }

        let pending_fd = open_ca_dir(&self.dir, false)?;
        let installed = write_new(
            &ca_fd,
            CERT_FILE,
            host_pem.as_bytes(),
            CERT_MODE,
            owner,
            "writing ca.crt",
        )
        .and_then(|()| match &chain_pem {
            Some(chain) => write_new(
                &ca_fd,
                CHAIN_FILE,
                chain.as_bytes(),
                CERT_MODE,
                owner,
                "writing ca-chain.pem",
            ),
            None => Ok(()),
        })
        .and_then(|()| {
            renameat(&pending_fd, KEY_FILE, &ca_fd, KEY_FILE)
                .map_err(|e| io_err("moving the key into the CA directory")(e.into()))
        });
        if let Err(e) = installed {
            let _ = unlinkat(&ca_fd, CERT_FILE, AtFlags::empty());
            let _ = unlinkat(&ca_fd, CHAIN_FILE, AtFlags::empty());
            return Err(e);
        }
        let _ = unlinkat(&pending_fd, CSR_FILE, AtFlags::empty());
        let _ = fs::remove_dir(&self.dir);
        AttestationCa::load(ca_dir)
    }

    /// Every check on a signed bundle, before anything is written. Returns the
    /// contents for `ca-chain.pem` (`None` for a self-signed host CA).
    fn check_signed(&self, signed_pem: &str, now: SystemTime) -> Result<Option<String>, CaError> {
        let certs = Certificate::load_pem_chain(signed_pem.as_bytes())
            .map_err(|_| CaError::Rejected("not a PEM certificate chain"))?;
        let host = certs
            .first()
            .ok_or(CaError::Rejected("no certificate in the file"))?;
        if certs.len() > MAX_IMPORTED_CHAIN {
            return Err(CaError::Rejected("more certificates than a chain may hold"));
        }
        let tbs = host.tbs_certificate();
        if tbs.subject_public_key_info() != &self.spki {
            return Err(CaError::Rejected(
                "the certificate is for a different key than the pending one",
            ));
        }
        if common_name_of(tbs.subject()).as_deref() != Some(self.common_name.as_str()) {
            return Err(CaError::Rejected(
                "the certificate's common name is not the request's",
            ));
        }
        match tbs.get_extension::<BasicConstraints>() {
            Ok(Some((_, bc))) if bc.ca && bc.path_len_constraint == Some(NO_FURTHER_CAS) => {}
            Ok(Some((_, bc))) if bc.ca => {
                return Err(CaError::Rejected(
                    "the CA may sign further CAs; its path length must be zero",
                ));
            }
            _ => return Err(CaError::Rejected("the certificate is not a CA")),
        }
        match tbs.get_extension::<KeyUsage>() {
            Ok(Some((_, ku))) if ku.key_cert_sign() => {}
            _ => return Err(CaError::Rejected("the certificate lacks keyCertSign")),
        }
        let validity = tbs.validity();
        if now < validity.not_before.to_system_time() {
            return Err(CaError::Rejected("the certificate is not valid yet"));
        }
        let not_after = validity.not_after.to_system_time();
        if now >= not_after {
            return Err(CaError::Rejected("the certificate has expired"));
        }
        if not_after > now + MAX_IMPORTED_CA_VALIDITY {
            return Err(CaError::Rejected(
                "the certificate is valid for longer than a host CA may be",
            ));
        }
        for pair in certs.windows(2) {
            if pair[0].tbs_certificate().issuer() != pair[1].tbs_certificate().subject() {
                return Err(CaError::Rejected(
                    "a certificate is not issued by the one after it",
                ));
            }
        }
        chain_file_contents(&certs)
    }
}

/// `ca-chain.pem`: the host CA and the intermediates above it, never a
/// self-signed root. `None` when the host CA is itself self-signed.
fn chain_file_contents(certs: &[Certificate]) -> Result<Option<String>, CaError> {
    let self_signed =
        |c: &Certificate| c.tbs_certificate().issuer() == c.tbs_certificate().subject();
    if self_signed(&certs[0]) {
        return Ok(None);
    }
    let mut out = String::new();
    for cert in certs.iter().filter(|c| !self_signed(c)) {
        let pem = cert
            .to_pem(LineEnding::LF)
            .map_err(|_| CaError::Rejected("a certificate could not be re-encoded"))?;
        out.push_str(&pem);
    }
    Ok(Some(out))
}

/// The first PEM certificate block in `bundle`, as text.
fn first_certificate_pem(bundle: &str) -> Result<String, CaError> {
    const END: &str = "-----END CERTIFICATE-----";
    let end = bundle
        .find(END)
        .ok_or(CaError::Rejected("no certificate in the file"))?;
    let start = bundle
        .find("-----BEGIN CERTIFICATE-----")
        .ok_or(CaError::Rejected("no certificate in the file"))?;
    Ok(format!("{}\n", bundle[start..end + END.len()].trim_end()))
}

fn build_csr(key: &SigningKey, common_name: &str) -> Result<String, CaError> {
    fn corrupt<E>(_: E) -> CaError {
        CaError::Corrupt("the request could not be built")
    }
    let name = Name::from_str(&format!("{CN_PREFIX}{common_name}")).map_err(corrupt)?;
    RequestBuilder::new(name)
        .map_err(corrupt)?
        .build::<_, DerSignature>(key)
        .map_err(corrupt)?
        .to_pem(LineEnding::LF)
        .map_err(corrupt)
}

/// The common name of a name that is exactly `CN=<name>`, or `None`.
fn common_name_of(name: &Name) -> Option<String> {
    name.to_string().strip_prefix(CN_PREFIX).map(str::to_owned)
}

#[cfg(test)]
#[path = "pending_tests.rs"]
mod pending_tests;
