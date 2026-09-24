// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `pem.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    /// The framing has to be something a certificate library accepts, which
    /// means RFC 7468 markers and 64-character lines — not base64 with a
    /// header glued on.
    #[test]
    fn der_becomes_well_formed_pem() {
        let der: Vec<u8> = (0..200u32).map(|i| (i % 251) as u8).collect();
        let pem = der_to_pem(&der);

        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"), "{pem}");
        assert!(pem.ends_with("-----END CERTIFICATE-----\n"), "{pem}");

        let body: Vec<&str> = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        assert!(body.len() > 1, "a 200-byte certificate must wrap");
        for line in &body[..body.len() - 1] {
            assert_eq!(line.len(), 64, "every full line is 64 chars: {line:?}");
        }

        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(body.concat())
            .expect("body is valid base64");
        assert_eq!(decoded, der, "must round-trip to the bytes we were given");
    }

    /// A payload that is an exact multiple of the line width must not emit a
    /// trailing blank line — some parsers reject it.
    #[test]
    fn an_exact_multiple_of_the_line_width_does_not_emit_a_blank_line() {
        let der = vec![7u8; 48]; // 48 bytes -> exactly 64 base64 chars
        let pem = der_to_pem(&der);
        assert!(!pem.contains("\n\n"), "{pem}");
        let body: Vec<&str> = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        assert_eq!(body.len(), 1, "{pem}");
        assert_eq!(body[0].len(), 64);
    }
}
