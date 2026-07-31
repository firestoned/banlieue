// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! XDR (External Data Representation, RFC 4506) codec.
//!
//! libvirt's RPC payloads are XDR-encoded, so this is the foundation the rest
//! of the protocol sits on. The rules that matter:
//!
//! - Everything is **big-endian** and occupies a **multiple of four bytes**.
//! - Variable-length items carry a `u32` length prefix, then the bytes, then
//!   0–3 **zero** padding bytes to reach the next 4-byte boundary.
//! - Padding MUST be zero, and this decoder enforces that rather than skipping
//!   it: on a desynchronised stream, non-zero padding is often the first
//!   observable symptom, and failing there gives a far better error than
//!   misparsing everything downstream.
//!
//! [`Decoder`] never trusts a length prefix. A `u32` read off the wire is
//! bounds-checked against the bytes actually buffered *before* anything is
//! allocated — otherwise four hostile bytes become a multi-gigabyte
//! allocation, which is the classic way a protocol parser becomes a denial of
//! service.

/// Every XDR item occupies a multiple of this many bytes (RFC 4506 §3).
const XDR_ALIGNMENT: usize = 4;

/// Wire size of an XDR `int` / `unsigned int` / `bool` / `enum`.
const XDR_INT_LEN: usize = 4;

/// Wire size of an XDR `hyper` / `unsigned hyper`.
const XDR_HYPER_LEN: usize = 8;

/// XDR encodes `FALSE` as 0 and `TRUE` as 1; nothing else is legal.
const XDR_FALSE: u32 = 0;
const XDR_TRUE: u32 = 1;

/// Errors produced while decoding XDR.
///
/// Encoding cannot fail: it only ever appends to an owned buffer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XdrError {
    /// Fewer bytes remain than the item requires. Also returned for a length
    /// prefix larger than the buffered remainder, so a corrupt or hostile
    /// length is rejected before any allocation.
    #[error("unexpected end of input")]
    UnexpectedEof,

    /// Padding bytes were not zero, contrary to RFC 4506 §3. Usually the first
    /// visible sign that the stream has desynchronised.
    #[error("non-zero padding byte")]
    NonZeroPadding,

    /// A boolean was neither 0 nor 1.
    #[error("invalid boolean encoding: {0}")]
    InvalidBool(u32),

    /// A string was not valid UTF-8.
    #[error("string is not valid UTF-8")]
    InvalidUtf8,
}

/// Convenient alias.
pub type Result<T> = std::result::Result<T, XdrError>;

/// Bytes of zero padding needed to round `len` up to the XDR alignment.
#[inline]
const fn padding_for(len: usize) -> usize {
    // (4 - len % 4) % 4, without a branch.
    (XDR_ALIGNMENT - (len % XDR_ALIGNMENT)) % XDR_ALIGNMENT
}

/// Builds an XDR byte stream.
#[derive(Debug, Default, Clone)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    /// Create an empty encoder.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Borrow the encoded bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Consume the encoder and yield the encoded bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// Number of bytes encoded so far. Always a multiple of 4.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing has been encoded yet.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Append an XDR `int`.
    pub fn write_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Append an XDR `unsigned int`.
    pub fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Append an XDR `hyper`.
    pub fn write_i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Append an XDR `unsigned hyper`.
    pub fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Append an XDR `bool` (a 4-byte 0 or 1).
    pub fn write_bool(&mut self, v: bool) {
        self.write_u32(if v { XDR_TRUE } else { XDR_FALSE });
    }

    /// Append fixed-length opaque data: the bytes, then zero padding. No
    /// length prefix — the length is part of the protocol definition, so the
    /// caller and the decoder must agree on it out of band.
    pub fn write_opaque_fixed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        self.pad_to_alignment(bytes.len());
    }

    /// Append variable-length opaque data: a `u32` length, the bytes, then
    /// zero padding.
    pub fn write_opaque_var(&mut self, bytes: &[u8]) {
        self.write_u32(bytes.len() as u32);
        self.write_opaque_fixed(bytes);
    }

    /// Append an XDR `string`. Encoded exactly like variable-length opaque;
    /// the length is in **bytes**, not characters.
    pub fn write_string(&mut self, s: &str) {
        self.write_opaque_var(s.as_bytes());
    }

    fn pad_to_alignment(&mut self, written: usize) {
        for _ in 0..padding_for(written) {
            self.buf.push(0);
        }
    }
}

/// Reads an XDR byte stream.
#[derive(Debug, Clone)]
pub struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    /// Create a decoder over `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bytes not yet consumed.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Whether every byte has been consumed.
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Read an XDR `int`.
    ///
    /// # Errors
    /// [`XdrError::UnexpectedEof`] if fewer than 4 bytes remain.
    pub fn read_i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take_array::<XDR_INT_LEN>()?))
    }

    /// Read an XDR `unsigned int`.
    ///
    /// # Errors
    /// [`XdrError::UnexpectedEof`] if fewer than 4 bytes remain.
    pub fn read_u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take_array::<XDR_INT_LEN>()?))
    }

    /// Read an XDR `hyper`.
    ///
    /// # Errors
    /// [`XdrError::UnexpectedEof`] if fewer than 8 bytes remain.
    pub fn read_i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take_array::<XDR_HYPER_LEN>()?))
    }

    /// Read an XDR `unsigned hyper`.
    ///
    /// # Errors
    /// [`XdrError::UnexpectedEof`] if fewer than 8 bytes remain.
    pub fn read_u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take_array::<XDR_HYPER_LEN>()?))
    }

    /// Read an XDR `bool`.
    ///
    /// # Errors
    /// [`XdrError::UnexpectedEof`] if fewer than 4 bytes remain, or
    /// [`XdrError::InvalidBool`] if the value is neither 0 nor 1.
    pub fn read_bool(&mut self) -> Result<bool> {
        match self.read_u32()? {
            XDR_FALSE => Ok(false),
            XDR_TRUE => Ok(true),
            other => Err(XdrError::InvalidBool(other)),
        }
    }

    /// Read `len` bytes of fixed-length opaque data plus its padding.
    ///
    /// # Errors
    /// [`XdrError::UnexpectedEof`] if the data or its padding is truncated, or
    /// [`XdrError::NonZeroPadding`] if a padding byte is not zero.
    pub fn read_opaque_fixed(&mut self, len: usize) -> Result<&'a [u8]> {
        let pad = padding_for(len);
        // `checked_add`, not `+`: `len` may come straight off the wire via
        // read_opaque_var, and a near-usize::MAX value plus its padding
        // overflows. Unchecked, that panics in debug and — far worse — wraps
        // to a small number in release, so the bounds check below would pass
        // and `self.pos += len` would then corrupt the cursor.
        let needed = len.checked_add(pad).ok_or(XdrError::UnexpectedEof)?;
        // Check data AND padding together: a stream that ends mid-padding is
        // truncated, not merely unpadded.
        if self.remaining() < needed {
            return Err(XdrError::UnexpectedEof);
        }
        let start = self.pos;
        self.pos += len;
        for _ in 0..pad {
            if self.buf[self.pos] != 0 {
                return Err(XdrError::NonZeroPadding);
            }
            self.pos += 1;
        }
        Ok(&self.buf[start..start + len])
    }

    /// Read variable-length opaque data (`u32` length, bytes, padding).
    ///
    /// The length prefix is validated against the remaining input before any
    /// slicing, so a corrupt or hostile length yields
    /// [`XdrError::UnexpectedEof`] rather than a huge allocation.
    ///
    /// # Errors
    /// [`XdrError::UnexpectedEof`] or [`XdrError::NonZeroPadding`].
    pub fn read_opaque_var(&mut self) -> Result<&'a [u8]> {
        let len = self.read_u32()? as usize;
        // Redundant with the check inside read_opaque_fixed, but explicit: the
        // length came off the wire and is not to be trusted.
        if len > self.remaining() {
            return Err(XdrError::UnexpectedEof);
        }
        self.read_opaque_fixed(len)
    }

    /// Read an XDR `string`.
    ///
    /// # Errors
    /// As [`read_opaque_var`](Self::read_opaque_var), plus
    /// [`XdrError::InvalidUtf8`] if the bytes are not valid UTF-8.
    pub fn read_string(&mut self) -> Result<&'a str> {
        let bytes = self.read_opaque_var()?;
        std::str::from_utf8(bytes).map_err(|_| XdrError::InvalidUtf8)
    }

    /// Take exactly `N` bytes, advancing the cursor.
    fn take_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        if self.remaining() < N {
            return Err(XdrError::UnexpectedEof);
        }
        let mut out = [0u8; N];
        out.copy_from_slice(&self.buf[self.pos..self.pos + N]);
        self.pos += N;
        Ok(out)
    }
}

#[cfg(test)]
#[path = "xdr_tests.rs"]
mod xdr_tests;
