// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The leaf certificate: what the signer says, not what the guest asked for.

use std::time::{Duration, SystemTime};

use rcgen::{
    CertificateParams, DistinguishedName, DnType, IsCa, KeyUsagePurpose, SerialNumber,
    SubjectPublicKeyInfo,
};
use thiserror::Error;
use time::OffsetDateTime;

use super::ca::Ca;
use super::csr::GuestPublicKey;
use super::name::{NameError, validate_name};

/// The shortest validity issued (renewal is at half life).
pub const MIN_VALIDITY: Duration = Duration::from_secs(300);
/// The longest validity issued.
pub const MAX_VALIDITY: Duration = Duration::from_secs(7 * 24 * 3600);

/// Length of a serial in bytes (128 bits).
const SERIAL_LEN: usize = 16;
/// Clears the sign bit of the first serial byte.
const SERIAL_TOP_BIT_MASK: u8 = 0x7f;
/// How far `notBefore` is set before now, to absorb guest clock skew.
const NOT_BEFORE_BACKDATE: Duration = Duration::from_secs(300);

/// A certificate serial: 128 bits, positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Serial([u8; SERIAL_LEN]);

impl Serial {
    /// A serial from bytes; the top bit is cleared so it is a positive integer.
    pub fn from_bytes(bytes: [u8; SERIAL_LEN]) -> Self {
        let mut bytes = bytes;
        bytes[0] &= SERIAL_TOP_BIT_MASK;
        Self(bytes)
    }
    /// A serial from the OS RNG.
    ///
    /// # Errors
    /// [`IssueError::Rng`] if the OS RNG fails.
    pub fn random() -> Result<Self, IssueError> {
        let mut bytes = [0_u8; SERIAL_LEN];
        getrandom::fill(&mut bytes).map_err(|_| IssueError::Rng)?;
        Ok(Self::from_bytes(bytes))
    }
    /// The bytes.
    pub fn as_bytes(&self) -> &[u8; SERIAL_LEN] {
        &self.0
    }
}

/// Why issuing failed.
#[derive(Debug, Error)]
pub enum IssueError {
    /// The subject is not a valid name.
    #[error("subject: {0}")]
    Subject(#[from] NameError),
    /// The validity is outside [`MIN_VALIDITY`], [`MAX_VALIDITY`].
    #[error("validity out of range")]
    Validity,
    /// The OS RNG failed.
    #[error("OS RNG failed")]
    Rng,
    /// The X.509 library refused.
    #[error("certificate: {0}")]
    Crypto(String),
}

/// Maps a library error into [`IssueError::Crypto`].
fn crypto(e: impl std::fmt::Display) -> IssueError {
    IssueError::Crypto(e.to_string())
}

/// Issues a leaf for `key` with subject CN `subject`, as PEM.
///
/// # Errors
/// [`IssueError`].
pub fn issue(
    ca: &Ca,
    subject: &str,
    key: &GuestPublicKey,
    validity: Duration,
    now: SystemTime,
    serial: Serial,
) -> Result<String, IssueError> {
    validate_name(subject)?;
    if !(MIN_VALIDITY..=MAX_VALIDITY).contains(&validity) {
        return Err(IssueError::Validity);
    }
    let spki = SubjectPublicKeyInfo::from_der(key.as_der()).map_err(crypto)?;

    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params.distinguished_name.push(DnType::CommonName, subject);
    // CA:FALSE is forced: SPIRE does not enforce it, so the signer must.
    params.is_ca = IsCa::ExplicitNoCa;
    // SPIRE requires digitalSignature; nothing else is granted.
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = Vec::new();
    params.subject_alt_names = Vec::new();
    params.serial_number = Some(SerialNumber::from_slice(serial.as_bytes()));
    params.not_before = OffsetDateTime::from(now - NOT_BEFORE_BACKDATE);
    params.not_after = OffsetDateTime::from(now + validity);

    let cert = params.signed_by(&spki, &ca.issuer).map_err(crypto)?;
    Ok(cert.pem())
}

#[cfg(test)]
#[path = "issue_tests.rs"]
mod issue_tests;
