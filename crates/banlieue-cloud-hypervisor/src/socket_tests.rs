// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `socket.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::Error;

    const GUEST_UID: u32 = 2_000_001;
    const BANLIEUE_GID: u32 = 998;

    fn expected() -> ExpectedSocket {
        ExpectedSocket {
            uid: GUEST_UID,
            gid: BANLIEUE_GID,
        }
    }

    fn good() -> SocketMeta {
        SocketMeta {
            is_socket: true,
            uid: GUEST_UID,
            gid: BANLIEUE_GID,
            mode: 0o140_660,
        }
    }

    /// ADR-0063 Decision 5: guest-uid:banlieue, 0660.
    #[test]
    fn the_layout_adr_0063_creates_passes() {
        assert!(check_socket(good(), expected()).is_ok());
    }

    #[test]
    fn a_stricter_mode_passes() {
        let mut m = good();
        m.mode = 0o140_600;
        assert!(check_socket(m, expected()).is_ok());
    }

    /// Whoever can write the API socket owns the guest. Any access for
    /// "other" is refused.
    #[test]
    fn world_access_is_refused() {
        for mode in [0o140_666, 0o140_664, 0o140_661] {
            let mut m = good();
            m.mode = mode;
            assert!(
                matches!(check_socket(m, expected()), Err(Error::Socket(_))),
                "mode {mode:o}"
            );
        }
    }

    /// A socket swapped in by another local user.
    #[test]
    fn the_wrong_owner_or_group_is_refused() {
        let mut m = good();
        m.uid = GUEST_UID + 1;
        assert!(check_socket(m, expected()).is_err());
        let mut m = good();
        m.gid = BANLIEUE_GID + 1;
        assert!(check_socket(m, expected()).is_err());
    }

    /// A regular file (or a symlink target) at the socket path is not the
    /// VMM, whoever owns it.
    #[test]
    fn a_non_socket_is_refused() {
        let mut m = good();
        m.is_socket = false;
        assert!(check_socket(m, expected()).is_err());
    }

    /// The path check reads the entry itself, never a symlink's target:
    /// pointing a symlink at a correctly-owned socket must not pass.
    #[test]
    fn a_symlink_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&target).unwrap();
        let link = dir.path().join("api.sock");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let meta = socket_meta(&link).unwrap();
        assert!(!meta.is_socket, "a symlink must be seen as a symlink");
        let meta = socket_meta(&target).unwrap();
        assert!(meta.is_socket);
    }

    #[test]
    fn a_missing_path_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(socket_meta(&dir.path().join("absent.sock")).is_err());
    }
}
