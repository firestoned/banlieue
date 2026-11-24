// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The per-host attestation CA as the installer sees it (ADR-0146 Decision 5,
//! ADR-0143 Decision 1): the flags' values, the `[attestation]` host config
//! section, what the CA on disk looks like, and what the administrator is
//! told to do with its certificate.
//!
//! The CA itself (creation, loading, the checks `load` makes) is
//! `banlieue-attestation-ca`'s; nothing here generates a key, and nothing
//! here reads or prints one.
//!
//! The section is the provider crate's own `AttestationSection`, so the file
//! the installer writes is the file the provider loads:
//!
//! ```toml
//! [attestation]
//! ca_dir = "/var/lib/banlieue/attestation-ca"
//! certificate_ttl_secs = 3600
//! ```

use crate::paths::{ATTESTATION_CA_DIR, HOST_CONFIG};
use banlieue_attestation_ca::{
    AttestationCa, MAX_CERTIFICATE_TTL, MIN_CERTIFICATE_TTL, validate_name,
};
use banlieue_provider_cloud_hypervisor::host_config::{AttestationSection, HostConfig};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The certificate lifetime when none is asked for: one hour.
pub const DEFAULT_CERTIFICATE_TTL: Duration = Duration::from_secs(SECONDS_PER_HOUR);
/// The section's table name.
pub const SECTION: &str = "attestation";
/// The key naming the CA directory.
pub const KEY_CA_DIR: &str = "ca_dir";
/// The key holding the certificate lifetime, in whole seconds.
pub const KEY_TTL_SECS: &str = "certificate_ttl_secs";
/// The CA certificate's file name, in the CA directory.
pub const CA_CERT_FILE: &str = "ca.crt";
/// The CA key's file name, in the CA directory.
pub const CA_KEY_FILE: &str = "ca.key";
/// The pending request's file name, in the pending directory (ADR-0146
/// Decision 7).
pub const CA_CSR_FILE: &str = "ca.csr";
/// The chain above an imported CA, in the CA directory: the host CA
/// certificate and any intermediates, never the root. Public.
pub const CA_CHAIN_FILE: &str = "ca-chain.pem";

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 3600;
const SECONDS_PER_DAY: u64 = 86_400;

/// The `[attestation]` section this installer writes for `ttl`.
#[must_use]
pub fn section(ttl: Duration) -> AttestationSection {
    AttestationSection {
        ca_dir: PathBuf::from(ATTESTATION_CA_DIR),
        certificate_ttl_secs: ttl.as_secs(),
    }
}

/// Parse `--attestation-certificate-ttl`: whole numbers with a unit, `s`,
/// `m`, `h` or `d` (`90s`, `30m`, `1h`, `12h`, `2d`), within the signer's
/// bounds.
///
/// # Errors
/// A sentence saying what is wrong with `raw`.
pub fn parse_ttl(raw: &str) -> Result<Duration, String> {
    let unit = |c: char| match c {
        's' => Some(1),
        'm' => Some(SECONDS_PER_MINUTE),
        'h' => Some(SECONDS_PER_HOUR),
        'd' => Some(SECONDS_PER_DAY),
        _ => None,
    };
    let bad = || format!("{raw:?}: not a duration like 30m, 1h or 12h");
    let last = raw.chars().last().ok_or_else(bad)?;
    let per_unit = unit(last).ok_or_else(bad)?;
    let digits = &raw[..raw.len() - last.len_utf8()];
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    let ttl = digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(per_unit))
        .map(Duration::from_secs)
        .ok_or_else(bad)?;
    if ttl < MIN_CERTIFICATE_TTL || ttl > MAX_CERTIFICATE_TTL {
        return Err(format!(
            "{raw:?}: between {}s and {}s",
            MIN_CERTIFICATE_TTL.as_secs(),
            MAX_CERTIFICATE_TTL.as_secs()
        ));
    }
    Ok(ttl)
}

/// The CA name for a host that opts in without choosing one: its short
/// name, when that is inside the name alphabet.
///
/// # Errors
/// A sentence saying why the short name cannot be a CA name.
pub fn default_name(hostname: &str) -> Result<String, String> {
    validate_name(hostname).map_err(|e| {
        format!(
            "the host's short name {hostname:?} cannot be the CA name ({e}); \
             give one with --attestation-ca-name"
        )
    })?;
    Ok(hostname.to_string())
}

/// The `[attestation]` section the host config at [`HOST_CONFIG`] has now.
///
/// A missing or unreadable file, or one that does not load, is `None`: this
/// answers "what is there", and `install --force` regenerates a broken file.
#[must_use]
pub fn existing_section(probe: &dyn crate::ops::Probe) -> Option<AttestationSection> {
    let raw = probe.read(Path::new(HOST_CONFIG)).ok()?;
    HostConfig::parse(&String::from_utf8_lossy(&raw))
        .ok()?
        .attestation
}

/// What the CA on disk is: the facts `status` and the stage report. Never
/// the key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaInfo {
    /// The certificate's common name, the issuer CN of every agent ID it
    /// signs.
    pub name: String,
    /// When the CA certificate expires, as text.
    pub not_after: String,
    /// Whole days from now until it expires; negative once it has.
    pub days_remaining: i64,
}

const SECONDS_PER_DAY_I64: i64 = 86_400;

/// Whole days from `now` to `not_after`, rounded toward the past: `0` for the
/// last day, `-1` for one that expired within the last day.
#[must_use]
pub fn days_until(not_after: std::time::SystemTime, now: std::time::SystemTime) -> i64 {
    let signed = |t: std::time::SystemTime| {
        t.duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    };
    (signed(not_after) - signed(now)).div_euclid(SECONDS_PER_DAY_I64)
}

/// `"N days left"`, or `"EXPIRED"` once it has: the phrase `status` and
/// `preflight` put after a CA's expiry.
#[must_use]
pub fn expiry_phrase(days_remaining: i64) -> String {
    if days_remaining < 0 {
        return "EXPIRED".to_string();
    }
    format!("{days_remaining} days left")
}

/// Load the CA in `dir` with the crate's own checks (key mode `0600`, a
/// directory closed to group and world, no symlinks, key and certificate
/// matching) and report its name and expiry.
///
/// # Errors
/// A sentence saying what is wrong.
pub fn inspect(dir: &Path) -> Result<CaInfo, String> {
    let ca = AttestationCa::load(dir).map_err(|e| e.to_string())?;
    let pem = ca.certificate_pem().to_string();
    let not_after = x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .map_err(|e| e.to_string())
        .and_then(|(_, p)| {
            let cert = p.parse_x509().map_err(|e| e.to_string())?;
            Ok(cert.validity().not_after.to_string())
        })?;
    Ok(CaInfo {
        name: ca.common_name().to_string(),
        not_after,
        days_remaining: days_until(ca.not_after(), std::time::SystemTime::now()),
    })
}

/// What the administrator does with the new CA, as lines for the terminal:
/// the certificate's path, its common name, and the one step. Never the key.
#[must_use]
pub fn admin_action(dir: &Path, name: &str) -> Vec<String> {
    vec![
        format!(
            "attestation CA certificate: {}",
            dir.join(CA_CERT_FILE).display()
        ),
        format!("attestation CA common name: {name}"),
        "administrator action: copy that single certificate into \
         Provider.spec.attestation.hostCa and into the SPIRE server's x509pop bundle \
         (the common name must be unique across the bundle)"
            .to_string(),
    ]
}

/// What the administrator does with a new request (ADR-0146 Decision 7), as
/// lines for the terminal: the request's path and common name, and the steps
/// that follow. Never the key.
#[must_use]
pub fn csr_admin_action(pending_dir: &Path, name: &str) -> Vec<String> {
    vec![
        format!(
            "attestation CA request: {}",
            pending_dir.join(CA_CSR_FILE).display()
        ),
        format!("attestation CA common name: {name}"),
        "administrator action: have that request signed on the node-attestation PKI mount \
         as an intermediate (CA:TRUE, path length 0, keyCertSign only), checking the common \
         name is not already used there, then run this command again with \
         --attestation-ca-import FILE (the signed certificate, then any chain above it)"
            .to_string(),
        "the live CA, if any, keeps serving until the import".to_string(),
    ]
}

#[cfg(test)]
#[path = "attestation_tests.rs"]
mod attestation_tests;
