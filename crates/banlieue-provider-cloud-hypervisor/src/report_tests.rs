// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `report.rs`: the parser, and a listener on a real socket
//! in a temporary directory, owned by this process's own uid.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn only_an_exact_installed_line_counts() {
        assert!(parse_report(b"phase=installed\n").installed);
        assert!(parse_report(b"  phase=installed  \r\nek-pem-begin\n").installed);
        assert!(!parse_report(b"phase=not-installed\n").installed);
        assert!(!parse_report(b"phase=installedX\n").installed);
        assert!(!parse_report(b"").installed);
        assert!(!parse_report(&[0xff, 0xfe]).installed, "not UTF-8");
    }

    /// More than the cap is not a report: a guest cannot make the provider
    /// buffer without bound, and a too-long message is refused whole.
    #[test]
    fn an_oversized_report_is_refused() {
        let mut big = b"phase=installed\n".to_vec();
        big.resize(MAX_REPORT_BYTES + 1, b'x');
        assert!(!parse_report(&big).installed);
    }

    #[tokio::test]
    async fn a_guest_connection_is_heard_and_the_socket_is_the_guests() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vsock.sock_1024");
        let me = crate::sys::effective_uid();
        let group = crate::sys::effective_gid();
        let listeners = Listeners::default();
        listeners.ensure("m1", &path, me, group).await.unwrap();
        assert!(!listeners.installed("m1"));
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(meta.uid(), me);
        assert_eq!(meta.permissions().mode() & 0o777, 0o660);

        let mut guest = std::os::unix::net::UnixStream::connect(&path).unwrap();
        guest.write_all(b"phase=installed\n").unwrap();
        drop(guest);
        for _ in 0..50 {
            if listeners.installed("m1") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(listeners.installed("m1"));

        // Idempotent while running; stop removes the socket.
        listeners.ensure("m1", &path, me, group).await.unwrap();
        listeners.stop("m1", &path);
        assert!(!path.exists());
        assert!(!listeners.installed("m1"));
    }

    /// A report wakes the reconciler at once rather than waiting for its next
    /// periodic pass: in the roadmap 17 phase G run (2026-09-28) every
    /// member sat 180–204 s between reporting and `GuestReady`, more than the
    /// install itself. Once per machine: the boot stage reports on every
    /// boot, and only the first changes anything.
    #[tokio::test]
    async fn the_first_report_wakes_the_reconciler_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vsock.sock_1024");
        let me = crate::sys::effective_uid();
        let group = crate::sys::effective_gid();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let listeners = Listeners::waking(tx);
        listeners.ensure("m1", &path, me, group).await.unwrap();

        for _ in 0..2 {
            let mut guest = std::os::unix::net::UnixStream::connect(&path).unwrap();
            guest.write_all(b"phase=installed\n").unwrap();
        }
        let woke = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await;
        assert_eq!(
            woke.ok().flatten(),
            Some(()),
            "a report wakes the reconciler"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            rx.try_recv().is_err(),
            "a second report does not wake it again"
        );
        listeners.stop("m1", &path);
    }

    /// Something planted at the socket path that is not ours is refused.
    #[tokio::test]
    async fn a_planted_symlink_at_the_socket_path_is_replaced_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        let path = dir.path().join("vsock.sock_1024");
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        let listeners = Listeners::default();
        listeners
            .ensure(
                "m1",
                &path,
                crate::sys::effective_uid(),
                crate::sys::effective_gid(),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
        use std::os::unix::fs::FileTypeExt;
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_socket()
        );
        listeners.stop("m1", &path);
    }
}
