// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `stages.rs`, against the in-memory host. The numbered
//! invariants are roadmap 09 phase 10's.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::dryrun::DryRun;
    use crate::fake::{FakeHost, State};
    use crate::fetch::Fetch;
    use crate::pins::{Artifact, Release, Source};
    use crate::settings::{Settings, resolve};
    use async_trait::async_trait;
    use banlieue_provider_cloud_hypervisor::provider::HostFacts;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    const VMM_BYTES: &[u8] = b"cloud-hypervisor v53.0";
    const REMOTE_BYTES: &[u8] = b"ch-remote v53.0";
    const FIRMWARE_BYTES: &[u8] = b"CLOUDHV.fd";
    /// Few guest uids, so the tests stay fast.
    const TEST_UID_COUNT: u32 = 4;

    const NEWER_VMM_BYTES: &[u8] = b"cloud-hypervisor v54.0";
    const NEWER_REMOTE_BYTES: &[u8] = b"ch-remote v54.0";
    const NEWER_FIRMWARE_BYTES: &[u8] = b"CLOUDHV.fd, newer";

    fn known(url: &str, bytes: &[u8]) -> Source {
        Source {
            url: url.into(),
            sha256: pins::sha256_hex(bytes),
        }
    }

    /// The pinned release's layout with known bytes: no test can produce
    /// bytes matching the real digests.
    fn release() -> Release {
        Release::new(
            pins::VMM_VERSION,
            pins::FIRMWARE_TAG,
            known("https://github.com/vmm", VMM_BYTES),
            known("https://github.com/ch-remote", REMOTE_BYTES),
            known("https://github.com/firmware", FIRMWARE_BYTES),
        )
    }

    /// Another release, as `--vmm-version` and `--firmware-tag` choose.
    fn newer() -> Release {
        Release::new(
            "v54.0",
            "ch-0123456789",
            known("https://internal.example.com/vmm", NEWER_VMM_BYTES),
            known("https://internal.example.com/ch-remote", NEWER_REMOTE_BYTES),
            known(
                "https://internal.example.com/firmware",
                NEWER_FIRMWARE_BYTES,
            ),
        )
    }

    /// Serves artifacts by name, and counts fetches.
    #[derive(Default)]
    struct Canned {
        files: BTreeMap<&'static str, Vec<u8>>,
        fetched: Mutex<Vec<&'static str>>,
    }

    impl Canned {
        fn good() -> Self {
            Self {
                files: BTreeMap::from([
                    ("cloud-hypervisor-static", VMM_BYTES.to_vec()),
                    ("ch-remote-static", REMOTE_BYTES.to_vec()),
                    ("CLOUDHV.fd", FIRMWARE_BYTES.to_vec()),
                ]),
                ..Self::default()
            }
        }

        fn newer() -> Self {
            Self {
                files: BTreeMap::from([
                    ("cloud-hypervisor-static", NEWER_VMM_BYTES.to_vec()),
                    ("ch-remote-static", NEWER_REMOTE_BYTES.to_vec()),
                    ("CLOUDHV.fd", NEWER_FIRMWARE_BYTES.to_vec()),
                ]),
                ..Self::default()
            }
        }
    }

    #[async_trait]
    impl Fetch for Canned {
        async fn fetch(&self, a: &Artifact) -> Result<Vec<u8>, Error> {
            self.fetched.lock().unwrap().push(a.name);
            self.files.get(a.name).cloned().ok_or(Error::Fetch {
                name: a.name,
                why: "not canned".into(),
            })
        }
    }

    fn all_present(c: &HostConfig) -> HostFacts {
        HostFacts {
            kvm: true,
            vmm_binary: true,
            firmware: true,
            storage_present: c.storage_classes.keys().cloned().collect(),
            bridges_present: c.network_classes.keys().cloned().collect(),
            vtpm: true,
            ..HostFacts::default()
        }
    }

    /// A host whose own package manager installed what banlieue needs.
    fn fake_host() -> FakeHost {
        let h = FakeHost::debian();
        h.install_packages(&["swtpm", "swtpm-tools", "systemd"]);
        h
    }

    /// The flags every test installs with: few guest uids.
    fn host_args() -> crate::settings::HostArgs {
        crate::settings::HostArgs {
            guest_uid_count: Some(TEST_UID_COUNT),
            registry_keep_unreferenced: crate::settings::DEFAULT_KEEP_UNREFERENCED,
            ..crate::settings::HostArgs::default()
        }
    }

    fn settings(h: &FakeHost) -> Settings {
        resolve(&host_args(), h).unwrap()
    }

    fn opts() -> Options {
        Options::default()
    }

    async fn install_all(h: &FakeHost, s: &Settings, o: &Options) -> Result<(), Error> {
        install(h, &Canned::good(), &release(), s, o, &all_present).await
    }

    /// The host's state without the EK CA's serial counter, which every
    /// manufacture advances, the self-test's included.
    fn without_serial(mut st: State) -> State {
        st.fs
            .remove(Path::new("/var/lib/banlieue/swtpm-localca/certserial"));
        st
    }

    fn serial(h: &FakeHost) -> u64 {
        let raw = h
            .read(Path::new("/var/lib/banlieue/swtpm-localca/certserial"))
            .unwrap();
        String::from_utf8(raw).unwrap().trim().parse().unwrap()
    }

    fn mode_owner(h: &FakeHost, p: &str) -> (u32, String, String) {
        let st = h
            .stat(Path::new(p))
            .unwrap_or_else(|| panic!("{p} missing"));
        (st.mode, st.owner.user, st.owner.group)
    }

    fn count(h: &FakeHost, prefix: &str) -> usize {
        h.commands()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }

    // ------------------------------------------------------------ preflight

    #[test]
    fn preflight_passes_on_a_ready_host() {
        let h = fake_host();
        preflight(&h, &settings(&h)).unwrap();
    }

    /// banlieue installs no packages (ADR-0084 Decision 1): preflight
    /// names every command the host must supply, by command, on any OS.
    #[test]
    fn preflight_names_every_command_the_host_must_supply() {
        let h = FakeHost::debian();
        let Err(Error::Preflight(problems)) = preflight(&h, &settings(&h)) else {
            panic!("preflight passed without swtpm or systemd");
        };
        let all = problems.join("\n");
        for c in REQUIRED_COMMANDS {
            assert!(
                all.contains(&format!("{c} is not on PATH")),
                "{c} in:\n{all}"
            );
        }
        assert!(h.commands().is_empty(), "preflight installs nothing");
        h.install_packages(&["swtpm", "swtpm-tools", "systemd"]);
        preflight(&h, &settings(&h)).unwrap();
    }

    /// Every problem is reported at once, not one per run.
    #[test]
    fn preflight_reports_every_problem() {
        let h = fake_host();
        h.env.lock().unwrap().virt = Some("kvm".into());
        {
            let mut st = h.state.lock().unwrap();
            st.fs.remove(Path::new("/dev/kvm"));
            st.users.insert("mallory".into(), (2_000_001, 2_000_001));
            st.fs.remove(Path::new("/usr/bin/systemctl"));
        }
        // Neither `systemd` (userdb) nor `files`/`compat` (files mode,
        // ADR-0139): no mode can resolve a guest account on this host. A
        // plain `files` line is not a problem any more: it selects files mode.
        h.put_file(
            "/etc/nsswitch.conf",
            b"passwd: sss\ngroup: sss\n",
            Kind::File,
        );
        h.put_file("/etc/subuid", b"someone:1999000:100000\n", Kind::File);
        let mut s = settings(&h);
        s.network = vec![("lan".into(), "br9".into())];
        let Err(Error::Preflight(problems)) = preflight(&h, &s) else {
            panic!("preflight passed");
        };
        let all = problems.join("\n");
        for expected in [
            "inside a VM",
            "/dev/kvm",
            "systemctl is not on PATH",
            "br9: not a bridge",
            "account mallory",
            "nsswitch.conf passwd",
            "nsswitch.conf group",
            "/etc/subuid",
        ] {
            assert!(all.contains(expected), "missing {expected:?} in:\n{all}");
        }
    }

    #[test]
    fn a_virtualized_lab_host_passes_when_allowed() {
        let h = fake_host();
        h.env.lock().unwrap().virt = Some("kvm".into());
        let mut s = settings(&h);
        s.allow_virtualized = true;
        preflight(&h, &s).unwrap();
    }

    // ------------------------------------------------------------------ vmm

    #[tokio::test]
    async fn the_vmm_is_installed_verified_and_linked() {
        let h = fake_host();
        let fetch = Canned::good();
        vmm(&h, &fetch, &release(), &Options::default())
            .await
            .unwrap();
        let r = release();
        for a in &r.artifacts {
            assert_eq!(pins::sha256_hex(&h.read(&a.dest).unwrap()), a.sha256);
            assert_eq!(h.stat(&a.dest).unwrap().mode, a.mode);
        }
        for (at, target) in &r.symlinks {
            assert_eq!(h.stat(at).unwrap().kind, Kind::Symlink(target.clone()));
        }
        // Already installed and matching: nothing is fetched again.
        let again = Canned::good();
        vmm(&h, &again, &release(), &Options::default())
            .await
            .unwrap();
        assert!(again.fetched.lock().unwrap().is_empty());
    }

    /// Another release installs beside the pinned one: its own directories,
    /// the symlinks moved, the old files kept for a rollback (ADR-0084
    /// Decision 3).
    #[tokio::test]
    async fn another_release_installs_beside_the_pinned_one() {
        let h = fake_host();
        vmm(&h, &Canned::good(), &release(), &Options::default())
            .await
            .unwrap();
        let fetch = Canned::newer();
        vmm(&h, &fetch, &newer(), &Options::default())
            .await
            .unwrap();
        let n = newer();
        for a in &n.artifacts {
            assert_eq!(
                pins::sha256_hex(&h.read(&a.dest).unwrap()),
                a.sha256,
                "{}",
                a.name
            );
        }
        for (at, target) in &n.symlinks {
            assert_eq!(h.stat(at).unwrap().kind, Kind::Symlink(target.clone()));
        }
        for a in &release().artifacts {
            assert!(h.exists(&a.dest), "{} kept", a.dest.display());
        }
        // Rolling back fetches nothing: the old files are still verified.
        let back = Canned::good();
        vmm(&h, &back, &release(), &Options::default())
            .await
            .unwrap();
        assert!(back.fetched.lock().unwrap().is_empty());
        for (at, target) in &release().symlinks {
            assert_eq!(h.stat(at).unwrap().kind, Kind::Symlink(target.clone()));
        }
    }

    /// Invariant 1: a corrupt artifact installs nothing and leaves the
    /// previous release, symlinks included, byte-identical.
    #[tokio::test]
    async fn a_pin_mismatch_installs_nothing() {
        let h = fake_host();
        vmm(&h, &Canned::good(), &release(), &Options::default())
            .await
            .unwrap();
        let before = h.snapshot();
        let mut corrupt = Canned::good();
        corrupt.files.insert("CLOUDHV.fd", b"tampered".to_vec());
        let forced = Options {
            force: true,
            ..Options::default()
        };
        let err = vmm(&h, &corrupt, &release(), &forced).await.unwrap_err();
        assert!(
            matches!(
                err,
                Error::Pin {
                    name: "CLOUDHV.fd",
                    ..
                }
            ),
            "{err}"
        );
        assert_eq!(h.snapshot(), before);
    }

    // ----------------------------------------------------------------- host

    #[tokio::test]
    async fn the_host_layout_is_the_access_control() {
        let h = fake_host();
        let s = settings(&h);
        install_all(&h, &s, &opts()).await.unwrap();
        for (path, mode, user, group) in [
            (CONF_DIR, 0o755, "root", "root"),
            (CREDENTIALS_DIR, 0o700, "banlieue", "banlieue"),
            (STATE_ROOT, 0o751, "banlieue", "banlieue"),
            ("/var/lib/banlieue/ek", 0o700, "banlieue", "banlieue"),
            ("/var/lib/banlieue/tpm", 0o711, "banlieue", "banlieue"),
            ("/var/lib/banlieue/units", 0o700, "banlieue", "banlieue"),
            ("/srv/banlieue/ch", 0o711, "banlieue", "banlieue"),
            ("/srv/banlieue/ch/images", 0o750, "banlieue", "banlieue"),
            (HOST_CONFIG, 0o640, "root", "banlieue"),
            (EK_CA_DIR, 0o700, "banlieue", "banlieue"),
            (EK_CA_CERT, 0o644, "banlieue", "banlieue"),
            (
                "/var/lib/banlieue/swtpm-localca/signkey.pem",
                0o600,
                "banlieue",
                "banlieue",
            ),
            (POLKIT_RULE, 0o644, "root", "root"),
        ] {
            assert_eq!(
                mode_owner(&h, path),
                (mode, user.into(), group.into()),
                "{path}"
            );
        }
        assert!(
            h.getent("group", "kvm")
                .is_some_and(|l| !l.contains("banlieue")),
            "the provider only stat()s /dev/kvm: it is not in kvm"
        );
        assert!(
            h.exists(Path::new("/dev/net/tun")),
            "the tun node exists before the provider starts"
        );
        // One user and one group record per guest uid, plus a uid link each.
        let records = h.list(Path::new(USERDB_DIR));
        assert_eq!(records.len(), 4 * TEST_UID_COUNT as usize, "{records:?}");
        assert!(h.getent("passwd", "2000003").is_some());
        // The host config is the provider's own.
        let text = String::from_utf8(h.read(Path::new(HOST_CONFIG)).unwrap()).unwrap();
        let config = HostConfig::parse(&text).unwrap();
        assert_eq!(config.provider.name, "bar");
        assert_eq!(config.guests.uid_count, TEST_UID_COUNT);
    }

    /// Invariant 2: idempotence is equality, not absence of error.
    #[tokio::test]
    async fn a_second_install_changes_nothing() {
        let h = fake_host();
        let s = settings(&h);
        install_all(&h, &s, &opts()).await.unwrap();
        let first = without_serial(h.snapshot());
        let ran = h.commands().len();
        install_all(&h, &s, &opts()).await.unwrap();
        assert_eq!(without_serial(h.snapshot()), first);
        let again: Vec<String> = h.commands().split_off(ran);
        for c in &again {
            assert!(
                ![
                    "useradd",
                    "usermod",
                    "gpasswd",
                    "apt-get",
                    "systemd-tmpfiles",
                    "systemctl restart",
                ]
                .iter()
                .any(|p| c.starts_with(p))
                    && !c.contains("--create-ek-cert --config"),
                "second run changed the host: {c}"
            );
        }
    }

    /// The documented upgrade, `--only provider --provider-binary`, re-run
    /// without the original flags on a host where the probe would now pick
    /// other storage (found live: the units named `/home/banlieue/ch`, the
    /// provider's sandbox mounted the real storage read-only, and every
    /// machine failed with EROFS). The kept config decides, so the units
    /// still name the storage it names and the re-run changes nothing.
    #[tokio::test]
    async fn a_provider_only_rerun_follows_the_kept_config() {
        let h = fake_host();
        let first = settings(&h);
        install_all(&h, &first, &opts()).await.unwrap();
        let unit = format!("/etc/systemd/system/{PROVIDER_UNIT}");
        let before = String::from_utf8(h.read(Path::new(&unit)).unwrap()).unwrap();
        assert!(before.contains("/srv/banlieue/ch"), "{before}");

        h.put_dir("/home");
        h.env.lock().unwrap().free.insert("/home".into(), 900 << 30);
        assert_eq!(
            crate::settings::default_storage(&h),
            PathBuf::from("/home/banlieue/ch")
        );
        let again = resolve(&crate::settings::HostArgs::default(), &h).unwrap();
        assert_eq!(again.storage, first.storage);
        assert_eq!(again.network, first.network);
        assert_eq!(
            (again.uid_base, again.uid_count),
            (first.uid_base, first.uid_count)
        );

        let snapshot = without_serial(h.snapshot());
        let ran = h.commands().len();
        let only = Options {
            only: Some(Stage::Provider),
            ..Options::default()
        };
        install_all(&h, &again, &only).await.unwrap();
        assert_eq!(without_serial(h.snapshot()), snapshot);
        assert!(
            h.commands()
                .split_off(ran)
                .iter()
                .all(|c| !c.starts_with("systemctl restart")),
            "an unchanged unit restarted the provider"
        );
    }

    /// A host installed before the provider stopped needing `/dev/kvm`
    /// leaves the kvm group; one whose tmpfiles.d entry predates the tun
    /// node gets it applied, so the node exists before the provider's next
    /// start.
    #[tokio::test]
    async fn an_older_install_is_brought_to_least_privilege() {
        let h = fake_host();
        let s = settings(&h);
        install_all(&h, &s, &opts()).await.unwrap();
        h.run(&Cmd::new("usermod", &["-aG", "kvm", BANLIEUE_USER]))
            .unwrap();
        h.write(
            Path::new(TMPFILES_CONF),
            b"d /run/banlieue 0755 root root -\n",
            0o644,
            &crate::ops::Owner::root(),
        )
        .unwrap();
        h.remove(Path::new("/dev/net/tun")).unwrap();

        super::super::host(&h, &s, &release(), &Options::default()).unwrap();

        assert!(
            h.getent("group", "kvm")
                .is_some_and(|l| !l.contains("banlieue")),
            "left kvm"
        );
        assert_eq!(count(&h, "gpasswd -d banlieue kvm"), 1);
        assert!(h.exists(Path::new("/dev/net/tun")));
    }

    /// An edited host config is the admin's: kept unless --force.
    #[tokio::test]
    async fn the_host_config_is_kept_unless_forced() {
        let h = fake_host();
        let s = settings(&h);
        install_all(&h, &s, &opts()).await.unwrap();
        let edited = b"# the admin's\n".to_vec();
        h.write(Path::new(HOST_CONFIG), &edited, 0o640, &root_banlieue())
            .unwrap();
        super::super::host(&h, &s, &release(), &Options::default()).unwrap();
        assert_eq!(h.read(Path::new(HOST_CONFIG)).unwrap(), edited);
        // Not even a release change touches a file that does not parse.
        super::super::host(&h, &s, &newer(), &Options::default()).unwrap();
        assert_eq!(h.read(Path::new(HOST_CONFIG)).unwrap(), edited);
        let forced = Options {
            force: true,
            ..Options::default()
        };
        super::super::host(&h, &s, &release(), &forced).unwrap();
        assert_ne!(h.read(Path::new(HOST_CONFIG)).unwrap(), edited);
    }

    /// Changing the VMM never needs `--force` and its EK CA rotation: an
    /// existing host config keeps every key but `[vmm]`, which follows the
    /// release (ADR-0084 Decision 5).
    #[tokio::test]
    async fn the_host_configs_vmm_section_follows_the_release() {
        let h = fake_host();
        let s = settings(&h);
        install_all(&h, &s, &opts()).await.unwrap();
        let read = || {
            HostConfig::parse(&String::from_utf8(h.read(Path::new(HOST_CONFIG)).unwrap()).unwrap())
                .unwrap()
        };
        let mut admin = read();
        admin.guests.uid_count = TEST_UID_COUNT - 1;
        admin.network_classes.insert("lab".into(), "virbr0".into());
        h.write(
            Path::new(HOST_CONFIG),
            toml::to_string(&admin).unwrap().as_bytes(),
            0o640,
            &root_banlieue(),
        )
        .unwrap();
        let manufactures = count(&h, "(as banlieue) swtpm_setup");

        super::super::host(&h, &s, &newer(), &Options::default()).unwrap();
        let after = read();
        assert_eq!(after.vmm.version, "v54.0");
        assert_eq!(after.vmm.firmware, newer().firmware);
        assert_eq!(
            after.guests.uid_count,
            TEST_UID_COUNT - 1,
            "the admin's edit is kept"
        );
        assert_eq!(after.bridge("lab"), Some("virbr0"));
        assert_eq!(
            mode_owner(&h, HOST_CONFIG),
            (0o640, "root".into(), "banlieue".into())
        );
        assert_eq!(
            count(&h, "(as banlieue) swtpm_setup"),
            manufactures,
            "no EK CA rotation"
        );

        // Already naming it: a second run rewrites nothing.
        let before = h.snapshot();
        super::super::host(&h, &s, &newer(), &Options::default()).unwrap();
        assert_eq!(h.snapshot(), before);
    }

    /// The provider's user owns its state root, and could plant a symlink
    /// where the installer, as root, expects a directory. It is refused, and
    /// what it points at is left alone.
    #[tokio::test]
    async fn a_symlink_planted_where_a_directory_goes_is_refused() {
        let h = fake_host();
        let s = settings(&h);
        install_all(&h, &s, &opts()).await.unwrap();
        h.remove(Path::new(EK_CA_DIR)).unwrap();
        h.symlink(Path::new("/etc"), Path::new(EK_CA_DIR)).unwrap();
        let etc = h.stat(Path::new("/etc")).unwrap();
        assert!(tpm(&h, &Options::default()).is_err());
        assert_eq!(h.stat(Path::new("/etc")).unwrap(), etc);
    }

    /// Directories other packages own (polkit's rules directory is
    /// `root:polkitd` on Debian) are created when missing and otherwise left
    /// exactly as they are.
    #[tokio::test]
    async fn directories_other_packages_own_are_not_reowned() {
        let h = fake_host();
        h.state
            .lock()
            .unwrap()
            .groups
            .insert("polkitd".into(), (997, Default::default()));
        h.put_dir("/etc/polkit-1/rules.d");
        h.mkdir(
            Path::new("/etc/polkit-1/rules.d"),
            0o750,
            &crate::ops::Owner::new("root", "polkitd"),
        )
        .unwrap();
        let s = settings(&h);
        install_all(&h, &s, &opts()).await.unwrap();
        assert_eq!(
            mode_owner(&h, "/etc/polkit-1/rules.d"),
            (0o750, "root".into(), "polkitd".into())
        );
        assert!(h.exists(Path::new(POLKIT_RULE)));
    }

    // ------------------------------------------------------------------ tpm

    #[tokio::test]
    async fn the_ek_ca_is_created_once_and_rotated_only_when_forced() {
        let h = fake_host();
        let s = settings(&h);
        install_all(&h, &s, &opts()).await.unwrap();
        let manufactures = || {
            count(
                &h,
                "(as banlieue) swtpm_setup --tpm2 --tpmstate /var/lib/banlieue/.host-scratch --create-ek-cert --config",
            )
        };
        assert_eq!(manufactures(), 1);
        tpm(&h, &Options::default()).unwrap();
        assert_eq!(manufactures(), 1, "an existing CA is kept");
        tpm(
            &h,
            &Options {
                force: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(manufactures(), 2, "--force rotates it");
        assert!(!h.exists(&Path::new(STATE_ROOT).join(".host-scratch")));
    }

    // ------------------------------------------------------------- provider

    /// Invariant 5: the provider unit is enabled only when it can run.
    #[tokio::test]
    async fn the_provider_is_enabled_only_with_its_binary_and_kubeconfig() {
        let h = fake_host();
        let s = settings(&h);
        install_all(&h, &s, &opts()).await.unwrap();
        for t in crate::paths::UNIT_TEMPLATES.iter().chain([&PROVIDER_UNIT]) {
            assert!(h.exists(&Path::new(SYSTEMD_UNIT_DIR).join(t)), "{t}");
        }
        assert_eq!(count(&h, "systemctl enable"), 0, "no binary, no kubeconfig");

        h.put_file("/tmp/banlieue", b"new build", Kind::File);
        h.write(
            Path::new(KUBECONFIG_PATH),
            b"kubeconfig",
            0o600,
            &banlieue(),
        )
        .unwrap();
        let with_binary = Options {
            provider_binary: Some("/tmp/banlieue".into()),
            ..Options::default()
        };
        provider(&h, &s, &with_binary).unwrap();
        assert_eq!(h.read(Path::new(PROVIDER_BINARY)).unwrap(), b"new build");
        assert_eq!(
            mode_owner(&h, PROVIDER_BINARY),
            (0o755, "root".into(), "root".into())
        );
        assert_eq!(count(&h, "systemctl enable --now"), 1);
        // Already running: not enabled again, and nothing changed, so not
        // restarted either.
        provider(&h, &s, &Options::default()).unwrap();
        assert_eq!(count(&h, "systemctl enable --now"), 1);
        assert_eq!(count(&h, "systemctl restart"), 0);
    }

    /// A running provider is restarted when its unit or its binary changed,
    /// or the new sandbox and the new build would wait for a reboot. Guests
    /// keep running: KillMode=process (ADR-0063 Decision 1).
    #[tokio::test]
    async fn a_running_provider_restarts_when_its_unit_or_binary_changes() {
        let h = fake_host();
        let s = settings(&h);
        h.write(
            Path::new(PROVIDER_BINARY),
            b"build 1",
            0o755,
            &crate::ops::Owner::root(),
        )
        .unwrap();
        install_all(&h, &s, &opts()).await.unwrap();
        h.write(Path::new(KUBECONFIG_PATH), b"k", 0o600, &banlieue())
            .unwrap();
        provider(&h, &s, &Options::default()).unwrap();
        assert_eq!(count(&h, "systemctl enable --now"), 1);
        assert_eq!(count(&h, "systemctl restart"), 0, "just started");

        // An older unit on disk: the install rewrites it and restarts.
        let unit = Path::new(SYSTEMD_UNIT_DIR).join(PROVIDER_UNIT);
        h.write(
            &unit,
            b"[Service]\nReadOnlyPaths=/dev\n",
            0o644,
            &crate::ops::Owner::root(),
        )
        .unwrap();
        provider(&h, &s, &Options::default()).unwrap();
        assert_eq!(count(&h, &format!("systemctl restart {PROVIDER_UNIT}")), 1);

        // A new binary: restarted again.
        h.put_file("/tmp/banlieue", b"build 2", Kind::File);
        let with_binary = Options {
            provider_binary: Some("/tmp/banlieue".into()),
            ..Options::default()
        };
        provider(&h, &s, &with_binary).unwrap();
        assert_eq!(count(&h, "systemctl restart"), 2);

        // The same binary again changes nothing.
        provider(&h, &s, &with_binary).unwrap();
        assert_eq!(count(&h, "systemctl restart"), 2);
        assert_eq!(count(&h, "systemctl enable --now"), 1);
    }

    /// In a container, or an image being built, systemd is not running:
    /// the files are installed and nothing is loaded or started.
    #[tokio::test]
    async fn without_systemd_units_are_installed_not_loaded() {
        let h = fake_host();
        h.env.lock().unwrap().systemd_running = false;
        let s = settings(&h);
        h.write(
            Path::new("/usr/local/bin/banlieue"),
            b"b",
            0o755,
            &crate::ops::Owner::root(),
        )
        .unwrap();
        install_all(&h, &s, &opts()).await.unwrap();
        h.write(Path::new(KUBECONFIG_PATH), b"k", 0o600, &banlieue())
            .unwrap();
        provider(&h, &s, &Options::default()).unwrap();
        assert_eq!(count(&h, "systemctl"), 0);
    }

    // ------------------------------------------------------------- selftest

    #[tokio::test]
    async fn selftest_passes_on_an_installed_host() {
        let h = fake_host();
        let s = settings(&h);
        install_all(&h, &s, &opts()).await.unwrap();
        let before = h.snapshot();
        let serial_before = serial(&h);
        selftest(&h, &release(), &s, &all_present).unwrap();
        assert_eq!(
            without_serial(h.snapshot()),
            without_serial(before),
            "the self-test leaves nothing behind"
        );
        assert_eq!(
            serial(&h),
            serial_before + 1,
            "but it did manufacture a TPM"
        );
    }

    #[tokio::test]
    async fn selftest_reports_every_failure() {
        let h = fake_host();
        let s = settings(&h);
        install_all(&h, &s, &opts()).await.unwrap();
        h.remove(Path::new("/dev/net/tun")).unwrap();
        h.run(&Cmd::new("usermod", &["-aG", "kvm", BANLIEUE_USER]))
            .unwrap();
        h.write(
            &release().firmware,
            b"drifted",
            0o644,
            &crate::ops::Owner::root(),
        )
        .unwrap();
        let no_bridge = |c: &HostConfig| HostFacts {
            bridges_present: Default::default(),
            ..all_present(c)
        };
        let Err(Error::Selftest(problems)) = selftest(&h, &release(), &s, &no_bridge) else {
            panic!("self-test passed");
        };
        let all = problems.join("\n");
        for expected in [
            "firmware does not match",
            "/dev/net/tun is missing",
            "banlieue is in the kvm group",
            "network class default",
        ] {
            assert!(all.contains(expected), "missing {expected:?} in:\n{all}");
        }
    }

    #[test]
    fn the_ek_cn_is_read_from_the_certificate() {
        assert_eq!(
            certificate_cn(crate::fake::SELFTEST_EK_DER).as_deref(),
            Some("banlieue-selftest:00000000-0000-4000-8000-000000000000")
        );
        assert_eq!(certificate_cn(b"not a certificate"), None);
    }

    // -------------------------------------------------- read-only and order

    /// Invariant 4: the read-only verbs write nothing.
    #[tokio::test]
    async fn read_only_verbs_write_nothing() {
        let h = fake_host();
        let s = settings(&h);
        install_all(&h, &s, &opts()).await.unwrap();
        let before: State = h.snapshot();
        let ran = h.commands().len();
        preflight(&h, &s).unwrap();
        status(&h);
        assert_eq!(h.snapshot(), before);
        assert_eq!(h.commands().len(), ran);
    }

    /// `--dry-run` looks at the host and changes nothing.
    #[tokio::test]
    async fn a_dry_run_changes_nothing() {
        let h = fake_host();
        let s = settings(&h);
        let before = h.snapshot();
        let dry = DryRun::new(&h);
        let o = Options {
            dry_run: true,
            ..Options::default()
        };
        install(&dry, &Canned::good(), &release(), &s, &o, &all_present)
            .await
            .unwrap();
        assert_eq!(h.snapshot(), before);
        assert!(h.commands().is_empty());
    }

    #[test]
    fn the_order_is_the_dag() {
        use Stage::*;
        assert_eq!(
            plan(&Options::default()),
            vec![Preflight, Vmm, Host, Tpm, Polkit, Provider, Selftest]
        );
        assert!(
            !plan(&Options {
                dry_run: true,
                ..Options::default()
            })
            .contains(&Selftest)
        );
        assert_eq!(
            plan(&Options {
                only: Some(Tpm),
                ..Options::default()
            }),
            vec![Tpm]
        );
    }

    /// `--only` refuses rather than half-applies.
    #[tokio::test]
    async fn only_a_stage_whose_prerequisites_are_missing_is_refused() {
        let h = fake_host();
        let s = settings(&h);
        let only = |stage| Options {
            only: Some(stage),
            ..Options::default()
        };
        match install_all(&h, &s, &only(Stage::Tpm)).await {
            Err(Error::Prerequisite { stage, missing }) => {
                assert_eq!(stage, "tpm");
                assert_eq!(missing, vec!["host".to_string()]);
            }
            other => panic!("{other:?}"),
        }
        let bare = FakeHost::debian();
        match install_all(&bare, &s, &only(Stage::Polkit)).await {
            Err(Error::Prerequisite { missing, .. }) => {
                assert_eq!(missing, vec!["preflight".to_string(), "host".to_string()]);
            }
            other => panic!("{other:?}"),
        }
        match install_all(&h, &s, &only(Stage::Selftest)).await {
            Err(Error::Prerequisite { missing, .. }) => {
                assert_eq!(missing, vec!["vmm", "host", "tpm", "polkit"]);
            }
            other => panic!("{other:?}"),
        }
        assert!(h.commands().is_empty(), "nothing ran");
        install_all(&h, &s, &only(Stage::Vmm)).await.unwrap();
    }

    /// ADR-0105: the slice is a root-owned unit beside the templates, with
    /// the budget the operator declared, and a re-run changes nothing.
    #[tokio::test]
    async fn the_guest_slice_is_installed_with_the_declared_budget() {
        let h = fake_host();
        let mut s = settings(&h);
        s.guest_slice.cpus = Some("0-3".into());
        s.guest_slice.memory_max = Some("64G".into());
        install_all(&h, &s, &opts()).await.unwrap();
        let path = format!("{SYSTEMD_UNIT_DIR}/banlieue-guests.slice");
        let text = String::from_utf8(h.read(Path::new(&path)).unwrap()).unwrap();
        assert!(text.contains("AllowedCPUs=0-3") && text.contains("MemoryMax=64G"));
        assert_eq!(mode_owner(&h, &path), (0o644, "root".into(), "root".into()));
        let before = h.read(Path::new(&path)).unwrap();
        install_all(&h, &s, &opts()).await.unwrap();
        assert_eq!(h.read(Path::new(&path)).unwrap(), before);
    }

    // ----------------------------------- ADR-0139: an old host, cgroup v1

    /// The first real host that is not systemd 252 or newer: systemd 239 on
    /// the legacy cgroup hierarchy, with the host's own package manager.
    fn el8_host() -> FakeHost {
        let h = fake_host();
        {
            let mut env = h.env.lock().unwrap();
            env.systemd_version = Some(239);
            env.cgroup_unified = false;
        }
        h
    }

    fn accepting_settings(h: &FakeHost, accepted: &[&str]) -> Settings {
        let args = crate::settings::HostArgs {
            guest_uid_count: Some(TEST_UID_COUNT),
            registry_keep_unreferenced: crate::settings::DEFAULT_KEEP_UNREFERENCED,
            accept_reduced_hardening: accepted.iter().map(ToString::to_string).collect(),
            ..crate::settings::HostArgs::default()
        };
        resolve(&args, h).unwrap()
    }

    fn unit_text(h: &FakeHost, name: &str) -> String {
        let path = format!("{SYSTEMD_UNIT_DIR}/{name}");
        String::from_utf8(h.read(Path::new(&path)).unwrap()).unwrap()
    }

    const GUEST_UNITS: [&str; 4] = [
        "banlieue-ch@.service",
        "banlieue-swtpm@.service",
        "banlieue-swtpm-setup@.service",
        "banlieue-ch-import@.service",
    ];

    #[tokio::test]
    async fn an_old_host_without_the_acceptance_is_refused_before_anything_is_written() {
        let h = el8_host();
        let s = accepting_settings(&h, &[]);
        let before = h.snapshot();
        let Err(Error::Preflight(problems)) = install_all(&h, &s, &opts()).await else {
            panic!("an old host was installed without the acceptance");
        };
        let all = problems.join("\n");
        assert!(all.contains("PrivateIPC"), "{all}");
        assert!(
            all.contains("--accept-reduced-hardening=PrivateIPC"),
            "{all}"
        );
        assert_eq!(h.snapshot(), before, "nothing was written");
        assert!(h.commands().is_empty(), "nothing ran");
    }

    /// The acceptance is in the host config `install` keeps, so a re-run
    /// without the flag installs and leaves the record as it was (ADR-0067
    /// keeps the config, ADR-0139 records the acceptance in it).
    #[tokio::test]
    async fn a_rerun_without_the_flag_keeps_the_recorded_acceptance() {
        let h = el8_host();
        let s = accepting_settings(&h, &["PrivateIPC"]);
        install_all(&h, &s, &opts()).await.unwrap();
        let before = h.read(Path::new(HOST_CONFIG)).unwrap();

        let rerun = resolve(&host_args(), &h).unwrap();
        assert_eq!(rerun.accepted_reduced, vec!["PrivateIPC".to_string()]);
        install_all(&h, &rerun, &opts()).await.unwrap();
        assert_eq!(h.read(Path::new(HOST_CONFIG)).unwrap(), before);
    }

    /// `--only provider` skips the preflight stage, and must not skip this.
    #[tokio::test]
    async fn a_single_stage_does_not_bypass_the_hardening_guard() {
        let h = el8_host();
        let s = accepting_settings(&h, &["PrivateIPC"]);
        install_all(&h, &s, &opts()).await.unwrap();
        // Settings that accept nothing, however they came about (a re-run
        // would take the kept acceptance, so build them directly).
        let mut bare = s.clone();
        bare.accepted_reduced.clear();
        let only = Options {
            only: Some(Stage::Provider),
            ..opts()
        };
        assert!(matches!(
            install_all(&h, &bare, &only).await,
            Err(Error::Preflight(_))
        ));
    }

    #[test]
    fn preflight_names_the_acceptance_an_old_host_needs() {
        let h = el8_host();
        let Err(Error::Preflight(problems)) = preflight(&h, &accepting_settings(&h, &[])) else {
            panic!("preflight passed");
        };
        assert!(
            problems.iter().any(|p| p.contains("PrivateIPC")),
            "{problems:?}"
        );
        preflight(&h, &accepting_settings(&h, &["PrivateIPC"])).unwrap();
    }

    /// An old host with the acceptance gets a complete install: files-mode
    /// accounts, units without the directive it cannot enforce, the
    /// acceptance recorded in the host config, and the self-test green.
    #[tokio::test]
    async fn an_old_host_installs_with_files_accounts_and_a_recorded_acceptance() {
        let h = el8_host();
        let s = accepting_settings(&h, &["PrivateIPC"]);
        assert_eq!(s.guest_accounts, crate::accounts::GuestAccounts::Files);
        install_all(&h, &s, &opts()).await.unwrap();

        assert!(
            !h.exists(Path::new("/etc/userdb")),
            "no userdb on this host"
        );
        for uid in s.uid_base..=s.uid_end() {
            let line = h.getent("passwd", &format!("banlieue-g{uid}")).unwrap();
            assert!(line.contains(&format!(":{uid}:{uid}:")), "{line}");
        }
        assert_eq!(count(&h, "useradd --system --uid"), TEST_UID_COUNT as usize);
        assert_eq!(
            count(&h, "groupadd --system --gid"),
            TEST_UID_COUNT as usize
        );

        for unit in GUEST_UNITS {
            let text = unit_text(&h, unit);
            assert!(!text.contains("PrivateIPC="), "{unit}");
            assert!(text.contains("ProtectSystem=strict"), "{unit}");
        }
        let config = HostConfig::parse(&String::from_utf8_lossy(
            &h.read(Path::new(HOST_CONFIG)).unwrap(),
        ))
        .unwrap();
        assert_eq!(
            config.accepted_reduced_hardening(),
            ["PrivateIPC".to_string()]
        );
    }

    #[tokio::test]
    async fn an_old_host_install_is_idempotent() {
        let h = el8_host();
        let s = accepting_settings(&h, &["PrivateIPC"]);
        install_all(&h, &s, &opts()).await.unwrap();
        let (st, ran) = (without_serial(h.snapshot()), h.commands().len());
        install_all(&h, &s, &opts()).await.unwrap();
        assert_eq!(without_serial(h.snapshot()), st);
        let new: Vec<String> = h.commands().into_iter().skip(ran).collect();
        assert!(
            new.iter()
                .all(|c| !c.starts_with("useradd") && !c.starts_with("groupadd")),
            "{new:?}"
        );
    }

    /// A new host keeps every directive and records no acceptance.
    #[tokio::test]
    async fn a_new_host_keeps_private_ipc_and_records_nothing() {
        let h = fake_host();
        let s = accepting_settings(&h, &[]);
        install_all(&h, &s, &opts()).await.unwrap();
        for unit in GUEST_UNITS {
            assert!(unit_text(&h, unit).contains("\nPrivateIPC=yes\n"), "{unit}");
        }
        let config = HostConfig::parse(&String::from_utf8_lossy(
            &h.read(Path::new(HOST_CONFIG)).unwrap(),
        ))
        .unwrap();
        assert!(config.hardening.is_none());
        assert!(h.exists(Path::new("/etc/userdb")));
        assert_eq!(count(&h, "groupadd"), 0);
    }

    /// The acceptance reaches an existing host config without `--force`
    /// (which would rotate the EK CA).
    #[tokio::test]
    async fn a_rerun_records_a_new_acceptance_without_force() {
        let h = fake_host();
        install_all(&h, &accepting_settings(&h, &[]), &opts())
            .await
            .unwrap();
        {
            let mut env = h.env.lock().unwrap();
            env.systemd_version = Some(239);
        }
        let s = accepting_settings(&h, &["PrivateIPC"]);
        let before = serial(&h);
        install_all(&h, &s, &opts()).await.unwrap();
        let config = HostConfig::parse(&String::from_utf8_lossy(
            &h.read(Path::new(HOST_CONFIG)).unwrap(),
        ))
        .unwrap();
        assert_eq!(
            config.accepted_reduced_hardening(),
            ["PrivateIPC".to_string()]
        );
        assert!(!unit_text(&h, "banlieue-ch@.service").contains("PrivateIPC="));
        // No EK CA rotation: only the self-test advanced the serial.
        assert_eq!(serial(&h), before + 1);
    }

    #[tokio::test]
    async fn an_old_host_dry_run_changes_nothing() {
        let h = el8_host();
        let s = accepting_settings(&h, &["PrivateIPC"]);
        let before = h.snapshot();
        let dry = DryRun::new(&h);
        let o = Options {
            dry_run: true,
            ..opts()
        };
        install(&dry, &Canned::good(), &release(), &s, &o, &all_present)
            .await
            .unwrap();
        assert_eq!(h.snapshot(), before);
        assert!(h.commands().is_empty());
    }

    /// The CPU quota reaches the slice on a host that cannot pin.
    #[tokio::test]
    async fn a_cpu_quota_is_installed_on_a_cgroup_v1_host() {
        let h = el8_host();
        let mut s = accepting_settings(&h, &["PrivateIPC"]);
        s.guest_slice.cpu_quota = Some("4800%".into());
        install_all(&h, &s, &opts()).await.unwrap();
        let slice = unit_text(&h, "banlieue-guests.slice");
        assert!(slice.contains("CPUQuota=4800%"), "{slice}");
        assert!(!slice.contains("AllowedCPUs"), "{slice}");
    }
}
