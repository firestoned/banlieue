// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Check a VMM's API socket before trusting it (ADR-0061 Decision 6).
//!
//! Whoever can write to a guest's API socket owns that guest. ADR-0063
//! creates it `guest-uid:banlieue`, mode 0660, in a directory other guests
//! cannot traverse. Before connecting, the client confirms the path is still
//! exactly that. Anything else — another owner, "other" permission bits, a
//! regular file, a symlink — is refused.

use crate::error::{Error, Result};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

/// Permission bits a VMM socket may carry: owner and group read/write,
/// nothing for "other", no execute (ADR-0063 Decision 5).
pub const SOCKET_MODE_ALLOWED: u32 = 0o660;
/// Mask of the permission bits within `st_mode`.
const PERMISSION_BITS: u32 = 0o777;

/// Who must own a guest's API socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedSocket {
    /// The guest's unprivileged uid (`status.hostUid`).
    pub uid: u32,
    /// The provider's group.
    pub gid: u32,
}

/// The facts about a socket path the check needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocketMeta {
    /// Whether the entry itself is a socket. False for a symlink to one.
    pub is_socket: bool,
    /// Owner uid.
    pub uid: u32,
    /// Owner gid.
    pub gid: u32,
    /// Full `st_mode`.
    pub mode: u32,
}

/// Read [`SocketMeta`] for `path` without following a symlink.
///
/// # Errors
/// [`Error::Io`] when the path cannot be read.
pub fn socket_meta(path: &Path) -> Result<SocketMeta> {
    let m = std::fs::symlink_metadata(path)?;
    Ok(SocketMeta {
        is_socket: m.file_type().is_socket(),
        uid: m.uid(),
        gid: m.gid(),
        mode: m.mode(),
    })
}

/// Whether `meta` is the socket ADR-0063 creates for this guest.
///
/// # Errors
/// [`Error::Socket`] naming the first thing that does not match.
pub fn check_socket(meta: SocketMeta, expected: ExpectedSocket) -> Result<()> {
    if !meta.is_socket {
        return Err(Error::Socket("not a Unix socket".into()));
    }
    if meta.uid != expected.uid {
        return Err(Error::Socket(format!(
            "owner uid {} is not the guest's {}",
            meta.uid, expected.uid
        )));
    }
    if meta.gid != expected.gid {
        return Err(Error::Socket(format!(
            "group {} is not the provider's {}",
            meta.gid, expected.gid
        )));
    }
    let perms = meta.mode & PERMISSION_BITS;
    if perms & !SOCKET_MODE_ALLOWED != 0 {
        return Err(Error::Socket(format!(
            "mode {perms:o} allows more than {SOCKET_MODE_ALLOWED:o}"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "socket_tests.rs"]
mod socket_tests;
