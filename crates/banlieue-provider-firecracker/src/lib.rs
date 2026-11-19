// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-provider-firecracker
//!
//! banlieue's host-resident provider for Firecracker. For now this crate holds
//! only the guest attestation signer.
//!
//! - [`attest`]: the host's attestation CA and the signer that turns a guest's
//!   CSR into a short-lived `x509pop` certificate (ADR-0143 Decisions 1, 4, 5
//!   and 6; mediatore ADR-0049).

#![deny(unsafe_code)]

pub mod attest;
