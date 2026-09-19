// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `cloudinit/iso9660.rs`.
//!
//! Every structure in ISO9660 is bytes at a known offset, so these read the
//! image back field by field. That proves self-consistency and nothing more
//! — only a guest booting from one proves the format right, which is what
//! `tests/live_machine.rs` is for (ADR-0054 Decision 3).

#[cfg(test)]
mod tests {
    use super::super::*;

    const SECTOR: usize = 2048;

    fn seed() -> Vec<u8> {
        build_iso9660(
            "CIDATA",
            &[
                IsoFile::new("meta-data", b"instance-id: iid-01\n".to_vec()),
                IsoFile::new("user-data", b"#cloud-config\nhostname: x\n".to_vec()),
            ],
        )
        .expect("build")
    }

    fn sector(img: &[u8], n: usize) -> &[u8] {
        &img[n * SECTOR..(n + 1) * SECTOR]
    }

    // ------------------------------------------------------------------
    // Overall shape
    // ------------------------------------------------------------------

    #[test]
    fn image_is_a_whole_number_of_sectors() {
        let img = seed();
        assert_eq!(img.len() % SECTOR, 0, "len {}", img.len());
    }

    /// The first 16 sectors are the system area and must be zero — some
    /// loaders read them, and a boot sector is exactly what a seed must not
    /// look like.
    #[test]
    fn the_system_area_is_reserved_and_zeroed() {
        let img = seed();
        assert!(img[..16 * SECTOR].iter().all(|&b| b == 0));
    }

    // ------------------------------------------------------------------
    // Volume descriptors
    // ------------------------------------------------------------------

    /// Every volume descriptor carries `CD001` at offset 1. Sector 16 is the
    /// Primary, 17 the Joliet Supplementary, 18 the terminator.
    #[test]
    fn the_descriptor_sequence_is_primary_joliet_terminator() {
        let img = seed();
        for n in [16, 17, 18] {
            assert_eq!(&sector(&img, n)[1..6], b"CD001", "sector {n}");
            assert_eq!(sector(&img, n)[6], 1, "descriptor version, sector {n}");
        }
        assert_eq!(sector(&img, 16)[0], 1, "primary volume descriptor");
        assert_eq!(sector(&img, 17)[0], 2, "supplementary volume descriptor");
        assert_eq!(sector(&img, 18)[0], 255, "terminator");
    }

    /// The escape sequence is what makes a supplementary descriptor *Joliet*
    /// rather than merely supplementary. `%/E` is UCS-2 level 3.
    #[test]
    fn the_supplementary_descriptor_declares_joliet_ucs2() {
        let img = seed();
        assert_eq!(&sector(&img, 17)[88..91], b"%/E");
    }

    /// The label cloud-init matches on. Space-padded to 32 bytes in the
    /// primary descriptor, UCS-2BE in the Joliet one.
    #[test]
    fn the_volume_label_is_the_one_cloud_init_looks_for() {
        let img = seed();
        let pvd_label = &sector(&img, 16)[40..72];
        assert_eq!(&pvd_label[..6], b"CIDATA");
        assert!(pvd_label[6..].iter().all(|&b| b == b' '), "space-padded");

        // Joliet: same text, UCS-2 big endian.
        let svd_label = &sector(&img, 17)[40..72];
        assert_eq!(&svd_label[..12], b"\0C\0I\0D\0A\0T\0A");
    }

    /// Both descriptors must agree on the image size, or a mount reads past
    /// the end.
    #[test]
    fn both_descriptors_report_the_real_volume_size() {
        let img = seed();
        let sectors = (img.len() / SECTOR) as u32;
        for n in [16, 17] {
            let both = &sector(&img, n)[80..88];
            assert_eq!(u32::from_le_bytes(both[0..4].try_into().unwrap()), sectors);
            assert_eq!(u32::from_be_bytes(both[4..8].try_into().unwrap()), sectors);
        }
    }

    // ------------------------------------------------------------------
    // Names — the entire reason Joliet is here (ADR-0054)
    // ------------------------------------------------------------------

    /// The point of the whole exercise. `meta-data` is nine characters with
    /// a hyphen: unrepresentable in ISO9660 Level 1, which is why the
    /// documented `genisoimage` command passes `-joliet`. Linux prefers the
    /// Joliet tree when mounting, so this is the name cloud-init sees.
    #[test]
    fn joliet_carries_the_hyphenated_names_verbatim() {
        let img = seed();
        let ucs2: Vec<u8> = "meta-data"
            .encode_utf16()
            .flat_map(|c| c.to_be_bytes())
            .collect();
        assert!(
            img.windows(ucs2.len()).any(|w| w == ucs2),
            "meta-data must appear as UCS-2BE in the Joliet tree"
        );
        let ucs2_user: Vec<u8> = "user-data"
            .encode_utf16()
            .flat_map(|c| c.to_be_bytes())
            .collect();
        assert!(img.windows(ucs2_user.len()).any(|w| w == ucs2_user));
    }

    /// The primary tree still has to be valid ISO9660, so the same files
    /// appear there under mapped 8.3 names. A guest that ignores Joliet
    /// finds *something*; a guest that honours it finds the right thing.
    #[test]
    fn the_primary_tree_uses_legal_iso9660_names() {
        let img = seed();
        assert!(
            img.windows(10).any(|w| w == b"META_DATA."),
            "hyphen maps to underscore in the primary tree"
        );
    }

    // ------------------------------------------------------------------
    // File content
    // ------------------------------------------------------------------

    #[test]
    fn file_contents_are_present_verbatim() {
        let img = seed();
        assert!(img.windows(20).any(|w| w == b"instance-id: iid-01\n"));
        assert!(
            img.windows(13).any(|w| w == b"#cloud-config"),
            "user-data must be byte-identical"
        );
    }

    /// Each file starts on its own sector boundary, because a directory
    /// record addresses an extent by sector.
    #[test]
    fn each_file_starts_on_a_sector_boundary() {
        // Markers chosen so they cannot collide with a name: the Joliet
        // tree spells names in UCS-2BE, so single ASCII letters from those
        // names appear in the directory records too.
        let img = build_iso9660(
            "CIDATA",
            &[
                IsoFile::new("meta-data", b"<<<FIRST>>>".to_vec()),
                IsoFile::new("user-data", b"<<<SECOND>>>".to_vec()),
            ],
        )
        .unwrap();
        let a = img
            .windows(11)
            .position(|w| w == b"<<<FIRST>>>")
            .expect("first file content");
        let b = img
            .windows(12)
            .position(|w| w == b"<<<SECOND>>>")
            .expect("second file content");
        assert_eq!(a % SECTOR, 0, "first file not sector-aligned");
        assert_eq!(b % SECTOR, 0, "second file not sector-aligned");
        assert_ne!(a, b, "files must not share an extent");
    }

    /// A file larger than one sector must still work — user-data routinely
    /// exceeds 2 KiB.
    #[test]
    fn a_multi_sector_file_is_stored_whole() {
        let big = vec![b'z'; SECTOR * 3 + 7];
        let img = build_iso9660("CIDATA", &[IsoFile::new("user-data", big.clone())]).unwrap();
        assert!(
            img.windows(big.len()).any(|w| w == big.as_slice()),
            "a multi-sector payload must be contiguous"
        );
    }

    #[test]
    fn an_empty_file_is_allowed() {
        let img = build_iso9660("CIDATA", &[IsoFile::new("meta-data", Vec::new())]);
        assert!(img.is_ok(), "cloud-init accepts an empty meta-data");
    }

    // ------------------------------------------------------------------
    // Rejected input
    // ------------------------------------------------------------------

    /// Flat by design (ADR-0054 Decision 4). Silently flattening a nested
    /// path would put a file somewhere the caller did not ask for.
    #[test]
    fn a_nested_path_is_rejected_not_flattened() {
        let err = build_iso9660("CIDATA", &[IsoFile::new("nested/user-data", b"x".to_vec())])
            .unwrap_err();
        assert!(matches!(err, IsoError::NestedPath { .. }), "{err:?}");
    }

    #[test]
    fn an_empty_name_is_rejected() {
        let err = build_iso9660("CIDATA", &[IsoFile::new("", b"x".to_vec())]).unwrap_err();
        assert!(matches!(err, IsoError::EmptyName), "{err:?}");
    }

    /// A volume label is 32 bytes; a longer one would silently truncate into
    /// something cloud-init does not match.
    #[test]
    fn an_over_long_volume_label_is_rejected() {
        let err = build_iso9660("X".repeat(33).as_str(), &[]).unwrap_err();
        assert!(matches!(err, IsoError::LabelTooLong { .. }), "{err:?}");
    }

    #[test]
    fn a_name_too_long_for_the_primary_tree_is_rejected() {
        let long = format!("{}.txt", "a".repeat(40));
        let err = build_iso9660("CIDATA", &[IsoFile::new(&long, b"x".to_vec())]).unwrap_err();
        assert!(matches!(err, IsoError::NameTooLong { .. }), "{err:?}");
    }

    // ------------------------------------------------------------------
    // Determinism
    // ------------------------------------------------------------------

    /// The same inputs must produce the same bytes. A reconciler rebuilds
    /// this every pass; if the image churned, it would rewrite the volume
    /// on a host forever.
    #[test]
    fn the_same_input_produces_identical_bytes() {
        assert_eq!(seed(), seed());
    }
}
