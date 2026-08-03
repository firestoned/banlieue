// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `xdr.rs`.
//!
//! Assertions are written against RFC 4506's *encoded bytes*, not just
//! round-trips. A codec that is self-consistently wrong round-trips perfectly
//! and still desynchronises the moment it meets a real libvirtd, so every
//! primitive has at least one test pinning its exact wire representation.

#[cfg(test)]
mod tests {
    use super::super::*;

    // ------------------------------------------------------------------
    // Integers — big-endian, fixed width, no padding
    // ------------------------------------------------------------------

    #[test]
    fn u32_encodes_big_endian() {
        let mut e = Encoder::new();
        e.write_u32(0x0102_0304);
        assert_eq!(e.as_bytes(), &[0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn i32_negative_is_twos_complement() {
        let mut e = Encoder::new();
        e.write_i32(-1);
        assert_eq!(e.as_bytes(), &[0xFF, 0xFF, 0xFF, 0xFF]);

        let mut d = Decoder::new(e.as_bytes());
        assert_eq!(d.read_i32().unwrap(), -1);
    }

    #[test]
    fn u64_hyper_encodes_big_endian_msb_first() {
        let mut e = Encoder::new();
        e.write_u64(0x0102_0304_0506_0708);
        assert_eq!(
            e.as_bytes(),
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
        );
    }

    #[test]
    fn i64_negative_round_trips() {
        let mut e = Encoder::new();
        e.write_i64(-2);
        let mut d = Decoder::new(e.as_bytes());
        assert_eq!(d.read_i64().unwrap(), -2);
    }

    #[test]
    fn integers_round_trip_at_boundaries() {
        let mut e = Encoder::new();
        e.write_u32(u32::MAX);
        e.write_i32(i32::MIN);
        e.write_u64(u64::MAX);
        e.write_i64(i64::MIN);
        let mut d = Decoder::new(e.as_bytes());
        assert_eq!(d.read_u32().unwrap(), u32::MAX);
        assert_eq!(d.read_i32().unwrap(), i32::MIN);
        assert_eq!(d.read_u64().unwrap(), u64::MAX);
        assert_eq!(d.read_i64().unwrap(), i64::MIN);
        assert_eq!(d.remaining(), 0);
    }

    // ------------------------------------------------------------------
    // Bool — a 32-bit enum of exactly 0 or 1
    // ------------------------------------------------------------------

    #[test]
    fn bool_encodes_as_four_byte_zero_or_one() {
        let mut e = Encoder::new();
        e.write_bool(false);
        e.write_bool(true);
        assert_eq!(e.as_bytes(), &[0, 0, 0, 0, 0, 0, 0, 1]);
    }

    #[test]
    fn bool_rejects_values_other_than_zero_or_one() {
        // A libvirtd would never send this, but a desynchronised stream will
        // produce arbitrary words here -- catching it early turns a silent
        // misparse into a clear error.
        let bytes = [0, 0, 0, 2];
        let mut d = Decoder::new(&bytes);
        assert!(matches!(d.read_bool(), Err(XdrError::InvalidBool(2))));
    }

    // ------------------------------------------------------------------
    // Fixed-length opaque — padded to a 4-byte boundary with ZERO bytes
    // ------------------------------------------------------------------

    #[test]
    fn opaque_fixed_pads_to_four_byte_boundary_with_zeros() {
        for (input, expected) in [
            (&b"a"[..], vec![b'a', 0, 0, 0]),
            (&b"ab"[..], vec![b'a', b'b', 0, 0]),
            (&b"abc"[..], vec![b'a', b'b', b'c', 0]),
            (&b"abcd"[..], vec![b'a', b'b', b'c', b'd']),
        ] {
            let mut e = Encoder::new();
            e.write_opaque_fixed(input);
            assert_eq!(e.as_bytes(), &expected[..], "input {input:?}");
            assert_eq!(e.as_bytes().len() % 4, 0);
        }
    }

    #[test]
    fn opaque_fixed_round_trips() {
        let mut e = Encoder::new();
        e.write_opaque_fixed(b"abc");
        let mut d = Decoder::new(e.as_bytes());
        assert_eq!(d.read_opaque_fixed(3).unwrap(), b"abc");
        assert_eq!(d.remaining(), 0, "padding must be consumed");
    }

    // ------------------------------------------------------------------
    // Variable-length opaque / string — u32 length prefix, then padded data
    // ------------------------------------------------------------------

    #[test]
    fn opaque_var_writes_length_prefix_then_padded_data() {
        let mut e = Encoder::new();
        e.write_opaque_var(b"abc");
        assert_eq!(e.as_bytes(), &[0, 0, 0, 3, b'a', b'b', b'c', 0]);
    }

    #[test]
    fn string_writes_length_prefix_then_padded_bytes() {
        let mut e = Encoder::new();
        e.write_string("hi");
        assert_eq!(e.as_bytes(), &[0, 0, 0, 2, b'h', b'i', 0, 0]);
    }

    #[test]
    fn string_round_trips_including_multibyte_utf8() {
        // Length is in BYTES, not characters -- an easy place to go wrong.
        let mut e = Encoder::new();
        e.write_string("héllo");
        let encoded_len = u32::from_be_bytes(e.as_bytes()[0..4].try_into().unwrap());
        assert_eq!(encoded_len, 6, "5 chars, 6 bytes");
        let mut d = Decoder::new(e.as_bytes());
        assert_eq!(d.read_string().unwrap(), "héllo");
        assert_eq!(d.remaining(), 0);
    }

    #[test]
    fn empty_string_and_opaque_encode_as_bare_zero_length() {
        let mut e = Encoder::new();
        e.write_string("");
        e.write_opaque_var(b"");
        assert_eq!(e.as_bytes(), &[0, 0, 0, 0, 0, 0, 0, 0]);

        let mut d = Decoder::new(e.as_bytes());
        assert_eq!(d.read_string().unwrap(), "");
        assert_eq!(d.read_opaque_var().unwrap(), b"");
    }

    #[test]
    fn string_rejects_invalid_utf8() {
        let bytes = [0, 0, 0, 1, 0xFF, 0, 0, 0];
        let mut d = Decoder::new(&bytes);
        assert!(matches!(d.read_string(), Err(XdrError::InvalidUtf8)));
    }

    // ------------------------------------------------------------------
    // Padding must be zero (RFC 4506)
    // ------------------------------------------------------------------

    #[test]
    fn decoder_rejects_non_zero_padding() {
        // "abc" + a padding byte of 0xFF instead of 0.
        let bytes = [0, 0, 0, 3, b'a', b'b', b'c', 0xFF];
        let mut d = Decoder::new(&bytes);
        assert!(matches!(d.read_opaque_var(), Err(XdrError::NonZeroPadding)));
    }

    // ------------------------------------------------------------------
    // Bounds — the security-relevant cases
    // ------------------------------------------------------------------

    #[test]
    fn decoder_reports_eof_rather_than_panicking() {
        let bytes = [0, 0, 0];
        let mut d = Decoder::new(&bytes);
        assert!(matches!(d.read_u32(), Err(XdrError::UnexpectedEof)));
    }

    #[test]
    fn absurd_length_prefix_is_rejected_without_allocating() {
        // A corrupt or hostile length must be bounds-checked against what is
        // actually buffered BEFORE any allocation. Trusting this u32 is the
        // classic way a protocol parser turns 4 bytes into a 4 GiB allocation.
        let bytes = [0xFF, 0xFF, 0xFF, 0xFF, b'a', b'b', b'c', b'd'];
        let mut d = Decoder::new(&bytes);
        assert!(matches!(d.read_opaque_var(), Err(XdrError::UnexpectedEof)));
    }

    #[test]
    fn length_just_past_the_end_is_rejected() {
        // Length 5 with only 4 bytes of payload available.
        let bytes = [0, 0, 0, 5, b'a', b'b', b'c', b'd'];
        let mut d = Decoder::new(&bytes);
        assert!(matches!(d.read_opaque_var(), Err(XdrError::UnexpectedEof)));
    }

    #[test]
    fn read_opaque_fixed_rejects_length_that_overflows_when_padded() {
        // `read_opaque_fixed` is public and takes a caller-supplied length,
        // and `read_opaque_var` feeds it a length read off the wire. A value
        // near usize::MAX plus its padding overflows: unchecked that panics in
        // debug and wraps to a SMALL number in release, sailing past the
        // bounds check and corrupting the cursor. Must be a clean error.
        let bytes = [0u8; 8];
        let mut d = Decoder::new(&bytes);
        assert!(matches!(
            d.read_opaque_fixed(usize::MAX),
            Err(XdrError::UnexpectedEof)
        ));
    }

    #[test]
    fn read_opaque_fixed_rejects_truncated_padding() {
        // 3 bytes of data present but the pad byte is missing entirely.
        let bytes = *b"abc";
        let mut d = Decoder::new(&bytes);
        assert!(matches!(
            d.read_opaque_fixed(3),
            Err(XdrError::UnexpectedEof)
        ));
    }

    // ------------------------------------------------------------------
    // Composition
    // ------------------------------------------------------------------

    #[test]
    fn mixed_sequence_round_trips_in_order() {
        let mut e = Encoder::new();
        e.write_u32(7);
        e.write_string("pool");
        e.write_bool(true);
        e.write_opaque_var(&[0xDE, 0xAD]);
        e.write_u64(1 << 40);

        let mut d = Decoder::new(e.as_bytes());
        assert_eq!(d.read_u32().unwrap(), 7);
        assert_eq!(d.read_string().unwrap(), "pool");
        assert!(d.read_bool().unwrap());
        assert_eq!(d.read_opaque_var().unwrap(), &[0xDE, 0xAD]);
        assert_eq!(d.read_u64().unwrap(), 1 << 40);
        assert_eq!(d.remaining(), 0);
    }

    #[test]
    fn everything_encodes_to_a_multiple_of_four_bytes() {
        // RFC 4506: "The representation of all items requires a multiple of
        // four bytes of data."
        for s in ["", "a", "ab", "abc", "abcd", "abcde"] {
            let mut e = Encoder::new();
            e.write_string(s);
            assert_eq!(e.as_bytes().len() % 4, 0, "string {s:?}");
        }
    }
}
