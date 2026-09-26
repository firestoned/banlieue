// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! vTPM endorsement key certificate checks shared by the KVM providers
//! (ADR-0045 Decision 3, ADR-0065 Decision 1).
//!
//! Both libvirt and the Cloud Hypervisor provider have swtpm mint the EK
//! certificate with `swtpm_setup --vmid <name>:<uuid>`, and
//! `swtpm_localca` puts that string in the subject CN.

/// The subject CN a machine's EK certificate must carry (ADR-0045 Decision 3).
///
/// libvirt invokes `swtpm_setup --vmid <domain-name>:<domain-uuid>`, and
/// `swtpm_localca` puts that string in the certificate's subject CN. Both
/// halves are values banlieue itself assigned when it defined the domain,
/// which is what makes the check meaningful: a guest reporting another
/// member's certificate is caught without any cryptography.
#[must_use]
pub fn expected_ek_cn(domain_name: &str, domain_uuid: &str) -> String {
    format!("{domain_name}:{domain_uuid}")
}

/// Whether `pem` is a certificate issued to exactly this domain.
///
/// False for anything that is not a parseable certificate, so a caller can
/// use this as the single gate before publishing (ADR-0045 Decision 3).
#[must_use]
pub fn ek_cn_matches(pem: &str, domain_name: &str, domain_uuid: &str) -> bool {
    let Ok((_, parsed)) = x509_parser::pem::parse_x509_pem(pem.as_bytes()) else {
        return false;
    };
    let Ok(cert) = parsed.parse_x509() else {
        return false;
    };
    let expected = expected_ek_cn(domain_name, domain_uuid);
    cert.subject()
        .iter_common_name()
        .filter_map(|cn| cn.as_str().ok())
        .any(|cn| cn == expected)
}

/// Normalise and validate a PEM certificate, or `None`.
///
/// The string form of [`parse_ek_pem`], for callers that already have the
/// text rather than an agent reply.
#[must_use]
pub fn parse_ek_pem_str(text: &str) -> Option<String> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(text.as_bytes()).ok()?;
    // A `PRIVATE KEY` block is valid PEM and is not a certificate.
    if pem.label != "CERTIFICATE" {
        return None;
    }
    // Parses as X.509, or it is not a certificate whatever its label says.
    pem.parse_x509().ok()?;
    // Re-encode from the DER that actually parsed, rather than returning any
    // slice of the guest's buffer. Slicing is what an earlier version did and
    // it was wrong in a way that is easy to miss: `x509_parser` SKIPS leading
    // lines that do not begin a PEM block and counts them in its position, so
    // "junk\n<valid cert>" sliced back to a string that still carried the
    // junk, still matched on CN, and was published. Re-encoding makes the
    // published value exactly one certificate by construction — there is no
    // input layout that can smuggle bytes past it.
    Some(crate::pem::der_to_pem(&pem.contents))
}

#[cfg(test)]
#[path = "ek_tests.rs"]
mod ek_tests;
