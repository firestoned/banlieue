// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The Firecracker host's attestation signer (ADR-0143, mediatore ADR-0049).
//!
//! A guest with no vTPM attests with SPIRE's `x509pop` and a certificate its
//! host signs for the guest's own key. This module is the host side, pure
//! Rust with no subprocess and no FFI:
//!
//! - [`name`]: the alphabet a CA name and a subject may use.
//! - [`ca`]: the per-host CA, created once and loaded with its modes checked.
//! - [`csr`]: reading a guest's request down to its public key, nothing else.
//! - [`issue`]: the leaf certificate the signer, not the guest, describes.
//! - [`limiter`]: the per-guest rate limit, with an injectable clock.
//! - [`serve`]: one request on one connection, counting outcomes only.

mod ca;
mod csr;
mod issue;
mod limiter;
mod name;
mod serve;
#[cfg(test)]
mod test_support;

pub use ca::{Ca, CaError};
pub use csr::{CsrError, GuestPublicKey, MAX_CSR_BYTES, parse_csr};
pub use issue::{IssueError, MAX_VALIDITY, MIN_VALIDITY, Serial, issue};
pub use limiter::{Clock, DEFAULT_MAX_REQUESTS, DEFAULT_WINDOW, RateLimiter, SystemClock};
pub use name::{MAX_NAME_LEN, NameError, validate_name};
pub use serve::{CounterSnapshot, Counters, Outcome, READ_TIMEOUT, ServeError, serve_one};
