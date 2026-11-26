// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! An in-memory page blob: the upload side of a managed disk, for unit
//! tests.
//!
//! It refuses what the Blob service refuses (rules/testing.md, rule 1): a
//! write that is empty, not 512-byte aligned, longer than 4 MiB, or past
//! the blob's end. It can fail the next writes with a 503, as a busy
//! service does, and reports exactly which ranges were written, so a test
//! can check the upload wrote the non-zero content and nothing else.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::upload::{PageWriter, check_page_range};

const HTTP_SERVICE_UNAVAILABLE: u16 = 503;
const CODE_SERVER_BUSY: &str = "ServerBusy";
const CODE_INVALID_RANGE: &str = "InvalidPageRange";

/// A page blob of a fixed size.
#[derive(Debug)]
pub struct FakePageBlob {
    size: u64,
    pages: Mutex<BTreeMap<u64, Vec<u8>>>,
    fail_next: Mutex<u32>,
    writes: AtomicU32,
}

impl FakePageBlob {
    /// An empty (all-zero) blob of `size` bytes.
    #[must_use]
    pub fn new(size: u64) -> Self {
        Self {
            size,
            pages: Mutex::new(BTreeMap::new()),
            fail_next: Mutex::new(0),
            writes: AtomicU32::new(0),
        }
    }

    /// The blob's size.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The next `n` writes fail with a retryable 503.
    pub fn fail_next_writes(&self, n: u32) {
        *self
            .fail_next
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = n;
    }

    /// Successful writes so far.
    #[must_use]
    pub fn writes(&self) -> u32 {
        self.writes.load(Ordering::SeqCst)
    }

    fn pages(&self) -> std::sync::MutexGuard<'_, BTreeMap<u64, Vec<u8>>> {
        self.pages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The `(offset, length)` of every range written, in offset order. A
    /// range written twice appears once.
    #[must_use]
    pub fn written_ranges(&self) -> Vec<(u64, u64)> {
        self.pages()
            .iter()
            .map(|(o, d)| (*o, d.len() as u64))
            .collect()
    }

    /// `len` bytes at `offset`, as a reader of the disk would see them:
    /// zeros where nothing was written.
    #[must_use]
    pub fn read(&self, offset: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let end = offset + len as u64;
        for (start, data) in self.pages().iter() {
            let stop = start + data.len() as u64;
            if stop <= offset || *start >= end {
                continue;
            }
            let from = offset.max(*start);
            let to = end.min(stop);
            let src = usize::try_from(from - start).unwrap_or_default();
            let dst = usize::try_from(from - offset).unwrap_or_default();
            let n = usize::try_from(to - from).unwrap_or_default();
            out[dst..dst + n].copy_from_slice(&data[src..src + n]);
        }
        out
    }

    /// Checks the blob against `expected`, the whole file uploaded in
    /// `chunk`-byte ranges: every non-zero chunk was written with exactly
    /// its bytes, and no all-zero chunk was written.
    ///
    /// # Errors
    /// A description of the first difference.
    pub fn check_nonzero_written(
        &self,
        expected: &[u8],
        chunk: usize,
    ) -> std::result::Result<(), String> {
        if expected.len() as u64 != self.size {
            return Err(format!(
                "expected {} bytes, blob is {}",
                expected.len(),
                self.size
            ));
        }
        let pages = self.pages();
        for (i, want) in expected.chunks(chunk).enumerate() {
            let offset = (i * chunk) as u64;
            let zero = want.iter().all(|b| *b == 0);
            match (pages.get(&offset), zero) {
                (Some(_), true) => return Err(format!("zero range at {offset} was written")),
                (None, false) => return Err(format!("non-zero range at {offset} was not written")),
                (Some(got), false) if got.as_slice() != want => {
                    return Err(format!("range at {offset} differs"));
                }
                _ => {}
            }
        }
        if let Some(stray) = pages.keys().find(|o| !o.is_multiple_of(chunk as u64)) {
            return Err(format!(
                "a write at {stray} is not on a {chunk}-byte boundary"
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl PageWriter for FakePageBlob {
    async fn put_page(&self, offset: u64, data: Vec<u8>) -> Result<()> {
        check_page_range(offset, data.len())?;
        if offset + data.len() as u64 > self.size {
            return Err(Error::InvalidRequest {
                code: CODE_INVALID_RANGE.to_string(),
                message: format!(
                    "write at {offset} of {} bytes is past the blob's {} bytes",
                    data.len(),
                    self.size
                ),
            });
        }
        let failing = {
            let mut left = self
                .fail_next
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let failing = *left > 0;
            *left = left.saturating_sub(1);
            failing
        };
        if failing {
            return Err(Error::Server {
                status: HTTP_SERVICE_UNAVAILABLE,
                code: CODE_SERVER_BUSY.to_string(),
                message: "The server is busy.".to_string(),
            });
        }
        self.pages().insert(offset, data);
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(test)]
#[path = "fake_blob_tests.rs"]
mod fake_blob_tests;
