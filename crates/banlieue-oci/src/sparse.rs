// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A writer that turns runs of zeros into holes.
//!
//! A disk image is mostly zeros: a 20 GiB Kairos disk holds a few GiB of
//! data. Writing the decompressed stream byte for byte would allocate all
//! of it on the host; seeking over zero blocks instead keeps the file as
//! sparse as the original, and the final `set_len` fixes the size when the
//! image ends in a hole.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};

/// Granularity of hole detection: a filesystem block is 4 KiB on every
/// filesystem banlieue targets, and a smaller hole would not be one.
pub const BLOCK: usize = 4096;

/// Wraps a file; zero blocks are skipped rather than written.
pub struct SparseWriter {
    file: File,
    /// Logical length written so far, holes included.
    len: u64,
    /// Bytes not yet a whole block.
    pending: Vec<u8>,
    /// Most logical bytes accepted, holes included.
    limit: u64,
}

impl SparseWriter {
    /// Write into `file`, which should be new and empty.
    #[must_use]
    pub fn new(file: File) -> Self {
        Self::with_limit(file, u64::MAX)
    }

    /// Like [`Self::new`], refusing to accept more than `limit` bytes.
    #[must_use]
    pub fn with_limit(file: File, limit: u64) -> Self {
        Self {
            file,
            len: 0,
            pending: Vec::with_capacity(BLOCK),
            limit,
        }
    }

    fn emit(&mut self, block: &[u8]) -> io::Result<()> {
        if block.iter().all(|b| *b == 0) {
            self.file.seek(SeekFrom::Current(
                i64::try_from(block.len()).unwrap_or(i64::MAX),
            ))?;
        } else {
            self.file.write_all(block)?;
        }
        self.len += block.len() as u64;
        Ok(())
    }

    /// Flush the tail, set the final length (a trailing hole would
    /// otherwise be missing) and return the file.
    ///
    /// # Errors
    /// The I/O error.
    pub fn finish(mut self) -> io::Result<(File, u64)> {
        let tail = std::mem::take(&mut self.pending);
        if !tail.is_empty() {
            self.emit(&tail)?;
        }
        self.file.set_len(self.len)?;
        Ok((self.file, self.len))
    }
}

impl Write for SparseWriter {
    fn write(&mut self, mut buf: &[u8]) -> io::Result<usize> {
        let n = buf.len();
        let accepted = self.len + self.pending.len() as u64 + n as u64;
        if accepted > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("more than the declared {} bytes", self.limit),
            ));
        }
        if !self.pending.is_empty() {
            let take = (BLOCK - self.pending.len()).min(buf.len());
            self.pending.extend_from_slice(&buf[..take]);
            buf = &buf[take..];
            if self.pending.len() == BLOCK {
                let block = std::mem::take(&mut self.pending);
                self.emit(&block)?;
                self.pending = block;
                self.pending.clear();
            }
        }
        while buf.len() >= BLOCK {
            let (block, rest) = buf.split_at(BLOCK);
            self.emit(block)?;
            buf = rest;
        }
        self.pending.extend_from_slice(buf);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
#[path = "sparse_tests.rs"]
mod sparse_tests;
