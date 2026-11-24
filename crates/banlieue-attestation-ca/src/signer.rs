// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The signer: CSR in, certificate out, under the rules of ADR-0143 Decision 4.

use std::str::FromStr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{DerSignature, Signature, VerifyingKey};
use thiserror::Error;
use x509_cert::builder::profile::BuilderProfile;
use x509_cert::builder::{Builder, CertificateBuilder, Result as BuildResult};
use x509_cert::certificate::TbsCertificate;
use x509_cert::der::asn1::ObjectIdentifier;
use x509_cert::der::{DecodePem, Encode, EncodePem, pem::LineEnding};
use x509_cert::ext::pkix::{BasicConstraints, KeyUsage, KeyUsages};
use x509_cert::ext::{Extension, ToExtension};
use x509_cert::name::Name;
use x509_cert::request::CertReq;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::{SubjectPublicKeyInfo, SubjectPublicKeyInfoRef};
use x509_cert::time::{Time, Validity};

use crate::name::validate_name;
use crate::ratelimit::RateLimiter;
use crate::{AttestationCa, RateLimit};

/// `id-ecPublicKey`.
const OID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
/// `prime256v1` / `secp256r1`, the only curve accepted.
const OID_PRIME256V1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
/// `ecdsa-with-SHA256`, the only CSR signature algorithm verified.
const OID_ECDSA_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");
const CN_PREFIX: &str = "CN=";
/// Bytes of randomness in a leaf certificate's serial number (128 bits).
const SERIAL_BYTES: usize = 16;

/// Largest CSR accepted, in bytes.
pub const MAX_CSR_BYTES: usize = 4096;
/// Shortest certificate lifetime.
pub const MIN_CERTIFICATE_TTL: Duration = Duration::from_secs(300);
/// Longest certificate lifetime.
pub const MAX_CERTIFICATE_TTL: Duration = Duration::from_secs(MAX_TTL_DAYS * SECONDS_PER_DAY);
const MAX_TTL_DAYS: u64 = 7;
const SECONDS_PER_DAY: u64 = 86_400;

/// Why a request was refused.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum SignError {
    /// Over the size cap.
    #[error("request too large")]
    TooLarge,
    /// Not a PEM CSR.
    #[error("malformed request")]
    Malformed,
    /// Not an EC P-256 key.
    #[error("unsupported key type")]
    BadKeyType,
    /// The CSR's own signature does not verify.
    #[error("request signature invalid")]
    BadSignature,
    /// Too many requests for this subject.
    #[error("rate limited")]
    RateLimited,
    /// The subject is outside the name alphabet.
    #[error("invalid subject")]
    InvalidSubject,
    /// The CA certificate is past its `notAfter`: whatever it signed now would
    /// verify nowhere (ADR-0146 Decision 7).
    #[error("the CA has expired")]
    CaExpired,
    /// Issuing failed for a reason that is not the request's.
    #[error("internal error")]
    Internal,
}

/// Why a [`Signer`] could not be configured.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SignerConfigError {
    /// TTL outside the bounds.
    #[error("certificate ttl outside the allowed bounds")]
    TtlOutOfRange,
    /// A zero rate limit would refuse everything.
    #[error("rate limit must allow at least one request over a nonzero window")]
    RateLimitInvalid,
}

/// Signs guest certificates for one CA under the rules of ADR-0143 Decision 4.
pub struct Signer {
    ca: AttestationCa,
    ttl: Duration,
    limiter: Mutex<RateLimiter>,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer")
            .field("ca", &self.ca)
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

/// Profile of an issued certificate: `CA:FALSE` and `digitalSignature` only, both critical.
struct LeafProfile {
    issuer: Name,
    subject: Name,
}

impl BuilderProfile for LeafProfile {
    fn get_issuer(&self, _subject: &Name) -> Name {
        self.issuer.clone()
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
            ca: false,
            path_len_constraint: None,
        };
        exts.push((true, &basic).to_extension(tbs.subject(), &exts)?);
        let usage = KeyUsage(KeyUsages::DigitalSignature.into());
        exts.push((true, &usage).to_extension(tbs.subject(), &exts)?);
        Ok(exts)
    }
}

/// The request's public key, only if it is EC P-256; nothing else of the CSR is read.
fn p256_key(req: &CertReq) -> Result<VerifyingKey, SignError> {
    let spki = &req.info.public_key;
    if spki.algorithm.oid != OID_EC_PUBLIC_KEY {
        return Err(SignError::BadKeyType);
    }
    let curve = spki
        .algorithm
        .parameters
        .as_ref()
        .and_then(|p| p.decode_as::<ObjectIdentifier>().ok());
    if curve != Some(OID_PRIME256V1) {
        return Err(SignError::BadKeyType);
    }
    let point = spki
        .subject_public_key
        .as_bytes()
        .ok_or(SignError::BadKeyType)?;
    VerifyingKey::from_sec1_bytes(point).map_err(|_| SignError::BadKeyType)
}

/// Verify the CSR's own signature (proof the requester holds the private key).
fn check_self_signature(req: &CertReq, key: &VerifyingKey) -> Result<(), SignError> {
    if req.algorithm.oid != OID_ECDSA_SHA256 {
        return Err(SignError::BadSignature);
    }
    let der = req.signature.as_bytes().ok_or(SignError::BadSignature)?;
    let signature = Signature::from_der(der).map_err(|_| SignError::BadSignature)?;
    let signed = req.info.to_der().map_err(|_| SignError::Malformed)?;
    key.verify(&signed, &signature)
        .map_err(|_| SignError::BadSignature)
}

fn random_serial() -> Result<SerialNumber, SignError> {
    loop {
        let mut buf = [0u8; SERIAL_BYTES];
        getrandom::fill(&mut buf).map_err(|_| SignError::Internal)?;
        if buf.iter().any(|b| *b != 0) {
            return SerialNumber::new(&buf).map_err(|_| SignError::Internal);
        }
    }
}

impl Signer {
    /// Build a signer issuing certificates valid for `ttl`, rate limited per subject.
    ///
    /// # Errors
    /// [`SignerConfigError::TtlOutOfRange`] when `ttl` is outside
    /// [`MIN_CERTIFICATE_TTL`]..=[`MAX_CERTIFICATE_TTL`];
    /// [`SignerConfigError::RateLimitInvalid`] for a limit that refuses everything.
    pub fn new(
        ca: AttestationCa,
        ttl: Duration,
        limit: RateLimit,
    ) -> Result<Self, SignerConfigError> {
        if !(MIN_CERTIFICATE_TTL..=MAX_CERTIFICATE_TTL).contains(&ttl) {
            return Err(SignerConfigError::TtlOutOfRange);
        }
        if limit.max_requests == 0 || limit.window.is_zero() {
            return Err(SignerConfigError::RateLimitInvalid);
        }
        Ok(Self {
            ca,
            ttl,
            limiter: Mutex::new(RateLimiter::new(limit)),
        })
    }

    /// The CA this signer issues under.
    #[must_use]
    pub fn ca(&self) -> &AttestationCa {
        &self.ca
    }

    /// Sign one request: a PEM CSR in, one PEM certificate out.
    ///
    /// Only the CSR's EC P-256 public key is used; its subject and extensions
    /// are discarded. The certificate's subject CN is `subject`, `now` starts
    /// its validity (and the rate-limit window), and its issuer is the CA.
    /// Neither the CSR nor the certificate is ever logged or put in an error.
    ///
    /// # Errors
    /// [`SignError`]: too large (checked first, before any parsing), the CA
    /// expired, invalid
    /// subject, rate limited, malformed, wrong key type, bad CSR signature, or
    /// an internal failure.
    pub fn sign(
        &self,
        csr_pem: &[u8],
        subject: &str,
        now: SystemTime,
    ) -> Result<String, SignError> {
        if csr_pem.len() > MAX_CSR_BYTES {
            return Err(SignError::TooLarge);
        }
        // Before the limiter, so a refused request costs the guest no slot.
        if self.ca.is_expired(now) {
            return Err(SignError::CaExpired);
        }
        validate_name(subject).map_err(|_| SignError::InvalidSubject)?;
        let admitted = self
            .limiter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .admit(subject, now);
        if !admitted {
            return Err(SignError::RateLimited);
        }
        let req = CertReq::from_pem(csr_pem).map_err(|_| SignError::Malformed)?;
        let key = p256_key(&req)?;
        check_self_signature(&req, &key)?;
        self.issue(&key, subject, now)
    }

    fn issue(
        &self,
        key: &VerifyingKey,
        subject: &str,
        now: SystemTime,
    ) -> Result<String, SignError> {
        let secs = now
            .duration_since(UNIX_EPOCH)
            .map_err(|_| SignError::Internal)?
            .as_secs();
        let not_before = UNIX_EPOCH + Duration::from_secs(secs);
        let validity = Validity::new(
            Time::try_from(not_before).map_err(|_| SignError::Internal)?,
            Time::try_from(not_before + self.ttl).map_err(|_| SignError::Internal)?,
        );
        let subject_name =
            Name::from_str(&format!("{CN_PREFIX}{subject}")).map_err(|_| SignError::Internal)?;
        let profile = LeafProfile {
            issuer: self.ca.issuer_name().clone(),
            subject: subject_name,
        };
        // Rebuilt from the parsed key, never copied from the request.
        let spki = SubjectPublicKeyInfo::from_key(key).map_err(|_| SignError::Internal)?;
        let builder = CertificateBuilder::new(profile, random_serial()?, validity, spki)
            .map_err(|_| SignError::Internal)?;
        let cert = builder
            .build::<_, DerSignature>(self.ca.signing_key())
            .map_err(|_| SignError::Internal)?;
        let leaf = cert
            .to_pem(LineEnding::LF)
            .map_err(|_| SignError::Internal)?;
        // A CA signed by an upstream PKI answers with its chain too, so the
        // guest's agent can present the intermediates (ADR-0146 Decision 7).
        Ok(format!("{leaf}{}", self.ca.chain_pem()))
    }
}

#[cfg(test)]
#[path = "signer_tests.rs"]
mod signer_tests;
