// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Reading the installed-guest marker through `qemu-guest-agent` (ADR-0043).
//!
//! libvirt has no guestinfo channel, so the signal that a guest booted from
//! its *installed* disk — rather than from the live installer that is still
//! overwriting it — arrives as a file the installed system writes and the
//! host reads back out of band.
//!
//! Everything here treats the guest as untrusted input. The payload crosses
//! a boundary from inside a VM that, by the time this matters, may already
//! have been handed to somebody: it is size-capped, parsed defensively, and
//! every unparseable answer means "not installed" rather than an error. A
//! guest must not be able to make a reconcile loop panic, allocate without
//! bound, or retry forever.

use banlieue_libvirt::{AGENT_TIMEOUT_DEFAULT, Domain, Session, domain_qemu_agent_command};
use banlieue_provider_sdk::pem::der_to_pem;
use base64::Engine as _;
use tokio::io::{AsyncRead, AsyncWrite};

/// Path the installed system writes its phase to.
///
/// Under `/run` on purpose: it is tmpfs, so the marker cannot survive a
/// power cycle and be mistaken for a fresh assertion (ADR-0043 Decision 3).
pub const MARKER_PATH: &str = "/run/banlieue/phase";

/// The only value that means "booted from the installed disk".
pub const PHASE_INSTALLED: &str = "installed";

/// Largest marker we will decode, in bytes.
///
/// The marker is one short word. Anything larger is a wrong path or a guest
/// trying to make the host allocate, so it is refused rather than trusted.
pub const MARKER_READ_MAX: usize = 256;

/// Path the installed system writes its vTPM EK certificate to (ADR-0045).
///
/// Beside the phase marker and on the same tmpfs, for the same reason: a
/// certificate left behind by a previous boot must not be mistaken for this
/// boot's assertion.
///
/// The guest writes it because on libvirt there is nowhere else to read it
/// from. `swtpm_localca` issues the certificate into the vTPM's NVRAM and
/// persists no copy — not on the host filesystem, not through any libvirt
/// RPC — so the only readable location is inside the guest, at TPM NV index
/// `0x01c00002`. banlieue reads the file the guest exports and never runs
/// `guest-exec`, which would be host-to-guest arbitrary code execution
/// acquired to fetch a public key (ADR-0045 Decision 2).
pub const EK_PATH: &str = "/run/banlieue/ek.pem";

/// Largest EK certificate we will decode, in bytes.
///
/// A real `swtpm_localca` RSA-2048 EK certificate is about 1.4 KiB as PEM.
/// This leaves room for a larger key or a second certificate without leaving
/// room for a guest to make the provider allocate.
pub const EK_READ_MAX: usize = 8192;

/// `guest-file-open`, read-only.
///
/// `mode` is explicit rather than defaulted: a reconcile loop inspecting a
/// guest must not be able to modify it.
#[must_use]
pub fn guest_file_open_cmd(path: &str) -> String {
    serde_json::json!({
        "execute": "guest-file-open",
        "arguments": { "path": path, "mode": "r" }
    })
    .to_string()
}

/// `guest-file-read` for an open handle, capped at `count` bytes.
///
/// The cap is a parameter rather than a constant because the two things read
/// out of a guest differ by an order of magnitude — a one-word phase marker
/// ([`MARKER_READ_MAX`]) and an X.509 certificate ([`EK_READ_MAX`]) — and
/// sizing one for the other either truncates certificates or lets a marker
/// read allocate far more than a marker can justify.
#[must_use]
pub fn guest_file_read_cmd(handle: i64, count: usize) -> String {
    serde_json::json!({
        "execute": "guest-file-read",
        "arguments": { "handle": handle, "count": count }
    })
    .to_string()
}

/// `guest-ping` — "is anybody there".
///
/// Carries no information beyond liveness, which is exactly why it is the
/// right question to ask when an open has already failed.
#[must_use]
pub fn guest_ping_cmd() -> String {
    serde_json::json!({ "execute": "guest-ping" }).to_string()
}

/// `guest-file-close` for an open handle.
#[must_use]
pub fn guest_file_close_cmd(handle: i64) -> String {
    serde_json::json!({
        "execute": "guest-file-close",
        "arguments": { "handle": handle }
    })
    .to_string()
}

/// The handle from a `guest-file-open` reply, or `None`.
///
/// `None` covers the ordinary case as well as the broken one: the marker is
/// absent for the whole install, which is the expected state for most of a
/// Deferred member's life.
#[must_use]
pub fn parse_file_handle(reply: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(reply)
        .ok()?
        .get("return")?
        .as_i64()
}

/// The decoded payload of a `guest-file-read` reply, capped at `max` bytes.
///
/// Every byte here came from inside a guest, so the cap is applied to the
/// *encoded* length first: 4 base64 characters carry 3 bytes, so checking
/// before decoding bounds the allocation rather than discovering afterwards
/// that it was unbounded.
fn guest_text_from_read(reply: &str, max: usize) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(reply).ok()?;
    let b64 = value.get("return")?.get("buf-b64")?.as_str()?;
    if b64.len() > max * 2 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    if bytes.len() > max {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// The trimmed marker contents from a `guest-file-read` reply, or `None`.
#[must_use]
pub fn guest_phase_from_read(reply: &str) -> Option<String> {
    guest_text_from_read(reply, MARKER_READ_MAX).map(|t| t.trim().to_string())
}

/// The subject CN a domain's EK certificate must carry (ADR-0045 Decision 3).
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

/// The EK certificate PEM from a `guest-file-read` reply, or `None`.
///
/// Returns the certificate only when the payload really is one: a single
/// `CERTIFICATE` PEM block whose contents parse as X.509. A guest gets to
/// decide *whether* to report, never to decide what ends up in a status
/// field that consumers will feed to a certificate library.
#[must_use]
pub fn parse_ek_pem(reply: &str) -> Option<String> {
    let text = guest_text_from_read(reply, EK_READ_MAX)?;
    parse_ek_pem_str(&text)
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
    Some(der_to_pem(&pem.contents))
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

/// Whether a `guest-file-read` reply says the installed system is running.
///
/// Exact match after trimming — a substring test would let `not-installed`
/// read as installed.
#[must_use]
pub fn guest_is_installed(reply: &str) -> bool {
    guest_phase_from_read(reply).is_some_and(|p| p == PHASE_INSTALLED)
}

/// What a guest probe found.
///
/// Three outcomes, not two, because "the agent never answered" and "the
/// agent answered and there is no marker" call for different behaviour: the
/// first means nothing will *ever* announce (an image with no guest agent),
/// the second means something may be installing right now. Collapsing them
/// into a bool is what makes a poll loop either too slow or infinite
/// (ADR-0043 Decision 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestProbe {
    /// The agent did not answer at all — no `qemu-guest-agent` in the
    /// image, or the guest is not up far enough to run it.
    AgentUnreachable,
    /// The agent answered, but the marker is absent or does not say
    /// `installed`.
    NotAnnounced,
    /// The marker says the installed system is running.
    Installed,
}

impl GuestProbe {
    /// Whether the installed guest has announced itself.
    #[must_use]
    pub fn is_installed(self) -> bool {
        self == Self::Installed
    }
}

/// Ask a running domain whether its installed system has announced itself.
///
/// Returns `false` for every negative answer — no agent, no marker, an
/// unreadable one — because they are all indistinguishable from "not yet",
/// and a Deferred member spends most of its life legitimately in that state.
///
/// # Errors
/// Never fails on a guest-side condition. Propagates only transport errors
/// that mean the *host* connection is unusable.
pub async fn probe_guest<S>(session: &mut Session<S>, dom: &Domain) -> GuestProbe
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let open = guest_file_open_cmd(MARKER_PATH);
    // An agent error is NOT delivered as an Ok payload carrying JSON:
    // libvirtd turns it into an RPC fault, so `guest-file-open` on a marker
    // that does not exist yet arrives here as `Err` — indistinguishable, at
    // this point, from no agent at all. Verified against a real host: the
    // earlier reading of this (an Ok reply carrying a JSON error) made every
    // healthy member report `AgentUnreachable` for the whole install window.
    let reply = match domain_qemu_agent_command(session, dom, &open, AGENT_TIMEOUT_DEFAULT).await {
        Ok(Some(reply)) => reply,
        Ok(None) | Err(_) => return classify_open_failure(session, dom).await,
    };
    let Some(handle) = parse_file_handle(&reply) else {
        // The agent answered; the marker is simply not there yet.
        return GuestProbe::NotAnnounced;
    };

    let read = guest_file_read_cmd(handle, MARKER_READ_MAX);
    let read_reply = domain_qemu_agent_command(session, dom, &read, AGENT_TIMEOUT_DEFAULT).await;

    // Close whatever happened to the read: leaking guest file handles across
    // every reconcile would eventually exhaust the agent's table.
    let close = guest_file_close_cmd(handle);
    let _ = domain_qemu_agent_command(session, dom, &close, AGENT_TIMEOUT_DEFAULT).await;

    match read_reply {
        Ok(Some(r)) if guest_is_installed(&r) => GuestProbe::Installed,
        _ => GuestProbe::NotAnnounced,
    }
}

/// What an EK certificate probe found (ADR-0045).
///
/// Four outcomes, because they drive different behaviour: a guest that has
/// not exported its certificate yet is still installing and should be asked
/// again, while one whose certificate names a different domain is reporting
/// something it must not be allowed to publish, and saying so is the point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EkProbe {
    /// The agent did not answer, or the file is not there yet.
    NotPublished,
    /// A certificate was read, but its subject CN is not this domain's.
    /// Discarded rather than published (ADR-0045 Decision 3).
    Mismatch,
    /// A certificate issued to this domain, as PEM.
    Published(String),
}

/// Read a running domain's vTPM EK certificate, if it has exported one.
///
/// Read-only throughout: `guest-file-open` with `mode: "r"`, then read and
/// close. Never `guest-exec` (ADR-0045 Decision 2).
///
/// # Errors
/// Never fails on a guest-side condition — an absent file, an unreadable
/// one, and a guest that reports nonsense all resolve to [`EkProbe`]
/// variants. Only a `Session` that is itself unusable propagates, and it
/// does so as [`EkProbe::NotPublished`] for the same reason `probe_guest`
/// collapses its transport failures: a member spends most of its install
/// legitimately in this state.
pub async fn read_ek_certificate<S>(
    session: &mut Session<S>,
    dom: &Domain,
    domain_name: &str,
    domain_uuid: &str,
) -> EkProbe
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let open = guest_file_open_cmd(EK_PATH);
    let reply = match domain_qemu_agent_command(session, dom, &open, AGENT_TIMEOUT_DEFAULT).await {
        Ok(Some(reply)) => reply,
        // No agent, or no file yet. Indistinguishable here and treated the
        // same: ask again later.
        Ok(None) | Err(_) => return EkProbe::NotPublished,
    };
    let Some(handle) = parse_file_handle(&reply) else {
        return EkProbe::NotPublished;
    };

    let read = guest_file_read_cmd(handle, EK_READ_MAX);
    let read_reply = domain_qemu_agent_command(session, dom, &read, AGENT_TIMEOUT_DEFAULT).await;

    // Close whatever happened to the read: leaking guest file handles across
    // every reconcile would eventually exhaust the agent's table.
    let close = guest_file_close_cmd(handle);
    let _ = domain_qemu_agent_command(session, dom, &close, AGENT_TIMEOUT_DEFAULT).await;

    let Ok(Some(r)) = read_reply else {
        return EkProbe::NotPublished;
    };
    let Some(pem) = parse_ek_pem(&r) else {
        return EkProbe::NotPublished;
    };
    if !ek_cn_matches(&pem, domain_name, domain_uuid) {
        return EkProbe::Mismatch;
    }
    EkProbe::Published(pem)
}

/// Decide which of the two negative answers a failed open meant.
///
/// The only way to separate "no marker" from "no agent" is to ask the agent
/// something that succeeds when it is alive. A ping costs one extra round
/// trip and is paid only on the negative path — which is the common one
/// while a Deferred member installs, and the reason this distinction exists
/// at all: `AgentUnreachable` and `NotAnnounced` drive different requeue
/// cadences (ADR-0043 Decision 8).
async fn classify_open_failure<S>(session: &mut Session<S>, dom: &Domain) -> GuestProbe
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match domain_qemu_agent_command(session, dom, &guest_ping_cmd(), AGENT_TIMEOUT_DEFAULT).await {
        Ok(Some(_)) => GuestProbe::NotAnnounced,
        Ok(None) | Err(_) => GuestProbe::AgentUnreachable,
    }
}

#[cfg(test)]
#[path = "guest_tests.rs"]
mod guest_tests;
