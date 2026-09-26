// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `sys.rs` that need no privilege. Tap and bridge calls are
//! exercised for real in `tests/live_sys.rs`, inside a user+network
//! namespace.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn interface_names_that_do_not_fit_are_refused_before_any_syscall() {
        assert!(ifreq_named("").is_err());
        assert!(ifreq_named("sixteen-chars-xx").is_err());
        assert!(ifreq_named("nul\0inside").is_err());
        assert!(ifreq_named("bch0f3c9a1e5b0").is_ok());
    }

    /// The kernel's own rule (dev_valid_name): not "." or "..", no '/',
    /// ':' or whitespace. Checked here too, so nothing reaches a syscall or
    /// a sysfs path that the kernel would not accept as a name.
    #[test]
    fn interface_names_follow_the_kernels_rules() {
        for bad in [
            ".",
            "..",
            "a b",
            "a:b",
            "a/b",
            "tab\there",
            "",
            "sixteen-chars-xx",
        ] {
            assert!(!valid_ifname(bad), "{bad:?}");
            assert!(ifreq_named(bad).is_err(), "{bad:?}");
        }
        for good in [
            "lo",
            "br0",
            "virbr0",
            "bch0f3c9a1e5b0",
            "enp9s0",
            "fifteen-chars-x",
        ] {
            assert!(valid_ifname(good), "{good:?}");
        }
        assert!(
            !interface_exists(".."),
            ".. is /sys/class, not an interface"
        );
        assert!(master_of("..").is_none());
    }

    /// Our `ifreq` must be byte-for-byte the kernel's: the ioctls read and
    /// write exactly `size_of::<libc::ifreq>()` bytes through the pointer.
    #[test]
    fn our_ifreq_has_the_kernels_layout() {
        assert_eq!(
            std::mem::size_of::<IfReq>(),
            std::mem::size_of::<libc::ifreq>()
        );
        assert_eq!(
            std::mem::align_of::<IfReq>(),
            std::mem::align_of::<libc::ifreq>()
        );
        let mut r = ifreq_named("br0").unwrap();
        assert_eq!(&r.name[..4], b"br0\0");
        r.set_flags(0x1043);
        assert_eq!(r.flags(), 0x1043);
        r.set_ifindex(7);
        assert_eq!(&r.data[..4], &7_i32.to_ne_bytes());
    }

    #[test]
    fn loopback_exists_and_a_made_up_interface_does_not() {
        assert!(interface_exists("lo"));
        assert!(!interface_exists("bchdoesnotexist"));
        assert!(!interface_exists("../lo"));
        assert!(!interface_exists(""));
    }

    #[test]
    fn deleting_an_absent_tap_is_success() {
        assert!(delete_persistent_tap("bchabsent00").is_ok());
    }

    #[test]
    fn clone_file_copies_content_and_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("image.raw");
        std::fs::write(&src, b"kairos").unwrap();
        let dst = dir.path().join("os.raw");
        let (_file, kind) = clone_file(&src, &dst).unwrap();
        assert!(matches!(kind, CloneKind::Reflink | CloneKind::Copy));
        assert_eq!(std::fs::read(&dst).unwrap(), b"kairos");
        // Never clobber an existing OS disk: it may hold an installed guest.
        assert!(clone_file(&src, &dst).is_err());
    }

    #[test]
    fn effective_ids_match_the_process() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("probe");
        std::fs::write(&f, b"").unwrap();
        let m = std::fs::metadata(&f).unwrap();
        assert_eq!(m.uid(), effective_uid());
        assert_eq!(m.gid(), effective_gid());
    }

    /// The destination is opened O_CREAT|O_EXCL, which never follows a
    /// symlink; the fallback copy goes into that same open file, never back
    /// through the path. A symlink planted at the destination is refused and
    /// its target left alone.
    #[test]
    fn clone_file_never_writes_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("image.raw");
        std::fs::write(&src, b"kairos").unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        let dst = dir.path().join("os.raw.tmp");
        std::os::unix::fs::symlink(&victim, &dst).unwrap();
        assert!(clone_file(&src, &dst).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    }

    /// Without reflink (tmpfs, ext4) the copy keeps the source's holes: an
    /// OS disk cloned from a 20 GiB image must not allocate 20 GiB. A hole
    /// at the end still counts toward the length.
    #[test]
    fn a_fallback_copy_stays_sparse() {
        use std::io::{Seek, SeekFrom, Write};
        use std::os::unix::fs::MetadataExt;
        const MIB: u64 = 1024 * 1024;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("image.raw");
        let mut f = std::fs::File::create(&src).unwrap();
        f.write_all(b"MBR").unwrap();
        f.seek(SeekFrom::Start(9 * MIB)).unwrap();
        f.write_all(b"data").unwrap();
        f.set_len(32 * MIB).unwrap();
        drop(f);

        let dst = dir.path().join("os.raw");
        let (_file, _kind) = clone_file(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), std::fs::read(&src).unwrap());
        let allocated = std::fs::metadata(&dst).unwrap().blocks() * 512;
        assert!(allocated < 2 * MIB, "{allocated} bytes allocated");
    }
}
