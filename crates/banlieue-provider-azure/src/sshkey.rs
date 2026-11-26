// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The throwaway SSH public key a Linux VM create needs (ADR-0164).
//!
//! Azure refuses a Linux VM with password login off and no SSH key. banlieue
//! images never let the admin user log in, so the provider generates a fresh
//! Ed25519 key per create, sends the public half, and drops the private half
//! with the stack frame: nothing anywhere can use the key.
//!
//! The key is generated with `aws-lc-rs`, already in the build as rustls'
//! crypto provider (ADR-0170), and written in the OpenSSH `authorized_keys` form:
//! `ssh-ed25519 <base64 of the wire blob> <comment>`.

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair as _};
use base64::Engine as _;

use crate::error::{Error, Result};

/// The OpenSSH key type name of an Ed25519 key.
pub const SSH_ED25519: &str = "ssh-ed25519";
/// The comment on the key, so an admin reading the VM knows what it is.
pub const KEY_COMMENT: &str = "banlieue-unused";

/// Appends `bytes` as an SSH wire `string`: a big-endian `u32` length, then
/// the bytes.
fn put_string(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let len = u32::try_from(bytes.len()).map_err(|_| Error::Invalid {
        what: "ssh key",
        detail: "field longer than u32".to_string(),
    })?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

/// The `authorized_keys` line for a raw 32-byte Ed25519 public key.
///
/// # Errors
/// [`Error::Invalid`] only if a field cannot be length-prefixed.
pub fn authorized_key_line(public_key: &[u8]) -> Result<String> {
    let mut blob = Vec::new();
    put_string(&mut blob, SSH_ED25519.as_bytes())?;
    put_string(&mut blob, public_key)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&blob);
    Ok(format!("{SSH_ED25519} {encoded} {KEY_COMMENT}"))
}

/// A fresh Ed25519 public key in `authorized_keys` form. The private half
/// is never returned and never stored.
///
/// # Errors
/// [`Error::Invalid`] if the system random source fails.
pub fn throwaway_public_key() -> Result<String> {
    let pair = Ed25519KeyPair::generate().map_err(|_| Error::Invalid {
        what: "ssh key",
        detail: "the system random source failed".to_string(),
    })?;
    authorized_key_line(pair.public_key().as_ref())
}

#[cfg(test)]
#[path = "sshkey_tests.rs"]
mod sshkey_tests;
