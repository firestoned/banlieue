// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! PEM encoding for certificates a provider publishes into a CR status.
//!
//! Both providers publish vTPM endorsement key certificates (ADR-0045) and
//! both arrive at DER: vCenter hands over
//! `VirtualTpm.endorsementKeyCertificate` as DER, and on libvirt the guest's
//! PEM is re-encoded from the DER an X.509 parser actually accepted. They
//! share this so there is one definition of "what we publish", rather than
//! two encoders that can drift.

/// PEM line length for base64 payloads, per RFC 7468 §2.
const PEM_LINE_WIDTH: usize = 64;

/// Encode a DER certificate as PEM.
///
/// Every consumer of `status.tpmEndorsementCertificates` feeds the value to
/// a certificate library, and a CR status is a text document — so the
/// conversion happens once, here, rather than in each verifier.
///
/// Re-encoding rather than echoing an input buffer is deliberate where the
/// DER came from an untrusted source: the output contains exactly the bytes
/// that parsed as a certificate and nothing a caller wrapped around them.
#[must_use]
pub fn der_to_pem(der: &[u8]) -> String {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = String::from("-----BEGIN CERTIFICATE-----\n");
    for chunk in b64.as_bytes().chunks(PEM_LINE_WIDTH) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}

#[cfg(test)]
#[path = "pem_tests.rs"]
mod pem_tests;
