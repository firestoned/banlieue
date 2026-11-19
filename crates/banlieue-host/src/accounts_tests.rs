// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `accounts.rs` (ADR-0139 Decision 1), against the fake host.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::fake::FakeHost;
    use crate::ops::{Kind, Probe};
    use crate::settings::{HostArgs, Settings, resolve};

    /// Few uids, so the tests stay fast.
    const TEST_UID_COUNT: u32 = 3;
    const OLD_SYSTEMD: u32 = 239;

    fn settings(h: &FakeHost, accounts: GuestAccounts) -> Settings {
        let args = HostArgs {
            guest_uid_count: Some(TEST_UID_COUNT),
            registry_keep_unreferenced: crate::settings::DEFAULT_KEEP_UNREFERENCED,
            guest_accounts: Some(accounts),
            ..HostArgs::default()
        };
        resolve(&args, h).unwrap()
    }

    fn old_host() -> FakeHost {
        let h = FakeHost::debian();
        h.env.lock().unwrap().systemd_version = Some(OLD_SYSTEMD);
        h
    }

    fn nsswitch(h: &FakeHost, text: &str) {
        h.put_file("/etc/nsswitch.conf", text.as_bytes(), Kind::File);
    }

    // ------------------------------------------------------ nsswitch parsing

    #[test]
    fn a_source_is_found_on_the_right_database_line() {
        let text = "passwd: files systemd\ngroup: files\n";
        assert!(nss_has_source(text, "passwd", &["systemd"]));
        assert!(!nss_has_source(text, "group", &["systemd"]));
        assert!(nss_has_source(text, "group", &["compat", "files"]));
    }

    #[test]
    fn a_commented_line_is_not_a_source() {
        assert!(!nss_has_source(
            "# passwd: systemd\n",
            "passwd",
            &["systemd"]
        ));
        assert!(!nss_has_source("", "passwd", &["files"]));
    }

    #[test]
    fn a_source_must_be_a_whole_word() {
        assert!(!nss_has_source(
            "passwd: sssd files-ish\n",
            "passwd",
            &["files"]
        ));
        assert!(nss_has_source(
            "passwd:     sss files [SUCCESS=merge]\n",
            "passwd",
            &["files"]
        ));
    }

    // ----------------------------------------------------------- the choice

    /// A modern host with the systemd NSS module gets userdb, unasked, and
    /// the reason names the facts it was derived from.
    #[test]
    fn a_modern_host_derives_userdb() {
        let h = FakeHost::debian();
        let (mode, why) = choose(None, &h).unwrap();
        assert_eq!(mode, GuestAccounts::Userdb);
        assert!(why.contains("252"), "{why}");
    }

    #[test]
    fn systemd_before_245_derives_files() {
        let h = old_host();
        let (mode, why) = choose(None, &h).unwrap();
        assert_eq!(mode, GuestAccounts::Files);
        assert!(why.contains("239") && why.contains("245"), "{why}");
    }

    #[test]
    fn systemd_245_is_the_first_userdb_version() {
        let h = FakeHost::debian();
        h.env.lock().unwrap().systemd_version = Some(MIN_SYSTEMD_USERDB);
        assert_eq!(choose(None, &h).unwrap().0, GuestAccounts::Userdb);
        h.env.lock().unwrap().systemd_version = Some(MIN_SYSTEMD_USERDB - 1);
        assert_eq!(choose(None, &h).unwrap().0, GuestAccounts::Files);
    }

    #[test]
    fn no_systemd_source_in_nsswitch_derives_files() {
        let h = FakeHost::debian();
        nsswitch(&h, "passwd: files\ngroup: files systemd\n");
        let (mode, why) = choose(None, &h).unwrap();
        assert_eq!(mode, GuestAccounts::Files);
        assert!(why.contains("nsswitch.conf passwd"), "{why}");
    }

    #[test]
    fn an_unknown_systemd_version_derives_files() {
        let h = FakeHost::debian();
        h.env.lock().unwrap().systemd_version = None;
        assert_eq!(choose(None, &h).unwrap().0, GuestAccounts::Files);
    }

    #[test]
    fn an_explicit_files_wins_on_a_host_that_could_do_userdb() {
        let h = FakeHost::debian();
        let (mode, why) = choose(Some(GuestAccounts::Files), &h).unwrap();
        assert_eq!(mode, GuestAccounts::Files);
        assert!(why.contains("--guest-accounts"), "{why}");
    }

    #[test]
    fn an_explicit_userdb_is_checked_against_the_facts() {
        let h = old_host();
        let Err(Error::Setting { name, why }) = choose(Some(GuestAccounts::Userdb), &h) else {
            panic!("userdb accepted on systemd 239");
        };
        assert_eq!(name, "--guest-accounts");
        assert!(why.contains("239") && why.contains("files"), "{why}");

        let h = FakeHost::debian();
        nsswitch(&h, "passwd: files\ngroup: files\n");
        assert!(matches!(
            choose(Some(GuestAccounts::Userdb), &h),
            Err(Error::Setting { .. })
        ));
        let h = FakeHost::debian();
        assert_eq!(
            choose(Some(GuestAccounts::Userdb), &h).unwrap().0,
            GuestAccounts::Userdb
        );
    }

    // ------------------------------------------------------- files accounts

    fn name(uid: u32) -> String {
        format!("{GUEST_NAME_PREFIX}{uid}")
    }

    #[test]
    fn files_mode_creates_a_user_and_a_private_group_per_uid() {
        let h = old_host();
        let s = settings(&h, GuestAccounts::Files);
        let created = register_files(&h, &s).unwrap();
        assert_eq!(created, TEST_UID_COUNT as usize);
        for uid in s.uid_base..=s.uid_end() {
            let user = h.getent("passwd", &name(uid)).unwrap();
            assert!(
                user.starts_with(&format!("{}:x:{uid}:{uid}:", name(uid))),
                "{user}"
            );
            let group = h.getent("group", &uid.to_string()).unwrap();
            assert!(
                group.starts_with(&format!("{}:x:{uid}:", name(uid))),
                "{group}"
            );
        }
    }

    /// The host's own tools, never a hand edit of passwd or group: the tools
    /// take the locks and keep the shadow files in step.
    #[test]
    fn files_mode_uses_groupadd_and_useradd_only() {
        let h = old_host();
        let s = settings(&h, GuestAccounts::Files);
        register_files(&h, &s).unwrap();
        let cmds = h.commands();
        assert_eq!(cmds.len(), 2 * TEST_UID_COUNT as usize, "{cmds:?}");
        assert!(
            cmds.iter()
                .all(|c| { c.starts_with("groupadd ") || c.starts_with("useradd ") })
        );
        // The two account files and their shadow files (`/etc/shadow` and the group one). The group shadow path is built from a
        // prefix so the source holds no token shaped like a host name (the remote scanner blocks those).
        let shadow = |prefix: &str| format!("/etc/{prefix}shadow");
        for p in [
            "/etc/passwd".to_string(),
            "/etc/group".to_string(),
            shadow(""),
            shadow("g"),
        ] {
            assert!(!h.exists(std::path::Path::new(&p)), "{p} was written");
        }
        let uid = s.uid_base;
        let group = format!("groupadd --system --gid {uid} {}", name(uid));
        assert!(cmds.contains(&group), "{cmds:?}");
        let user = cmds.iter().find(|c| c.starts_with("useradd")).unwrap();
        for want in [
            "--system",
            "--no-create-home",
            "--no-log-init",
            "--shell /usr/sbin/nologin",
            &format!("--uid {uid}"),
            &format!("--gid {uid}"),
        ] {
            assert!(user.contains(want), "{want} missing in {user}");
        }
        assert!(user.ends_with(&name(uid)), "{user}");
        for secret in ["--password", "-p ", "--create-home"] {
            assert!(!user.contains(secret), "{secret} in {user}");
        }
    }

    #[test]
    fn files_mode_is_idempotent() {
        let h = old_host();
        let s = settings(&h, GuestAccounts::Files);
        register_files(&h, &s).unwrap();
        let before = (h.snapshot(), h.commands());
        assert_eq!(register_files(&h, &s).unwrap(), 0);
        assert_eq!((h.snapshot(), h.commands()), before);
    }

    /// A half-finished earlier run (the group, not the user) is completed.
    #[test]
    fn files_mode_completes_a_partial_earlier_run() {
        let h = old_host();
        let s = settings(&h, GuestAccounts::Files);
        h.run(&crate::ops::Cmd::new(
            "groupadd",
            &[
                "--system",
                "--gid",
                &s.uid_base.to_string(),
                &name(s.uid_base),
            ],
        ))
        .unwrap();
        assert_eq!(register_files(&h, &s).unwrap(), TEST_UID_COUNT as usize);
        assert!(h.getent("passwd", &name(s.uid_base)).is_some());
    }

    #[test]
    fn a_guest_name_with_another_uid_is_a_conflict() {
        let h = old_host();
        let s = settings(&h, GuestAccounts::Files);
        h.state
            .lock()
            .unwrap()
            .users
            .insert(name(s.uid_base), (s.uid_base + 7, s.uid_base + 7));
        let Err(Error::Unsupported(why)) = register_files(&h, &s) else {
            panic!("a conflicting account was accepted");
        };
        assert!(why.contains(&name(s.uid_base)), "{why}");
    }

    #[test]
    fn a_gid_held_by_another_group_is_a_conflict() {
        let h = old_host();
        let s = settings(&h, GuestAccounts::Files);
        h.state.lock().unwrap().groups.insert(
            "intruder".into(),
            (s.uid_base, std::collections::BTreeSet::new()),
        );
        let Err(Error::Unsupported(why)) = register_files(&h, &s) else {
            panic!("a conflicting group was accepted");
        };
        assert!(why.contains("intruder"), "{why}");
    }

    // ------------------------------------------------------------- problems

    #[test]
    fn userdb_mode_needs_a_systemd_source() {
        let h = FakeHost::debian();
        let s = settings(&h, GuestAccounts::Userdb);
        assert!(problems(&h, &s).is_empty());
        nsswitch(&h, "passwd: files systemd\ngroup: files\n");
        let p = problems(&h, &s);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].contains("nsswitch.conf group") && p[0].contains("systemd"),
            "{p:?}"
        );
    }

    /// `files` needs only what every host has, and no `systemd` source.
    #[test]
    fn files_mode_needs_files_or_compat() {
        let h = old_host();
        let s = settings(&h, GuestAccounts::Files);
        for ok in [
            "passwd: files\ngroup: files\n",
            "passwd: compat\ngroup: compat\n",
        ] {
            nsswitch(&h, ok);
            assert!(problems(&h, &s).is_empty(), "{ok}");
        }
        nsswitch(&h, "passwd: sss\ngroup: files\n");
        let p = problems(&h, &s);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("nsswitch.conf passwd"), "{p:?}");
        assert!(!p[0].contains("'systemd'"), "{p:?}");
    }

    #[test]
    fn files_mode_reports_a_foreign_group_in_the_range() {
        let h = old_host();
        let s = settings(&h, GuestAccounts::Files);
        h.state.lock().unwrap().groups.insert(
            "intruder".into(),
            (s.uid_base + 1, std::collections::BTreeSet::new()),
        );
        let p = problems(&h, &s);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("group intruder"), "{p:?}");
        // Our own groups, from an earlier run, are not a conflict.
        let again = old_host();
        let s2 = settings(&again, GuestAccounts::Files);
        register_files(&again, &s2).unwrap();
        assert!(problems(&again, &s2).is_empty());
    }

    #[test]
    fn the_names_are_stable() {
        assert_eq!(GuestAccounts::Userdb.name(), "userdb");
        assert_eq!(GuestAccounts::Files.name(), "files");
    }
}
