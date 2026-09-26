// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The guest's `phase` report over vsock (ADR-0065 Decision 5).
//!
//! Every guest has a hybrid vsock device; a guest connecting to host port
//! [`REPORT_PORT`] reaches the Unix socket `<vsock socket>_<port>` in its run
//! directory, where the provider listens. The installed system's boot stage
//! connects and writes `phase=installed`; the provider sends nothing back.
//!
//! Everything received is untrusted: at most [`MAX_REPORT_BYTES`], read
//! with a timeout, and only an exact `phase=installed` line counts. Other
//! lines (an image may still send its EK certificate, which this provider
//! reads host-side instead) are ignored. A guest can only claim its *own*
//! installation: the socket is in its run directory and nobody else's.
//!
//! The listening socket is created by the provider and handed to the
//! guest's uid (the VMM, running as the guest, connects to it). The run
//! directory is the guest's, so the handover acts on the socket opened
//! `O_PATH | O_NOFOLLOW`, never on a path a symlink could redirect.

use std::collections::HashMap;
use std::fs::{self, OpenOptions, Permissions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tracing::{debug, info};

/// The host vsock port a guest reports on.
pub const REPORT_PORT: u32 = 1024;
/// Largest report accepted. Room for a phase line and, from images written
/// for ADR-0045, two PEM certificates the provider ignores.
pub const MAX_REPORT_BYTES: usize = 16 * 1024;
/// A report is one short write; a connection that dawdles is dropped.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// The one line that means "the installed system booted".
const INSTALLED_LINE: &str = "phase=installed";
/// The listening socket: the guest (owner) and the provider (group).
const SOCKET_MODE: u32 = 0o660;

/// What a guest reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The installed system booted (ADR-0043's marker).
    pub installed: bool,
}

/// Parse one connection's bytes. Oversized or non-UTF-8 input is no report.
#[must_use]
pub fn parse_report(bytes: &[u8]) -> Report {
    if bytes.len() > MAX_REPORT_BYTES {
        return Report::default();
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Report::default();
    };
    Report {
        installed: text.lines().any(|l| l.trim() == INSTALLED_LINE),
    }
}

struct Entry {
    task: JoinHandle<()>,
    installed: Arc<AtomicBool>,
}

/// One listener per machine, keyed by machine UID.
#[derive(Default)]
pub struct Listeners {
    inner: Mutex<HashMap<String, Entry>>,
}

impl Listeners {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Listen at `path` for machine `key`, unless already listening there.
    /// The socket is handed to `guest_uid`:`group`, mode `0660`. What was
    /// heard before a rebind is kept.
    ///
    /// # Errors
    /// Binding, or a socket that is not the one just bound.
    pub async fn ensure(
        &self,
        key: &str,
        path: &Path,
        guest_uid: u32,
        group: u32,
    ) -> io::Result<()> {
        let alive = self.lock().get(key).is_some_and(|e| !e.task.is_finished());
        if alive && fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket()) {
            return Ok(());
        }
        // Whatever is there (a stale socket, or something the guest put
        // there) is replaced: unlink removes a symlink, never its target.
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(path)?;
        hand_to_guest(path, guest_uid, group)?;

        let installed = self
            .lock()
            .get(key)
            .map_or_else(|| Arc::new(AtomicBool::new(false)), |e| e.installed.clone());
        let heard = installed.clone();
        let name = key.to_string();
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let heard = heard.clone();
                let name = name.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let limit = u64::try_from(MAX_REPORT_BYTES).unwrap_or(u64::MAX) + 1;
                    let read = tokio::time::timeout(
                        READ_TIMEOUT,
                        (&mut stream).take(limit).read_to_end(&mut buf),
                    )
                    .await;
                    if !matches!(read, Ok(Ok(_))) {
                        debug!(machine = %name, "guest report connection dropped");
                        return;
                    }
                    if parse_report(&buf).installed && !heard.swap(true, Ordering::SeqCst) {
                        info!(machine = %name, "guest reported phase=installed");
                    }
                });
            }
        });
        if let Some(old) = self
            .lock()
            .insert(key.to_string(), Entry { task, installed })
        {
            old.task.abort();
        }
        Ok(())
    }

    /// Whether machine `key`'s guest has reported `phase=installed` since
    /// this provider started listening.
    #[must_use]
    pub fn installed(&self, key: &str) -> bool {
        self.lock()
            .get(key)
            .is_some_and(|e| e.installed.load(Ordering::SeqCst))
    }

    /// Stop listening for `key` and remove its socket.
    pub fn stop(&self, key: &str, path: &Path) {
        if let Some(e) = self.lock().remove(key) {
            e.task.abort();
        }
        let _ = fs::remove_file(path);
    }
}

/// Give the socket just bound at `path` to the guest: opened `O_PATH |
/// O_NOFOLLOW`, checked to be a socket this process owns, then re-owned and
/// re-moded through `/proc/self/fd`, so nothing a guest swaps in at `path`
/// is ever changed.
fn hand_to_guest(path: &Path, guest_uid: u32, group: u32) -> io::Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.file_type().is_socket() || meta.uid() != crate::sys::effective_uid() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: not the socket this provider bound", path.display()),
        ));
    }
    let by_fd = format!("/proc/self/fd/{}", file.as_raw_fd());
    std::os::unix::fs::chown(&by_fd, Some(guest_uid), Some(group))?;
    fs::set_permissions(&by_fd, Permissions::from_mode(SOCKET_MODE))
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod report_tests;
