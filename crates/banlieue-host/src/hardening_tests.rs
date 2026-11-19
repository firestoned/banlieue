// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `hardening.rs` (ADR-0139 Decision 4).

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::render::{PROVIDER_UNIT, UNIT_TEMPLATES};

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    /// Every unit the installer places, as `(name, text)`.
    fn units() -> Vec<(&'static str, &'static str)> {
        UNIT_TEMPLATES
            .iter()
            .chain([&PROVIDER_UNIT])
            .map(|t| (t.name, t.text))
            .collect()
    }

    /// The directive keys of a unit file, comments ignored.
    fn keys(text: &str) -> Vec<&str> {
        text.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| l.split_once('='))
            .map(|(k, _)| k)
            .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric()))
            .collect()
    }

    // ------------------------------------------------- the named constants

    /// ADR-0139: today the only directive above 239 is `PrivateIPC=` (248).
    #[test]
    fn private_ipc_needs_systemd_248() {
        assert_eq!(PRIVATE_IPC.name, "PrivateIPC");
        assert_eq!(PRIVATE_IPC.min_systemd, 248);
        assert_eq!(VERSIONED_DIRECTIVES, [PRIVATE_IPC]);
    }

    /// A directive whose minimum is at or below the oldest supported systemd
    /// is not a gap and does not belong in the table.
    #[test]
    fn every_versioned_directive_is_newer_than_the_baseline() {
        for d in VERSIONED_DIRECTIVES {
            assert!(d.min_systemd > BASELINE_SYSTEMD, "{}", d.name);
        }
    }

    /// The table must match what the templates actually say: a directive
    /// listed here that no unit uses is dead, and one a unit uses that is in
    /// neither list is a new, unmeasured gap (ADR-0139: "the current set is
    /// fully measured").
    #[test]
    fn every_directive_in_every_unit_is_baseline_or_versioned() {
        for (name, text) in units() {
            for k in keys(text) {
                let known = BASELINE_DIRECTIVES.contains(&k)
                    || VERSIONED_DIRECTIVES.iter().any(|d| d.name == k);
                assert!(
                    known,
                    "{name}: {k}= has no minimum systemd version recorded in hardening.rs"
                );
            }
        }
    }

    #[test]
    fn every_versioned_directive_is_used_by_the_guest_templates() {
        for d in VERSIONED_DIRECTIVES {
            for (name, text) in units().into_iter().filter(|(n, _)| n.contains('@')) {
                assert!(keys(text).contains(&d.name), "{name} lacks {}", d.name);
            }
        }
    }

    #[test]
    fn the_baseline_names_no_versioned_directive() {
        for d in VERSIONED_DIRECTIVES {
            assert!(!BASELINE_DIRECTIVES.contains(&d.name), "{}", d.name);
        }
    }

    // ------------------------------------------------------------ unenforced

    #[test]
    fn systemd_239_lacks_private_ipc() {
        assert_eq!(unenforced(239), vec!["PrivateIPC"]);
        assert_eq!(unenforced(247), vec!["PrivateIPC"]);
    }

    #[test]
    fn systemd_248_and_newer_enforce_everything() {
        assert!(unenforced(248).is_empty());
        assert!(unenforced(252).is_empty());
        assert!(unenforced(u32::MAX).is_empty());
    }

    // --------------------------------------------------------- version text

    #[test]
    fn the_version_is_read_from_systemctl_version() {
        let el8 = "systemd 239 (239-82.el8)\n+PAM +AUDIT +SELINUX\n";
        assert_eq!(parse_systemd_version(el8), Some(239));
        assert_eq!(parse_systemd_version("systemd 252 (252.22-1)\n"), Some(252));
        assert_eq!(
            parse_systemd_version("systemd 256~rc3 (256~rc3-1)\n"),
            Some(256)
        );
    }

    #[test]
    fn nonsense_has_no_version() {
        for text in ["", "\n", "systemd\n", "systemd abc\n", "upstart 1.13\n"] {
            assert_eq!(parse_systemd_version(text), None, "{text:?}");
        }
    }

    // -------------------------------------------------------------- problems

    #[test]
    fn nothing_is_asked_of_a_host_with_nothing_missing() {
        assert!(problems(Some(252), &[]).is_empty());
    }

    /// The acceptance is a recorded statement of what the host lacks, so it
    /// is never implied and never silent.
    #[test]
    fn an_old_host_is_refused_without_the_acceptance() {
        let p = problems(Some(239), &[]);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("PrivateIPC"), "{p:?}");
        assert!(
            p[0].contains("--accept-reduced-hardening=PrivateIPC"),
            "{p:?}"
        );
    }

    #[test]
    fn the_exact_list_is_accepted() {
        assert!(problems(Some(239), &strings(&["PrivateIPC"])).is_empty());
    }

    /// A list that names more than the host lacks is not "exactly the same
    /// set": it would record a weakness the host does not have.
    #[test]
    fn an_acceptance_for_what_the_host_enforces_is_refused() {
        let p = problems(Some(252), &strings(&["PrivateIPC"]));
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("enforces"), "{p:?}");
    }

    #[test]
    fn an_unknown_systemd_version_is_a_problem_not_a_pass() {
        let p = problems(None, &[]);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("systemd version"), "{p:?}");
    }

    // --------------------------------------------------------- the accepted

    #[test]
    fn accepted_names_are_trimmed_sorted_and_deduplicated() {
        let got = accepted(&strings(&[" PrivateIPC ", "PrivateIPC", ""])).unwrap();
        assert_eq!(got, strings(&["PrivateIPC"]));
        assert!(accepted(&[]).unwrap().is_empty());
    }

    #[test]
    fn an_unknown_directive_cannot_be_accepted() {
        let e = accepted(&strings(&["PrivateIPC", "ProtectSystem"])).unwrap_err();
        assert!(e.contains("ProtectSystem"), "{e}");
    }

    // ------------------------------------------------------------- dropping

    #[test]
    fn dropping_a_directive_removes_only_its_lines() {
        let text = "[Service]\nPrivateIPC=yes\n# PrivateIPC=yes in a comment\nPrivateTmp=yes\n";
        let out = drop_directives(text, &strings(&["PrivateIPC"]));
        assert_eq!(
            out,
            "[Service]\n# PrivateIPC=yes in a comment\nPrivateTmp=yes\n"
        );
    }

    #[test]
    fn dropping_nothing_changes_nothing() {
        for (_, text) in units() {
            assert_eq!(drop_directives(text, &[]), text);
        }
    }

    #[test]
    fn a_dropped_directive_leaves_the_real_templates_valid() {
        for (name, text) in units() {
            let out = drop_directives(text, &strings(&["PrivateIPC"]));
            assert!(!keys(&out).contains(&"PrivateIPC"), "{name}");
            assert_eq!(
                out.lines().count() + keys(text).iter().filter(|k| **k == "PrivateIPC").count(),
                text.lines().count(),
                "{name}"
            );
        }
    }
}
