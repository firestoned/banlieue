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
        let read = guest_file_read_cmd(HANDLE);
        assert!(read.contains(r#""execute":"guest-file-read""#), "{read}");
        assert!(read.contains(r#""handle":7"#), "{read}");

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
}
