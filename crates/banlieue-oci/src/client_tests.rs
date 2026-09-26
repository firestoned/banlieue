// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `client.rs` that need no registry. The protocol itself is
//! exercised against a real registry in `tests/live_registry.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    /// The digest pushed describes exactly the compressed bytes uploaded,
    /// and they decompress back to the input.
    #[test]
    fn gzip_and_hash_describes_the_compressed_copy() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("disk.raw");
        let mut data = vec![0u8; 256 * 1024];
        data[..5].copy_from_slice(b"hello");
        std::fs::write(&src, &data).unwrap();
        let dst = dir.path().join("disk.raw.gz");

        let (digest, size) = gzip_and_hash(&src, File::create(&dst).unwrap()).unwrap();
        let compressed = std::fs::read(&dst).unwrap();
        assert_eq!(size, compressed.len() as u64);
        assert_eq!(digest, sha256_digest(&compressed));
        assert!(size < data.len() as u64 / 10, "zeros compress well: {size}");

        let mut round = Vec::new();
        flate2::read::GzDecoder::new(&compressed[..])
            .read_to_end(&mut round)
            .unwrap();
        assert_eq!(round, data);
    }

    /// The compressed copy goes in the scratch directory it is given, not
    /// beside the source: in the push Job the source is on a read-only PVC
    /// and the only writable place is the scratch emptyDir it names.
    #[test]
    fn compressed_copy_is_created_in_the_given_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let src_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("disk.raw");
        std::fs::write(&src, b"x").unwrap();
        std::fs::set_permissions(src_dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let tmp = compressed_copy_in(scratch.path(), &src);
        std::fs::set_permissions(src_dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let tmp = tmp.unwrap();
        assert_eq!(tmp.path().parent(), Some(scratch.path()));
        let name = tmp
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with(".disk.raw."), "{name}");
        assert!(name.ends_with(".gz"), "{name}");
    }

    /// Two pushes of the same file must not collide: the name is not
    /// predictable, so nothing can pre-create it either.
    #[test]
    fn compressed_copies_of_one_source_get_distinct_names() {
        let scratch = tempfile::tempdir().unwrap();
        let src = Path::new("/c/disk.raw");

        let a = compressed_copy_in(scratch.path(), src).unwrap();
        let b = compressed_copy_in(scratch.path(), src).unwrap();
        assert_ne!(a.path(), b.path());
    }

    /// The copy is the artifact's contents: readable by its owner only.
    #[test]
    fn compressed_copy_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = tempfile::tempdir().unwrap();

        let tmp = compressed_copy_in(scratch.path(), Path::new("/c/disk.raw")).unwrap();
        let mode = std::fs::metadata(tmp.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }

    /// Dropping it removes it, so an error anywhere in a push — including
    /// during compression — leaves nothing behind.
    #[test]
    fn compressed_copy_is_removed_when_dropped() {
        let scratch = tempfile::tempdir().unwrap();

        let tmp = compressed_copy_in(scratch.path(), Path::new("/c/disk.raw")).unwrap();
        let path = tmp.path().to_path_buf();
        drop(tmp);
        assert!(!path.exists());
    }

    #[test]
    fn with_suffix_appends() {
        assert_eq!(
            with_suffix(Path::new("/c/images/sha256-x.raw"), ".partial"),
            PathBuf::from("/c/images/sha256-x.raw.partial")
        );
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(
            percent_encode("repository:org/disk:pull,push"),
            "repository%3Aorg%2Fdisk%3Apull%2Cpush"
        );
        assert_eq!(percent_encode("ghcr.io"), "ghcr.io");
    }
}
