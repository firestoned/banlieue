// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `vhd.rs`.

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};

    use super::super::*;

    /// The first 96 bytes of the footer `qemu-img convert -O vpc -o
    /// subformat=fixed,force_size` (QEMU 9 on Debian 13) wrote for a raw
    /// disk of 2,503,999,488 bytes, the size of the spike's auroraboot
    /// image. The remaining 416 bytes were zero. Captured once on the build
    /// host, 2026-11-26.
    const QEMU_FOOTER_HEAD_HEX: &str = "636f6e65637469780000000200010000ffffffffffffffff329a7e9d71656d32000500035769326b00000000954000000000000095400000ffff10ff00000002ffffe1b3ca8a5370239645f88b50e1f8d0eb4eb0000000000000000000000000";
    const SPIKE_RAW_LEN: u64 = 2_503_999_488;
    const SPIKE_VHD_LEN: u64 = 2_504_000_000;
    const ID: [u8; 16] = [7u8; 16];

    fn qemu_footer() -> [u8; FOOTER_LEN] {
        let mut f = [0u8; FOOTER_LEN];
        let bytes: Vec<u8> = (0..QEMU_FOOTER_HEAD_HEX.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&QEMU_FOOTER_HEAD_HEX[i..i + 2], 16).unwrap())
            .collect();
        f[..bytes.len()].copy_from_slice(&bytes);
        f
    }

    #[test]
    fn qemu_written_footer_parses_and_its_checksum_verifies() {
        let parsed = parse_footer(&qemu_footer()).expect("qemu footer is valid");
        assert_eq!(parsed.features, FEATURES_RESERVED);
        assert_eq!(parsed.format_version, FORMAT_VERSION);
        assert_eq!(parsed.data_offset, FIXED_DATA_OFFSET);
        assert_eq!(parsed.disk_type, DISK_TYPE_FIXED);
        assert_eq!(&parsed.creator_host_os, CREATOR_HOST_OS_WI2K);
        assert_eq!(parsed.current_size, SPIKE_RAW_LEN);
        assert_eq!(parsed.original_size, SPIKE_RAW_LEN);
    }

    #[test]
    fn our_footer_matches_qemu_in_every_field_azure_reads() {
        let ours = parse_footer(&fixed_footer(SPIKE_RAW_LEN, 0, ID)).unwrap();
        let qemu = parse_footer(&qemu_footer()).unwrap();
        assert_eq!(ours.features, qemu.features);
        assert_eq!(ours.format_version, qemu.format_version);
        assert_eq!(ours.data_offset, qemu.data_offset);
        assert_eq!(ours.creator_host_os, qemu.creator_host_os);
        assert_eq!(ours.current_size, qemu.current_size);
        assert_eq!(ours.original_size, qemu.original_size);
        assert_eq!(ours.disk_type, qemu.disk_type);
        assert_eq!(ours.saved_state, qemu.saved_state);
        // qemu's `force_size` writes the maximum geometry as a marker that
        // the size field is authoritative; we follow the spec's algorithm.
        assert_eq!(qemu.geometry.cylinders, u16::MAX);
    }

    #[test]
    fn footer_has_cookie_type_creator_and_a_valid_checksum() {
        let f = fixed_footer(SPIKE_RAW_LEN, 12_345, ID);
        assert_eq!(&f[..8], b"conectix");
        let parsed = parse_footer(&f).unwrap();
        assert_eq!(&parsed.creator_app, CREATOR_APP);
        assert_eq!(parsed.timestamp, 12_345);
        assert_eq!(parsed.unique_id, ID);
        assert_eq!(parsed.checksum, footer_checksum(&f));
        assert_eq!(parsed.geometry, chs_geometry(SPIKE_RAW_LEN));
    }

    #[test]
    fn a_flipped_byte_fails_the_checksum() {
        let mut f = fixed_footer(SPIKE_RAW_LEN, 0, ID);
        f[100] ^= 1;
        assert!(matches!(parse_footer(&f), Err(VhdError::InvalidFooter(_))));
    }

    #[test]
    fn a_wrong_cookie_is_refused() {
        let mut f = fixed_footer(SPIKE_RAW_LEN, 0, ID);
        f[0] = b'x';
        assert!(matches!(parse_footer(&f), Err(VhdError::InvalidFooter(_))));
    }

    #[test]
    fn checksum_is_ones_complement_of_the_byte_sum() {
        let mut f = [0u8; FOOTER_LEN];
        f[0] = 1;
        f[1] = 2;
        // The checksum field itself is excluded.
        f[64] = 0xff;
        assert_eq!(footer_checksum(&f), !3u32);
    }

    /// Hand-computed with the spec's algorithm; 1 GiB is the 2080/16/63
    /// geometry Hyper-V writes for that size.
    #[test]
    fn geometry_follows_the_spec_algorithm() {
        let cases: [(u64, (u16, u8, u8)); 5] = [
            (20 * MIB, (602, 4, 17)),
            (GIB, (2080, 16, 63)),
            (SPIKE_RAW_LEN, (4851, 16, 63)),
            (127 * GIB, (65_278, 16, 255)),
            (200 * GIB, (65_535, 16, 255)),
        ];
        for (size, (c, h, s)) in cases {
            assert_eq!(
                chs_geometry(size),
                Geometry {
                    cylinders: c,
                    heads: h,
                    sectors_per_track: s
                },
                "size {size}"
            );
        }
    }

    #[test]
    fn geometry_never_addresses_more_than_the_disk() {
        for size in [MIB, 20 * MIB, 3 * GIB, 31 * GIB, SPIKE_RAW_LEN] {
            assert!(chs_geometry(size).sectors() <= size / SECTOR_SIZE);
        }
    }

    #[test]
    fn timestamp_counts_from_2000() {
        assert_eq!(vhd_timestamp(VHD_EPOCH_UNIX_SECS), 0);
        assert_eq!(vhd_timestamp(VHD_EPOCH_UNIX_SECS + 60), 60);
        assert_eq!(vhd_timestamp(0), 0);
    }

    #[test]
    fn layout_of_an_aligned_disk_has_no_padding() {
        let l = FixedVhdLayout::for_raw_len(SPIKE_RAW_LEN).unwrap();
        assert_eq!(l.virtual_size, SPIKE_RAW_LEN);
        assert_eq!(l.padding, 0);
        assert_eq!(l.total_len, SPIKE_VHD_LEN);
        assert_eq!(l.min_disk_gib(), 3);
    }

    #[test]
    fn layout_rounds_up_to_the_next_mib() {
        let l = FixedVhdLayout::for_raw_len(MIB + 1).unwrap();
        assert_eq!(l.virtual_size, 2 * MIB);
        assert_eq!(l.padding, MIB - 1);
        assert_eq!(l.total_len, 2 * MIB + FOOTER_LEN_U64);
        assert_eq!(l.virtual_size % MIB, 0);
        assert_eq!(l.total_len % SECTOR_SIZE, 0);
        assert_eq!(l.min_disk_gib(), 1);
    }

    #[test]
    fn min_disk_is_whole_gib_rounded_up() {
        assert_eq!(FixedVhdLayout::for_raw_len(GIB).unwrap().min_disk_gib(), 1);
        assert_eq!(
            FixedVhdLayout::for_raw_len(GIB + 1).unwrap().min_disk_gib(),
            2
        );
    }

    #[test]
    fn an_empty_disk_or_an_overflow_is_refused() {
        assert_eq!(FixedVhdLayout::for_raw_len(0), Err(VhdError::Empty));
        assert!(matches!(
            FixedVhdLayout::for_raw_len(u64::MAX - 1),
            Err(VhdError::TooLarge(_))
        ));
    }

    #[test]
    fn all_zero_detection() {
        assert!(is_all_zero(&[0u8; 4096]));
        let mut b = [0u8; 4096];
        b[4095] = 1;
        assert!(!is_all_zero(&b));
        assert!(is_all_zero(&[]));
    }

    /// A reader that yields at most `step` bytes per call, as a file or a
    /// socket may.
    struct Trickle {
        inner: Cursor<Vec<u8>>,
        step: usize,
    }

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.step);
            self.inner.read(&mut buf[..n])
        }
    }

    #[test]
    fn reader_streams_raw_then_padding_then_footer() {
        let raw: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8 + 1).collect();
        let layout = FixedVhdLayout::for_raw_len(raw.len() as u64).unwrap();
        let footer = fixed_footer(layout.virtual_size, 0, ID);
        let mut out = Vec::new();
        FixedVhdReader::new(
            Trickle {
                inner: Cursor::new(raw.clone()),
                step: 7,
            },
            layout,
            footer,
        )
        .read_to_end(&mut out)
        .unwrap();
        assert_eq!(out.len() as u64, layout.total_len);
        assert_eq!(&out[..raw.len()], &raw[..]);
        assert!(is_all_zero(
            &out[raw.len()..usize::try_from(layout.virtual_size).unwrap()]
        ));
        assert_eq!(&out[out.len() - FOOTER_LEN..], &footer[..]);
    }

    #[test]
    fn reader_refuses_a_short_source() {
        let layout = FixedVhdLayout::for_raw_len(1000).unwrap();
        let footer = fixed_footer(layout.virtual_size, 0, ID);
        let mut out = Vec::new();
        let err = FixedVhdReader::new(Cursor::new(vec![1u8; 999]), layout, footer)
            .read_to_end(&mut out)
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn reader_refuses_a_long_source() {
        let layout = FixedVhdLayout::for_raw_len(1000).unwrap();
        let footer = fixed_footer(layout.virtual_size, 0, ID);
        let mut out = Vec::new();
        let err = FixedVhdReader::new(Cursor::new(vec![1u8; 1001]), layout, footer)
            .read_to_end(&mut out)
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
