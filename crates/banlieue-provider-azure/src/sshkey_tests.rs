// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `sshkey.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    const ED25519_PUBLIC_KEY_LEN: usize = 32;
    /// `u32` length + "ssh-ed25519" + `u32` length + 32 key bytes.
    const ED25519_BLOB_LEN: usize = 4 + 11 + 4 + 32;

    fn blob(line: &str) -> Vec<u8> {
        let mut parts = line.split(' ');
        assert_eq!(parts.next(), Some(SSH_ED25519));
        let encoded = parts.next().expect("base64 field");
        assert_eq!(parts.next(), Some(KEY_COMMENT));
        assert_eq!(parts.next(), None);
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("valid base64")
    }

    /// The well-known shape: every OpenSSH Ed25519 key starts `AAAAC3NzaC1lZDI1NTE5`.
    #[test]
    fn a_key_line_is_in_openssh_authorized_keys_form() {
        let line = authorized_key_line(&[7u8; ED25519_PUBLIC_KEY_LEN]).expect("line");
        assert!(
            line.starts_with("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5"),
            "{line}"
        );
        let b = blob(&line);
        assert_eq!(b.len(), ED25519_BLOB_LEN);
        assert_eq!(&b[..4], &11u32.to_be_bytes());
        assert_eq!(&b[4..15], b"ssh-ed25519");
        assert_eq!(&b[15..19], &32u32.to_be_bytes());
        assert_eq!(&b[19..], &[7u8; ED25519_PUBLIC_KEY_LEN]);
    }

    #[test]
    fn a_throwaway_key_is_a_valid_ed25519_line() {
        let line = throwaway_public_key().expect("key");
        assert_eq!(blob(&line).len(), ED25519_BLOB_LEN);
    }

    /// Fresh per create: two keys never match, so no key is reused.
    #[test]
    fn every_throwaway_key_is_different() {
        let a = throwaway_public_key().expect("key");
        let b = throwaway_public_key().expect("key");
        assert_ne!(a, b);
    }
}
