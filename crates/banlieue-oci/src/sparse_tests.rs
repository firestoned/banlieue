// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `sparse.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::os::unix::fs::MetadataExt;

    const MIB: usize = 1024 * 1024;

    /// Content is exact, zero runs cost no space, and odd write sizes (the
    /// decompressor's) do not matter.
    #[test]
    fn zero_runs_become_holes_and_content_is_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.raw");
        let mut expected = vec![0u8; 8 * MIB];
        expected[..3].copy_from_slice(b"MBR");
        expected[5 * MIB + 17] = 0xAA;
        // Ends in a hole, which only set_len can represent.

        let mut w = SparseWriter::new(File::create(&path).unwrap());
        for chunk in expected.chunks(1_000) {
            w.write_all(chunk).unwrap();
        }
        let (_f, len) = w.finish().unwrap();

        assert_eq!(len, expected.len() as u64);
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        let allocated = std::fs::metadata(&path).unwrap().blocks() * 512;
        assert!(
            allocated < (MIB as u64),
            "8 MiB of mostly zeros allocated {allocated} bytes"
        );
    }

    #[test]
    fn a_short_non_block_tail_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t");
        let mut w = SparseWriter::new(File::create(&path).unwrap());
        w.write_all(b"abc").unwrap();
        let (_f, len) = w.finish().unwrap();
        assert_eq!(len, 3);
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
    }

    /// A layer that decompresses to more than its digest-covered declared
    /// size is refused mid-stream: a hostile layer cannot fill the disk.
    #[test]
    fn writing_past_the_limit_fails() {
        use std::io::Write;
        let f = tempfile::tempfile().unwrap();
        let mut w = SparseWriter::with_limit(f, 10);
        w.write_all(b"0123456789").unwrap();
        assert!(w.write_all(b"x").is_err());

        let mut w = SparseWriter::with_limit(tempfile::tempfile().unwrap(), 10);
        assert!(w.write_all(&[0u8; 11]).is_err(), "zeros count too");
    }
}
