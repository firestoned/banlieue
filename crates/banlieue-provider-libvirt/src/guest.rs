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

/// `guest-file-read` for an open handle, capped at [`MARKER_READ_MAX`].
#[must_use]
pub fn guest_file_read_cmd(handle: i64) -> String {
    serde_json::json!({
        "execute": "guest-file-read",
        "arguments": { "handle": handle, "count": MARKER_READ_MAX }
    })
    .to_string()
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

/// The trimmed marker contents from a `guest-file-read` reply, or `None`.
#[must_use]
pub fn guest_phase_from_read(reply: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(reply).ok()?;
    let b64 = value.get("return")?.get("buf-b64")?.as_str()?;
    // Cap before decoding: 4 base64 characters carry 3 bytes, so this bounds
    // the allocation rather than checking after the fact.
    if b64.len() > MARKER_READ_MAX * 2 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    if bytes.len() > MARKER_READ_MAX {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    Some(text.trim().to_string())
}

/// Whether a `guest-file-read` reply says the installed system is running.
///
/// Exact match after trimming — a substring test would let `not-installed`
/// read as installed.
#[must_use]
pub fn guest_is_installed(reply: &str) -> bool {
    guest_phase_from_read(reply).is_some_and(|p| p == PHASE_INSTALLED)
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
pub async fn domain_guest_installed<S>(session: &mut Session<S>, dom: &Domain) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let open = guest_file_open_cmd(MARKER_PATH);
    let Ok(Some(reply)) =
        domain_qemu_agent_command(session, dom, &open, AGENT_TIMEOUT_DEFAULT).await
    else {
        return false;
    };
    let Some(handle) = parse_file_handle(&reply) else {
        return false;
    };

    let read = guest_file_read_cmd(handle);
    let read_reply = domain_qemu_agent_command(session, dom, &read, AGENT_TIMEOUT_DEFAULT).await;

    // Close whatever happened to the read: leaking guest file handles across
    // every reconcile would eventually exhaust the agent's table.
    let close = guest_file_close_cmd(handle);
    let _ = domain_qemu_agent_command(session, dom, &close, AGENT_TIMEOUT_DEFAULT).await;

    matches!(read_reply, Ok(Some(r)) if guest_is_installed(&r))
}

#[cfg(test)]
#[path = "guest_tests.rs"]
mod guest_tests;
