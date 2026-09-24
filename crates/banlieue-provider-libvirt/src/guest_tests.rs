// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `guest.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    /// base64 of "installed\n" — what a shell `echo installed > marker`
    /// produces, which is what every real image will actually write.
    const INSTALLED_B64: &str = "aW5zdGFsbGVkCg==";
    /// base64 of "installing\n".
    const INSTALLING_B64: &str = "aW5zdGFsbGluZwo=";

    fn read_reply(b64: &str, eof: bool) -> String {
        format!(r#"{{"return":{{"count":10,"buf-b64":"{b64}","eof":{eof}}}}}"#)
    }

    // ------------------------------------------------------------------
    // Command construction
    // ------------------------------------------------------------------

    /// Read-only, explicitly. A reconcile loop must never be able to modify
    /// a guest it is only inspecting, and `guest-file-open` defaults are
    /// not something to rely on.
    #[test]
    fn the_marker_is_opened_read_only() {
        let cmd = guest_file_open_cmd(MARKER_PATH);
        assert!(cmd.contains(r#""execute":"guest-file-open""#), "{cmd}");
        assert!(cmd.contains(MARKER_PATH), "{cmd}");
        assert!(cmd.contains(r#""mode":"r""#), "{cmd}");
    }

    #[test]
    fn read_and_close_carry_the_handle() {
        const HANDLE: i64 = 7;
        let read = guest_file_read_cmd(HANDLE, MARKER_READ_MAX);
        assert!(read.contains(r#""execute":"guest-file-read""#), "{read}");
        assert!(read.contains(r#""handle":7"#), "{read}");
        assert!(
            read.contains(&format!(r#""count":{MARKER_READ_MAX}"#)),
            "{read}"
        );

        let close = guest_file_close_cmd(HANDLE);
        assert!(close.contains(r#""execute":"guest-file-close""#), "{close}");
        assert!(close.contains(r#""handle":7"#), "{close}");
    }

    // ------------------------------------------------------------------
    // guest-file-open
    // ------------------------------------------------------------------

    #[test]
    fn a_successful_open_yields_its_handle() {
        assert_eq!(parse_file_handle(r#"{"return":7}"#), Some(7));
    }

    /// The marker is absent for the whole install, which is the *expected*
    /// state for most of a Deferred member's life — not an error.
    #[test]
    fn a_missing_marker_yields_no_handle() {
        let reply = r#"{"error":{"class":"GenericError","desc":"Failed to open file"}}"#;
        assert_eq!(parse_file_handle(reply), None);
    }

    #[test]
    fn garbage_yields_no_handle() {
        for junk in ["", "not json", "{}", r#"{"return":"seven"}"#] {
            assert_eq!(parse_file_handle(junk), None, "{junk:?}");
        }
    }

    // ------------------------------------------------------------------
    // guest-file-read → phase
    // ------------------------------------------------------------------

    #[test]
    fn a_marker_reading_installed_means_installed() {
        let reply = read_reply(INSTALLED_B64, true);
        assert_eq!(guest_phase_from_read(&reply).as_deref(), Some("installed"));
        assert!(guest_is_installed(&reply));
    }

    /// The entire point of ADR-0043: a guest that is still installing must
    /// not read as installed. Every cheap liveness signal fails this test.
    #[test]
    fn a_guest_still_installing_is_not_installed() {
        let reply = read_reply(INSTALLING_B64, true);
        assert_eq!(guest_phase_from_read(&reply).as_deref(), Some("installing"));
        assert!(!guest_is_installed(&reply));
    }

    /// A shell `echo` appends a newline and an operator may indent. A
    /// matcher that only worked on the exact bytes would pass here and fail
    /// against every real image.
    #[test]
    fn surrounding_whitespace_is_tolerated() {
        // "  installed \n"
        let reply = read_reply("ICBpbnN0YWxsZWQgCg==", true);
        assert!(guest_is_installed(&reply));
    }

    /// Matching is exact after trimming: a marker that merely *contains*
    /// the word must not count, or "not-installed" would read as installed.
    #[test]
    fn a_substring_match_is_not_enough() {
        // "not-installed\n"
        let reply = read_reply("bm90LWluc3RhbGxlZAo=", true);
        assert_eq!(
            guest_phase_from_read(&reply).as_deref(),
            Some("not-installed")
        );
        assert!(!guest_is_installed(&reply));
    }

    #[test]
    fn an_agent_error_is_not_installed_rather_than_a_failure() {
        let reply = r#"{"error":{"class":"GenericError","desc":"handle not found"}}"#;
        assert_eq!(guest_phase_from_read(reply), None);
        assert!(!guest_is_installed(reply));
    }

    #[test]
    fn garbage_from_the_agent_is_not_installed() {
        for junk in ["", "not json", "{}", r#"{"return":{}}"#] {
            assert!(
                !guest_is_installed(junk),
                "{junk:?} must not read installed"
            );
        }
    }

    /// Invalid base64 must not panic: the payload crosses a trust boundary
    /// from inside the guest, and a guest that can crash the provider's
    /// reconcile loop is a denial of service.
    #[test]
    fn undecodable_base64_is_not_installed() {
        let reply = read_reply("!!!!not-base64!!!!", true);
        assert!(!guest_is_installed(&reply));
        assert_eq!(guest_phase_from_read(&reply), None);
    }

    /// A guest could return megabytes. The marker is one short word, so
    /// anything larger is either a wrong file or an attempt to make the
    /// provider allocate — cap it rather than trust the guest.
    #[test]
    fn an_oversized_marker_is_rejected() {
        use base64::Engine as _;
        let huge = "x".repeat(MARKER_READ_MAX * 2);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&huge);
        let reply = read_reply(&b64, true);
        assert_eq!(guest_phase_from_read(&reply), None);
        assert!(!guest_is_installed(&reply));
    }

    // ==================================================================
    // ADR-0045 — the vTPM endorsement key certificate
    // ==================================================================

    /// A real `swtpm_localca`-issued EK certificate, read out of NV index
    /// `0x01c00002` of a live libvirt domain on 2026-09-23. Kept verbatim
    /// because the two things this code depends on are properties of the
    /// real issuer, not of a certificate we could mint ourselves: the
    /// subject CN is `<domain-name>:<domain-uuid>` (libvirt's `--vmid`),
    /// and the issuer is the host's local CA.
    const SWTPM_EK_PEM: &str = concat!(
        "-----BEGIN CERTIFICATE-----\n",
        "MIIEIzCCAougAwIBAgIBejANBgkqhkiG9w0BAQsFADAYMRYwFAYDVQQDEw1zd3Rw\n",
        "bS1sb2NhbGNhMCAXDTI2MDkyMzIwNDYxM1oYDzk5OTkxMjMxMjM1OTU5WjBBMT8w\n",
        "PQYDVQQDEzZiYW5saWV1ZS1lay1wcm9iZTo3YTk4NTZkNS1lNTE1LTQ5ZDUtODI2\n",
        "Zi05YTI2MWE3OGZiYzYwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQCf\n",
        "gLNYGXZZsnQEos7jSNOpr6sQxV7GJyBcdlwc1ddhp4zrg8aLe07WbIToDZnOtlmD\n",
        "++XwplOL3zwdnrzU7DwrrEKYqwDY8p58pWP20c+6WnsxXw+iwUaT8NDm2bRpnSOn\n",
        "0Ov8jHSs1LfZ0Y+nkRUR9JZcGALSZgFZSR9d3ZLebmYO39bQ6OjAgtJc4X11VMH7\n",
        "7kE5NFqTRm5qAOEVX5ONQKKIROkx6tgx+ro8VzArkgUn7G4C8DtoR/loEO3YSjK+\n",
        "wusOJ/qxS5Xy/VKmCTa64aYRVl29lhdXzm+ixQ/cAX9+sAmFYdUUUSS8U+RqHv7N\n",
        "i0drmW0qVa8F57wNf4FZAgMBAAGjgcwwgckwEAYDVR0lBAkwBwYFZ4EFCAEwUgYD\n",
        "VR0RAQH/BEgwRqREMEIxFjAUBgVngQUCAQwLaWQ6MDAwMDEwMTQxEDAOBgVngQUC\n",
        "AgwFc3d0cG0xFjAUBgVngQUCAwwLaWQ6MjAxOTEwMjMwDAYDVR0TAQH/BAIwADAi\n",
        "BgNVHQkEGzAZMBcGBWeBBQIQMQ4wDAwDMi4wAgEAAgIApDAfBgNVHSMEGDAWgBQO\n",
        "YCq1aXCJDugkKlbr9yBJBYdFKTAOBgNVHQ8BAf8EBAMCBSAwDQYJKoZIhvcNAQEL\n",
        "BQADggGBACvb8XeJjThq/yXH6pCVtiqVDb7i2+m39dIs4be0t40HYJDlbpd9XPeH\n",
        "suih5MFxzq6ySFAxpxdGG4OIke22FOHLidrJosr4XVD6I+0NbAZlS96yGOJbJWE0\n",
        "CjDHmXIBTpVuwGOc1TSc+PqyMKZoYglMv2nvFfnA3XjMlQDl/Yoe35Cl/fWN7bFy\n",
        "+ilSMkI3ElR82CC5nukuABX2qOqaiGurNs2u1zr8a2RbxjK3Yg+2Dr6urjdSsVMt\n",
        "5akTHhHzjUSnj0EzZ6AQLclMzI9mzswRGFsKsWTE88XG+F9jgh8/kVMbWwdmzwBe\n",
        "Q/nMmFNFPy62W5+E408VWzcM0qpPV+JAFKA0NydT+60Sa+SIDjNjLN690xq00yJb\n",
        "wi9nvQ1yezvB+ajm5Lzf4RGDRxphYCsGBYMEgyNyaPPf9Z/aEleX6a1ZUwO/OLhu\n",
        "Le+LL/CtTmtGn50h/Jsp/nkaaQDq5ruwEYVZZe52k2YelD722ktCIa924zdlYyWn\n",
        "jqEhDVZYlw==\n",
        "-----END CERTIFICATE-----\n",
    );

    /// The domain the fixture above was issued to.
    const FIXTURE_DOMAIN: &str = "banlieue-ek-probe";
    const FIXTURE_UUID: &str = "7a9856d5-e515-49d5-826f-9a261a78fbc6";

    fn ek_reply(pem: &str) -> String {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(pem);
        format!(
            r#"{{"return":{{"count":{},"buf-b64":"{b64}","eof":true}}}}"#,
            pem.len()
        )
    }

    /// The certificate lives beside the phase marker, on the same tmpfs, for
    /// the same reason (ADR-0043 Decision 3 / ADR-0045 Decision 2): a
    /// stale assertion must not survive a power cycle.
    #[test]
    fn the_ek_certificate_lives_beside_the_phase_marker() {
        assert!(EK_PATH.starts_with("/run/"), "{EK_PATH}");
        let marker_dir = MARKER_PATH.rsplit_once('/').expect("marker has a dir").0;
        assert!(
            EK_PATH.starts_with(marker_dir),
            "{EK_PATH} vs {MARKER_PATH}"
        );
    }

    /// banlieue reads the certificate; it must never be able to write it.
    /// The whole reason ADR-0045 rejects `guest-exec` is to keep this true.
    #[test]
    fn the_ek_certificate_is_opened_read_only() {
        let cmd = guest_file_open_cmd(EK_PATH);
        assert!(cmd.contains(r#""mode":"r""#), "{cmd}");
    }

    #[test]
    fn a_real_swtpm_certificate_parses() {
        let got = parse_ek_pem(&ek_reply(SWTPM_EK_PEM)).expect("fixture must parse");
        assert!(got.starts_with("-----BEGIN CERTIFICATE-----"), "{got}");
        assert!(
            got.trim_end().ends_with("-----END CERTIFICATE-----"),
            "{got}"
        );
    }

    /// libvirt passes `--vmid <domain-name>:<domain-uuid>` and swtpm_localca
    /// puts it in the subject CN. Both halves are values banlieue itself
    /// assigned when it defined the domain.
    #[test]
    fn the_expected_cn_is_name_colon_uuid() {
        assert_eq!(
            expected_ek_cn(FIXTURE_DOMAIN, FIXTURE_UUID),
            format!("{FIXTURE_DOMAIN}:{FIXTURE_UUID}")
        );
    }

    #[test]
    fn a_certificate_issued_to_this_domain_matches() {
        assert!(ek_cn_matches(SWTPM_EK_PEM, FIXTURE_DOMAIN, FIXTURE_UUID));
    }

    /// The substitution this check exists to catch: a guest reporting some
    /// *other* member's EK certificate. ADR-0049's activation would catch it
    /// cryptographically; this catches it one step earlier and for free.
    #[test]
    fn another_members_certificate_does_not_match() {
        assert!(!ek_cn_matches(
            SWTPM_EK_PEM,
            FIXTURE_DOMAIN,
            "00000000-0000-0000-0000-000000000000"
        ));
        assert!(!ek_cn_matches(
            SWTPM_EK_PEM,
            "some-other-domain",
            FIXTURE_UUID
        ));
    }

    /// A guest that reports something that is not a certificate must not get
    /// it published into a status field consumers treat as one.
    #[test]
    fn a_non_certificate_is_not_published() {
        let junk = [
            "",
            "hello",
            "-----BEGIN CERTIFICATE-----\nnot base64 at all\n-----END CERTIFICATE-----\n",
            "-----BEGIN PRIVATE KEY-----\nMIIB\n-----END PRIVATE KEY-----\n",
        ];
        for j in junk {
            assert_eq!(parse_ek_pem(&ek_reply(j)), None, "{j:?} must not parse");
            assert!(!ek_cn_matches(j, FIXTURE_DOMAIN, FIXTURE_UUID), "{j:?}");
        }
    }

    #[test]
    fn agent_garbage_yields_no_certificate() {
        for junk in ["", "not json", "{}", r#"{"return":{}}"#] {
            assert_eq!(parse_ek_pem(junk), None, "{junk:?}");
        }
    }

    /// Same reasoning as the marker cap: the payload crosses a boundary from
    /// inside a VM that may already be somebody else's.
    #[test]
    fn an_oversized_certificate_is_rejected() {
        use base64::Engine as _;
        let huge = "x".repeat(EK_READ_MAX * 2);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&huge);
        let reply = format!(r#"{{"return":{{"count":1,"buf-b64":"{b64}","eof":true}}}}"#);
        assert_eq!(parse_ek_pem(&reply), None);
    }

    /// A certificate is bigger than a one-word marker, so the read must ask
    /// for enough of it — a cap sized for the marker would truncate every
    /// certificate and silently publish nothing.
    #[test]
    fn the_certificate_cap_is_large_enough_for_a_real_certificate() {
        assert!(
            EK_READ_MAX > SWTPM_EK_PEM.len(),
            "cap {EK_READ_MAX} must exceed a real certificate ({})",
            SWTPM_EK_PEM.len()
        );
    }

    /// A guest may wrap anything around a valid certificate. Only the
    /// certificate is published — nothing the guest bolted on reaches a
    /// status field consumers feed to a certificate library.
    ///
    /// Both directions, because the first fix here only handled one. Slicing
    /// the input by what the parser did not consume dropped trailing bytes
    /// but kept LEADING ones: `x509_parser` skips lines that do not start a
    /// PEM block and counts them in its position, so the slice still carried
    /// them and `ek_cn_matches` still passed. Publication now re-encodes from
    /// the parsed DER, so no input layout can smuggle bytes through.
    #[test]
    fn bytes_wrapped_around_a_certificate_are_not_published() {
        let cases = [
            (
                "trailing block",
                format!("{SWTPM_EK_PEM}-----BEGIN EVIL-----\ngotcha\n-----END EVIL-----\n"),
            ),
            ("trailing junk", format!("{SWTPM_EK_PEM}gotcha\n")),
            (
                "leading junk",
                format!("EVIL-PREFIX: gotcha\nsecond line\n{SWTPM_EK_PEM}"),
            ),
            (
                "both",
                format!("EVIL-PREFIX: gotcha\n{SWTPM_EK_PEM}trailing gotcha\n"),
            ),
        ];
        for (what, raw) in cases {
            let got = parse_ek_pem(&ek_reply(&raw))
                .unwrap_or_else(|| panic!("{what}: the certificate should still parse"));
            assert!(
                got.starts_with("-----BEGIN CERTIFICATE-----"),
                "{what}: leading bytes leaked: {got}"
            );
            assert!(
                got.trim_end().ends_with("-----END CERTIFICATE-----"),
                "{what}: trailing bytes leaked: {got}"
            );
            assert!(!got.contains("gotcha"), "{what}: junk leaked: {got}");
            assert!(!got.contains("EVIL"), "{what}: junk leaked: {got}");
            // And it is still the certificate it claims to be.
            assert!(ek_cn_matches(&got, FIXTURE_DOMAIN, FIXTURE_UUID), "{what}");
        }
    }
}
