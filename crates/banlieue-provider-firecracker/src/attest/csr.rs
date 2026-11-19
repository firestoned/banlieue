// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Reading a guest's certificate request down to its public key.

use thiserror::Error;
use x509_parser::der_parser::oid;
use x509_parser::pem::parse_x509_pem;
use x509_parser::prelude::{FromDer, SubjectPublicKeyInfo, X509CertificationRequest};

/// The PEM label `openssl req` writes.
const PEM_LABEL: &str = "CERTIFICATE REQUEST";
/// The label older `openssl req -new` wrote.
const PEM_LABEL_LEGACY: &str = "NEW CERTIFICATE REQUEST";
/// Length of an uncompressed P-256 point: tag byte plus two 32 byte coordinates.
const P256_POINT_LEN: usize = 65;
/// The SEC1 tag of an uncompressed point.
const UNCOMPRESSED_POINT_TAG: u8 = 0x04;

/// The largest request accepted, in bytes (a real one is about 363).
pub const MAX_CSR_BYTES: usize = 4096;

/// A guest's EC P-256 public key, as a SubjectPublicKeyInfo. Nothing else of
/// the request survives parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestPublicKey {
    spki_der: Vec<u8>,
}

impl GuestPublicKey {
    /// The key as DER SubjectPublicKeyInfo.
    pub fn as_der(&self) -> &[u8] {
        &self.spki_der
    }
}

/// Why a request was refused.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CsrError {
    /// More than [`MAX_CSR_BYTES`].
    #[error("request too large")]
    TooLarge,
    /// Not exactly one well-formed PEM certificate request.
    #[error("request does not parse")]
    Parse,
    /// The key is not EC P-256.
    #[error("key is not EC P-256")]
    KeyType,
    /// The request's own signature does not verify.
    #[error("request signature does not verify")]
    Signature,
}

/// The DER of the one PEM request in `pem`, or `Parse` if there is not exactly
/// one block, it is not a request, or anything but whitespace follows it.
fn single_csr_der(pem: &[u8]) -> Result<Vec<u8>, CsrError> {
    let (rest, first) = parse_x509_pem(pem).map_err(|_| CsrError::Parse)?;
    if !rest.iter().all(u8::is_ascii_whitespace) {
        return Err(CsrError::Parse);
    }
    if first.label != PEM_LABEL && first.label != PEM_LABEL_LEGACY {
        return Err(CsrError::Parse);
    }
    Ok(first.contents)
}

/// Whether `spki` is an uncompressed EC P-256 point.
fn is_p256(spki: &SubjectPublicKeyInfo<'_>) -> bool {
    let ec_key = oid!(1.2.840.10045.2.1);
    let p256 = oid!(1.2.840.10045.3.1.7);
    if spki.algorithm.algorithm != ec_key {
        return false;
    }
    let named_p256 = spki
        .algorithm
        .parameters
        .as_ref()
        .and_then(|p| p.as_oid().ok())
        .is_some_and(|curve| curve == p256);
    let point = spki.subject_public_key.data.as_ref();
    named_p256 && point.len() == P256_POINT_LEN && point.first() == Some(&UNCOMPRESSED_POINT_TAG)
}

/// Parses one PEM CSR and returns only its public key.
///
/// # Errors
/// [`CsrError`]: size, parse, key type, then signature, in that order.
pub fn parse_csr(pem: &[u8]) -> Result<GuestPublicKey, CsrError> {
    if pem.len() > MAX_CSR_BYTES {
        return Err(CsrError::TooLarge);
    }
    let der = single_csr_der(pem)?;
    let (rest, csr) = X509CertificationRequest::from_der(&der).map_err(|_| CsrError::Parse)?;
    if !rest.is_empty() {
        return Err(CsrError::Parse);
    }
    let spki = &csr.certification_request_info.subject_pki;
    if !is_p256(spki) {
        return Err(CsrError::KeyType);
    }
    csr.verify_signature().map_err(|_| CsrError::Signature)?;
    Ok(GuestPublicKey {
        spki_der: spki.raw.to_vec(),
    })
}

#[cfg(test)]
#[path = "csr_tests.rs"]
mod csr_tests;
