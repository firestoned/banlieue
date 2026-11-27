// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `console.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    const ID: &str = "i-0abcdef1234567890";

    /// A serial console as EC2 returns it: kernel lines with kmsg
    /// timestamps, CRLF endings, and systemd's own lines around ours.
    fn console(ready_line: &str) -> String {
        [
            "[    0.000000] Linux version 6.6.58 (builder@localhost) #1 SMP",
            "[    0.000000] Command line: BOOT_IMAGE=/vmlinuz console=ttyS0 quiet",
            "[    3.214567] systemd[1]: Starting cos-setup-network.service...",
            "[   11.902311] banlieue-ec2-guest: userdata applied",
            ready_line,
            "[   12.501002] systemd[1]: Reached target multi-user.target.",
            "",
        ]
        .join("\r\n")
    }

    #[test]
    fn the_line_for_this_instance_is_ready() {
        let text = console(&format!(
            "[   12.345678] banlieue-ec2-guest: GuestReady instance={ID}"
        ));
        assert_eq!(ready_state(&text, ID), ReadyState::Ready);
    }

    #[test]
    fn a_line_without_a_timestamp_or_with_lf_only_is_ready() {
        let text = format!("boot\nbanlieue-ec2-guest: GuestReady instance={ID}\nmore\n");
        assert_eq!(ready_state(&text, ID), ReadyState::Ready);
    }

    #[test]
    fn no_line_is_not_yet() {
        let text = console("[   12.345678] systemd[1]: Started sshd.service.");
        assert_eq!(ready_state(&text, ID), ReadyState::NotYet);
        assert_eq!(ready_state("", ID), ReadyState::NotYet);
    }

    #[test]
    fn a_similar_but_unrelated_line_is_not_ready() {
        let text =
            console("[   12.345678] banlieue-ec2-guest: userdata instance=i-0abcdef1234567890");
        assert_eq!(ready_state(&text, ID), ReadyState::NotYet);
    }

    #[test]
    fn a_line_for_another_instance_is_reported_never_ready() {
        let text =
            console("[   12.345678] banlieue-ec2-guest: GuestReady instance=i-0fedcba9876543210");
        assert_eq!(
            ready_state(&text, ID),
            ReadyState::OtherInstance("i-0fedcba9876543210".to_string())
        );
    }

    #[test]
    fn a_prefix_of_the_id_is_not_a_match() {
        let shorter = &ID[..ID.len() - 1];
        let text = console(&format!(
            "[   12.345678] banlieue-ec2-guest: GuestReady instance={shorter}"
        ));
        assert_eq!(
            ready_state(&text, ID),
            ReadyState::OtherInstance(shorter.to_string())
        );
        let longer = format!("{ID}0");
        let text = console(&format!(
            "[   12.345678] banlieue-ec2-guest: GuestReady instance={longer}"
        ));
        assert_eq!(ready_state(&text, ID), ReadyState::OtherInstance(longer));
    }

    #[test]
    fn a_truncated_final_line_is_not_read() {
        // The console snapshot can end mid-line: `i-0abcdef12` is the start of
        // an ID, not another instance's.
        let text = format!(
            "[   11.0] boot\r\n[   12.345678] banlieue-ec2-guest: GuestReady instance={}",
            &ID[..ID.len() - 8]
        );
        assert_eq!(ready_state(&text, ID), ReadyState::NotYet);
    }

    #[test]
    fn this_instance_wins_over_another_line() {
        let text = format!(
            "banlieue-ec2-guest: GuestReady instance=i-0other\r\n\
             [   12.3] banlieue-ec2-guest: GuestReady instance={ID}\r\n"
        );
        assert_eq!(ready_state(&text, ID), ReadyState::Ready);
    }

    #[test]
    fn trailing_whitespace_after_the_id_is_ignored() {
        let text = format!("banlieue-ec2-guest: GuestReady instance={ID}  \r\n");
        assert_eq!(ready_state(&text, ID), ReadyState::Ready);
    }

    #[test]
    fn an_empty_id_is_not_a_report() {
        let text = "banlieue-ec2-guest: GuestReady instance=\r\n";
        assert_eq!(ready_state(text, ID), ReadyState::NotYet);
    }

    #[test]
    fn an_empty_expected_id_is_never_ready() {
        let text = format!("banlieue-ec2-guest: GuestReady instance={ID}\n");
        assert_eq!(
            ready_state(&text, ""),
            ReadyState::OtherInstance(ID.to_string())
        );
    }
}
