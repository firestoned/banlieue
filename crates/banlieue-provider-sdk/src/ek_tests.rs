// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `ek.rs`. A real swtpm certificate is checked in
//! `banlieue-provider-libvirt`'s `guest_tests.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn the_expected_cn_is_name_colon_uuid() {
        assert_eq!(expected_ek_cn("m", "u-1"), "m:u-1");
    }

    #[test]
    fn anything_but_a_certificate_never_matches() {
        assert!(!ek_cn_matches("", "m", "u"));
        assert!(!ek_cn_matches(
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
            "m",
            "u"
        ));
    }
}
