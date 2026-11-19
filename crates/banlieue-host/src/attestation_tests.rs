// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `attestation.rs`, and for the fake's attestation CA, which
//! must refuse what the real one refuses.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::fake::FakeHost;
    use crate::ops::{Host, Owner, Probe};
    use std::time::Duration;

    const CA_DIR: &str = "/var/lib/banlieue/attestation-ca";

    fn host_with_provider_user() -> FakeHost {
        let h = FakeHost::debian();
        {
            let mut s = h.state.lock().unwrap();
            s.users.insert("banlieue".into(), (998, 998));
            s.groups
                .insert("banlieue".into(), (998, Default::default()));
        }
        h.put_dir("/var/lib/banlieue");
        h
    }

    fn banlieue() -> Owner {
        Owner::new("banlieue", "banlieue")
    }

    #[test]
    fn ttl_accepts_the_documented_forms() {
        assert_eq!(parse_ttl("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_ttl("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_ttl("12h").unwrap(), Duration::from_secs(43_200));
        assert_eq!(parse_ttl("300s").unwrap(), MIN_CERTIFICATE_TTL);
        assert_eq!(parse_ttl("7d").unwrap(), MAX_CERTIFICATE_TTL);
    }

    #[test]
    fn ttl_refuses_out_of_bounds_and_malformed_values() {
        for bad in [
            "4m",
            "299s",
            "8d",
            "169h",
            "0h",
            "",
            "h",
            "1",
            "abc",
            "1x",
            "-1h",
            "1.5h",
            "+1h",
            " 1h",
            "1 h",
            "99999999999999999999h",
        ] {
            assert!(parse_ttl(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn the_default_ttl_is_one_hour() {
        assert_eq!(DEFAULT_CERTIFICATE_TTL, Duration::from_secs(3600));
    }

    #[test]
    fn the_default_name_is_the_short_name_inside_the_alphabet() {
        assert_eq!(default_name("bar").unwrap(), "bar");
        assert_eq!(default_name("bar-1.foo_2").unwrap(), "bar-1.foo_2");
        assert!(default_name("b@r").is_err());
        assert!(default_name("").is_err());
    }

    /// The schema the provider reads: exactly these keys.
    #[test]
    fn the_section_has_exactly_the_schema_the_provider_reads() {
        let s = section(Duration::from_secs(3600));
        assert_eq!(s.ca_dir, std::path::PathBuf::from(CA_DIR));
        assert_eq!(s.certificate_ttl_secs, 3600);
        let text = toml::to_string(&s).unwrap();
        assert_eq!(
            text,
            "ca_dir = \"/var/lib/banlieue/attestation-ca\"\ncertificate_ttl_secs = 3600\n"
        );
    }

    #[test]
    fn the_admin_action_names_the_certificate_the_name_and_the_step_never_the_key() {
        let lines = admin_action(Path::new(CA_DIR), "bar").join("\n");
        assert!(lines.contains("/var/lib/banlieue/attestation-ca/ca.crt"));
        assert!(lines.contains("common name: bar"));
        assert!(lines.contains("Provider.spec.attestation.hostCa"));
        assert!(lines.contains("x509pop"));
        assert!(lines.contains("unique"));
        assert!(!lines.contains("ca.key"));
        assert!(!lines.contains("PRIVATE"));
    }

    #[test]
    fn the_fake_removes_both_ca_files_and_keeps_the_directory() {
        let h = host_with_provider_user();
        h.create_attestation_ca(Path::new(CA_DIR), "bar", &banlieue())
            .unwrap();
        h.remove_attestation_ca(Path::new(CA_DIR)).unwrap();
        assert!(!h.exists(Path::new("/var/lib/banlieue/attestation-ca/ca.key")));
        assert!(!h.exists(Path::new("/var/lib/banlieue/attestation-ca/ca.crt")));
        assert!(h.exists(Path::new(CA_DIR)));
    }

    /// The real one acts on a handle: a link planted where the old key was is
    /// unlinked, and a link planted where the directory was is refused.
    #[test]
    fn the_real_removal_never_follows_a_planted_symlink() {
        use std::os::unix::fs::symlink;
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("attestation-ca");
        std::fs::create_dir(&dir).unwrap();
        let victim = t.path().join("victim");
        std::fs::write(&victim, b"victim").unwrap();
        symlink(&victim, dir.join("ca.key")).unwrap();
        crate::real::RealHost::new()
            .remove_attestation_ca(&dir)
            .unwrap();
        assert!(std::fs::symlink_metadata(dir.join("ca.key")).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"victim");

        let link = t.path().join("link");
        symlink(t.path(), &link).unwrap();
        std::fs::write(t.path().join("ca.key"), b"decoy").unwrap();
        assert!(
            crate::real::RealHost::new()
                .remove_attestation_ca(&link)
                .is_err()
        );
        assert_eq!(std::fs::read(t.path().join("ca.key")).unwrap(), b"decoy");
    }

    #[test]
    fn the_fake_creates_a_ca_as_the_real_one_would() {
        let h = host_with_provider_user();
        h.create_attestation_ca(Path::new(CA_DIR), "bar", &banlieue())
            .unwrap();
        let mode_owner = |p: &str| {
            let st = h.stat(Path::new(p)).unwrap();
            (st.mode, st.owner.user)
        };
        assert_eq!(mode_owner(CA_DIR), (0o700, "banlieue".into()));
        assert_eq!(
            mode_owner("/var/lib/banlieue/attestation-ca/ca.key"),
            (0o600, "banlieue".into())
        );
        assert_eq!(
            mode_owner("/var/lib/banlieue/attestation-ca/ca.crt"),
            (0o644, "banlieue".into())
        );
        assert_eq!(h.attestation_ca(Path::new(CA_DIR)).unwrap().name, "bar");
    }

    /// A fake more permissive than the real thing hides bugs.
    #[test]
    fn the_fake_never_replaces_a_ca_and_refuses_a_bad_name() {
        let h = host_with_provider_user();
        assert!(
            h.create_attestation_ca(Path::new(CA_DIR), "bad name", &banlieue())
                .is_err()
        );
        assert!(
            !h.exists(Path::new(CA_DIR)),
            "a refused name creates nothing"
        );
        h.create_attestation_ca(Path::new(CA_DIR), "bar", &banlieue())
            .unwrap();
        let before = h.snapshot();
        assert!(
            h.create_attestation_ca(Path::new(CA_DIR), "baz", &banlieue())
                .is_err()
        );
        assert_eq!(h.snapshot(), before);
        let ghost = Owner::new("nobody-here", "nobody-here");
        assert!(
            h.create_attestation_ca(Path::new("/var/lib/banlieue/other"), "bar", &ghost)
                .is_err()
        );
    }

    #[test]
    fn inspect_runs_the_crates_checks_through_the_fake() {
        let h = host_with_provider_user();
        h.create_attestation_ca(Path::new(CA_DIR), "bar", &banlieue())
            .unwrap();
        let key = "/var/lib/banlieue/attestation-ca/ca.key";
        h.set_mode(Path::new(key), 0o640).unwrap();
        assert!(h.attestation_ca(Path::new(CA_DIR)).is_err());
        h.set_mode(Path::new(key), 0o600).unwrap();
        h.set_mode(Path::new(CA_DIR), 0o755).unwrap();
        assert!(h.attestation_ca(Path::new(CA_DIR)).is_err());
        h.set_mode(Path::new(CA_DIR), 0o700).unwrap();
        let info = h.attestation_ca(Path::new(CA_DIR)).unwrap();
        assert_eq!(info.name, "bar");
        assert!(!info.not_after.is_empty());
    }
}
