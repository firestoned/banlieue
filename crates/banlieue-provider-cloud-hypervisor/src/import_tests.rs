// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `import.rs`, on real directories. The pull itself is
//! covered against a real registry by `banlieue-oci`'s live tests.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn a_held_image_is_copied_not_pulled() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(b.path().join("x.raw"), b"disk").unwrap();
        let dirs = vec![a.path().to_path_buf(), b.path().to_path_buf()];

        let plan = copy_plan(&dirs, "x.raw");
        assert_eq!(plan.source, Some(b.path().join("x.raw")));
        assert_eq!(plan.missing, vec![a.path().join("x.raw")]);

        place_copy(&b.path().join("x.raw"), &a.path().join("x.raw")).unwrap();
        assert_eq!(std::fs::read(a.path().join("x.raw")).unwrap(), b"disk");
        assert!(!a.path().join("x.raw.partial").exists());
        // Like an admin-placed image: owner and group only (found live as
        // 0644 from the default umask).
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(a.path().join("x.raw"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, CACHE_FILE_MODE);
        assert_eq!(copy_plan(&dirs, "x.raw").missing, Vec::<PathBuf>::new());
    }

    #[test]
    fn nothing_held_means_pull_into_the_first() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let dirs = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        let plan = copy_plan(&dirs, "x.raw");
        assert!(plan.source.is_none());
        assert_eq!(plan.missing.len(), 2);
    }

    /// A stale partial from a crashed run is replaced; a symlink planted at
    /// the final name is not written through (the rename replaces it).
    #[test]
    fn a_stale_partial_is_replaced() {
        let a = tempfile::tempdir().unwrap();
        let src = a.path().join("src.raw");
        std::fs::write(&src, b"disk").unwrap();
        std::fs::write(a.path().join("x.raw.partial"), b"junk").unwrap();
        place_copy(&src, &a.path().join("x.raw")).unwrap();
        assert_eq!(std::fs::read(a.path().join("x.raw")).unwrap(), b"disk");
    }
}
