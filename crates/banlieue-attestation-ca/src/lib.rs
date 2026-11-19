// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The per-host attestation CA and the guest certificate signer
//! (ADR-0143 Decisions 1, 3 to 6; ADR-0146 Decision 3).
//!
//! A library with no Kubernetes dependency, pure Rust (no FFI, no
//! subprocess). It knows nothing of machines, classes or clusters: the caller
//! supplies the CA directory, the subject to sign for and the request socket.
//!
//! The wire protocol is mediatore ADR-0049's: a guest sends one PEM CSR (at
//! most [`MAX_CSR_BYTES`]) and half-closes; the host answers one PEM
//! certificate and closes. A refusal gets no reply.
//!
//! Nothing here logs, returns in an error, or prints (`Debug`/`Display`) a CSR
//! or certificate body.

mod ca;
mod name;
mod ratelimit;
mod serve;
mod signer;

pub use ca::{AttestationCa, CaError};
pub use name::{MAX_NAME_LEN, NameError, validate_name};
pub use ratelimit::RateLimit;
pub use serve::{OutcomeCounts, Outcomes, ServeConfig, serve};
pub use signer::{
    MAX_CERTIFICATE_TTL, MAX_CSR_BYTES, MIN_CERTIFICATE_TTL, SignError, Signer, SignerConfigError,
};

#[cfg(test)]
mod openssl_tests;
#[cfg(test)]
mod testutil;
