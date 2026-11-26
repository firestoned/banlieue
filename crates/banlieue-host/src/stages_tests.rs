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

    // ------------------------------- isolation preflight (ADR-0144)

    /// A host with bridge `br1` carrying one member port; `isolated` says
    /// whether the kernel exposes `brport/isolated` on it.
    fn host_with_port(isolated: bool) -> FakeHost {
        let h = FakeHost::debian();
        h.put_dir("/sys/class/net/br1/bridge");
        h.put_file("/sys/class/net/br1/brif/dummy0", b"", Kind::Other);
        if isolated {
            h.put_file("/sys/class/net/dummy0/brport/isolated", b"0\n", Kind::File);
        }
        h
    }

    /// `iptables -S` output of a filter table carrying the recipe.
    const GOOD_FILTER: &str = "-P INPUT ACCEPT\n-P FORWARD DROP\n-P OUTPUT ACCEPT\n\
        -N BANLIEUE_SBX_FORWARD\n-N BANLIEUE_SBX_INPUT\n\
        -A INPUT -j BANLIEUE_SBX_INPUT\n-A INPUT -i lo -j ACCEPT\n\
        -A FORWARD -j BANLIEUE_SBX_FORWARD\n-A FORWARD -j DOCKER-USER\n\
        -A BANLIEUE_SBX_INPUT -i br-sbx -j DROP\n";
    /// `iptables -t nat -S` output of a nat table carrying the recipe.
    const GOOD_NAT: &str = "-P PREROUTING ACCEPT\n-P POSTROUTING ACCEPT\n\
        -N BANLIEUE_SBX_NAT\n-A POSTROUTING -j BANLIEUE_SBX_NAT\n\
        -A POSTROUTING -s 192.0.2.0/24 -j MASQUERADE\n";

    /// [`host_with_port`] plus the recipe's rules in place.
    fn host_with_rules() -> FakeHost {
        let h = host_with_port(true);
        h.put_iptables("filter", GOOD_FILTER);
        h.put_iptables("nat", GOOD_NAT);
        h
    }

    fn isolating_settings(h: &FakeHost) -> Settings {
        let mut s = settings(h);
        s.network = vec![("sandbox".into(), "br1".into())];
        s.isolate_ports = vec!["sandbox".into()];
        s
    }

    fn preflight_problems(h: &FakeHost, s: &Settings) -> String {
        match preflight(h, s) {
            Ok(()) => String::new(),
            Err(Error::Preflight(p)) => p.join("\n"),
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn preflight_passes_an_isolated_class_when_the_kernel_can_isolate() {
        let h = host_with_rules();
        let p = preflight_problems(&h, &isolating_settings(&h));
        assert!(!p.contains("sandbox"), "{p}");
    }

    #[test]
    fn preflight_refuses_an_isolated_class_on_a_kernel_without_port_isolation() {
        let h = host_with_port(false);
        let p = preflight_problems(&h, &isolating_settings(&h));
        for expected in ["sandbox", "br1", "brport/isolated", "dummy0"] {
            assert!(p.contains(expected), "missing {expected:?} in:\n{p}");
        }
    }

    #[test]
    fn preflight_refuses_an_isolated_class_whose_bridge_has_no_port_to_probe() {
        let h = FakeHost::debian();
        h.put_dir("/sys/class/net/br1/bridge");
        let p = preflight_problems(&h, &isolating_settings(&h));
        for expected in ["sandbox", "br1", "no member port", "anchor"] {
            assert!(p.contains(expected), "missing {expected:?} in:\n{p}");
        }
    }

    #[test]
    fn preflight_does_not_probe_a_class_that_is_not_isolated() {
        let h = host_with_port(false);
        let mut s = isolating_settings(&h);
        s.isolate_ports.clear();
        let p = preflight_problems(&h, &s);
        assert!(!p.contains("brport"), "{p}");
        assert!(!p.contains("BANLIEUE_SBX"), "{p}");
        assert!(!p.contains("iptables"), "{p}");
    }

    #[test]
    fn preflight_refuses_an_isolated_class_when_the_firewall_cannot_be_read() {
        let h = host_with_port(true);
        let p = preflight_problems(&h, &isolating_settings(&h));
        for expected in ["sandbox", "iptables", "cannot be read", "filter", "nat"] {
            assert!(p.contains(expected), "missing {expected:?} in:\n{p}");
        }
    }

    #[test]
    fn preflight_refuses_when_one_table_cannot_be_read() {
        let h = host_with_port(true);
        h.put_iptables("filter", GOOD_FILTER);
        let p = preflight_problems(&h, &isolating_settings(&h));
        assert!(p.contains("nat table cannot be read"), "{p}");
        assert!(!p.contains("filter table"), "{p}");
    }

    #[test]
    fn preflight_refuses_each_absent_chain_by_name() {
        for (chain, table) in [
            ("BANLIEUE_SBX_INPUT", "filter"),
            ("BANLIEUE_SBX_FORWARD", "filter"),
            ("BANLIEUE_SBX_NAT", "nat"),
        ] {
            let h = host_with_rules();
            let strip = |text: &str| {
                text.lines()
                    .filter(|l| !l.contains(chain))
                    .map(|l| format!("{l}\n"))
                    .collect::<String>()
            };
            let (filter, nat) = (strip(GOOD_FILTER), strip(GOOD_NAT));
            h.put_iptables("filter", &filter);
            h.put_iptables("nat", &nat);
            let p = preflight_problems(&h, &isolating_settings(&h));
            for expected in ["sandbox", chain, "absent", table] {
                assert!(
                    p.contains(expected),
                    "{chain}: missing {expected:?} in:\n{p}"
                );
            }
        }
    }

    #[test]
    fn preflight_refuses_a_chain_that_nothing_jumps_to() {
        let h = host_with_rules();
        let filter = GOOD_FILTER.replace("-A FORWARD -j BANLIEUE_SBX_FORWARD\n", "");
        h.put_iptables("filter", &filter);
        let p = preflight_problems(&h, &isolating_settings(&h));
        for expected in ["sandbox", "no jump", "BANLIEUE_SBX_FORWARD", "FORWARD"] {
            assert!(p.contains(expected), "missing {expected:?} in:\n{p}");
        }
        assert!(!p.contains("BANLIEUE_SBX_INPUT"), "{p}");
    }

    #[test]
    fn preflight_refuses_a_jump_that_is_not_first_in_its_hook() {
        let h = host_with_rules();
        let nat = GOOD_NAT.replace(
            "-A POSTROUTING -j BANLIEUE_SBX_NAT\n",
            "-A POSTROUTING -j KUBE-POSTROUTING\n-A POSTROUTING -j BANLIEUE_SBX_NAT\n",
        );
        h.put_iptables("nat", &nat);
        let p = preflight_problems(&h, &isolating_settings(&h));
        for expected in ["sandbox", "not first", "POSTROUTING", "BANLIEUE_SBX_NAT"] {
            assert!(p.contains(expected), "missing {expected:?} in:\n{p}");
        }
    }

    #[test]
    fn preflight_refuses_a_jump_when_its_hook_has_other_rules_first_even_if_a_later_one_matches() {
        let h = host_with_rules();
        let filter = GOOD_FILTER.replace(
            "-A INPUT -j BANLIEUE_SBX_INPUT\n-A INPUT -i lo -j ACCEPT\n",
            "-A INPUT -i lo -j ACCEPT\n-A INPUT -j BANLIEUE_SBX_INPUT\n",
        );
        h.put_iptables("filter", &filter);
        let p = preflight_problems(&h, &isolating_settings(&h));
        assert!(
            p.contains("not first") && p.contains("BANLIEUE_SBX_INPUT"),
            "{p}"
        );
    }

    #[test]
    fn preflight_does_not_mistake_a_longer_chain_name_for_ours() {
        let h = host_with_rules();
        let filter = GOOD_FILTER.replace("-N BANLIEUE_SBX_INPUT\n", "-N BANLIEUE_SBX_INPUT2\n");
        h.put_iptables("filter", &filter);
        let p = preflight_problems(&h, &isolating_settings(&h));
        assert!(
            p.contains("BANLIEUE_SBX_INPUT") && p.contains("absent"),
            "{p}"
        );
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
            vec![
                Preflight,
                Vmm,
                Host,
                Attestation,
                Tpm,
                Polkit,
                Provider,
                Selftest
            ]
        );
        assert_eq!(
            plan(&Options {
                only: Some(Host),
                ..Options::default()
            }),
            vec![Host, Attestation],
            "--only host carries the attestation stage, like the section it writes"
        );
        assert_eq!(
            plan(&Options {
                only: Some(Attestation),
                ..Options::default()
            }),
            vec![Attestation]
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

    // ------------------------------------------------- attestation (ADR-0146)

    const CA_DIR: &str = "/var/lib/banlieue/attestation-ca";
    const CA_KEY: &str = "/var/lib/banlieue/attestation-ca/ca.key";
    const CA_CRT: &str = "/var/lib/banlieue/attestation-ca/ca.crt";

    fn attest_settings(h: &FakeHost, extra: &[&str]) -> Settings {
        let mut a = host_args();
        for pair in extra.chunks(2) {
            match pair[0] {
                "name" => a.attestation_ca_name = Some(pair[1].into()),
                "ttl" => a.attestation_certificate_ttl = Some(pair[1].into()),
                "on" => a.attestation = true,
                "rotate" => a.rotate_attestation_ca = true,
                "csr" => a.attestation_ca_csr = true,
                "import" => a.attestation_ca_import = Some(pair[1].into()),
                other => panic!("{other}"),
            }
        }
        resolve(&a, h).unwrap()
    }

    fn config_text(h: &FakeHost) -> String {
        String::from_utf8(h.read(Path::new(HOST_CONFIG)).unwrap()).unwrap()
    }

    fn ca_crt(h: &FakeHost) -> Vec<u8> {
        h.read(Path::new(CA_CRT)).unwrap()
    }

    fn force() -> Options {
        Options {
            force: true,
            ..Options::default()
        }
    }

    #[tokio::test]
    async fn the_ca_is_created_as_the_provider_and_the_section_written() {
        let h = fake_host();
        let s = attest_settings(&h, &["name", "bar", "ttl", "2h"]);
        install_all(&h, &s, &opts()).await.unwrap();
        assert_eq!(
            mode_owner(&h, CA_DIR),
            (0o700, "banlieue".into(), "banlieue".into())
        );
        assert_eq!(
            mode_owner(&h, CA_KEY),
            (0o600, "banlieue".into(), "banlieue".into())
        );
        assert_eq!(
            mode_owner(&h, CA_CRT),
            (0o644, "banlieue".into(), "banlieue".into())
        );
        assert_eq!(h.attestation_ca(Path::new(CA_DIR)).unwrap().name, "bar");
        let text = config_text(&h);
        assert!(
            text.contains(
                "[attestation]\nca_dir = \"/var/lib/banlieue/attestation-ca\"\ncertificate_ttl_secs = 7200\n"
            ),
            "{text}"
        );
        let loaded = HostConfig::parse(&text).unwrap().attestation.unwrap();
        assert_eq!(loaded.certificate_ttl_secs, 7200);
        assert!(
            count(&h, "(as banlieue) swtpm_setup") >= 1,
            "the vTPM path is untouched"
        );
    }

    /// Not opted in: no section, no directory.
    #[tokio::test]
    async fn a_host_that_did_not_opt_in_gets_no_section_and_no_ca() {
        let h = fake_host();
        install_all(&h, &settings(&h), &opts()).await.unwrap();
        assert!(!config_text(&h).contains("[attestation]"));
        assert!(!h.exists(Path::new(CA_DIR)));
    }

    #[tokio::test]
    async fn a_rerun_is_a_no_op_and_keeps_the_ca() {
        let h = fake_host();
        let s = attest_settings(&h, &["name", "bar"]);
        install_all(&h, &s, &opts()).await.unwrap();
        let (cert, key) = (ca_crt(&h), h.read(Path::new(CA_KEY)).unwrap());
        let before = without_serial(h.snapshot());
        let again = attest_settings(&h, &["name", "bar"]);
        install_all(&h, &again, &opts()).await.unwrap();
        assert_eq!(without_serial(h.snapshot()), before);
        assert_eq!(
            (ca_crt(&h), h.read(Path::new(CA_KEY)).unwrap()),
            (cert, key)
        );
    }

    /// The kept-config rule: with no flag, an existing section and CA stay.
    #[tokio::test]
    async fn an_existing_section_is_kept_when_not_asked_about_again() {
        let h = fake_host();
        install_all(
            &h,
            &attest_settings(&h, &["name", "bar", "ttl", "2h"]),
            &opts(),
        )
        .await
        .unwrap();
        let before = without_serial(h.snapshot());
        install_all(&h, &settings(&h), &opts()).await.unwrap();
        assert_eq!(without_serial(h.snapshot()), before);
        assert!(config_text(&h).contains("certificate_ttl_secs = 7200"));
    }

    #[tokio::test]
    async fn a_new_ttl_changes_the_section_and_not_the_ca() {
        let h = fake_host();
        install_all(&h, &attest_settings(&h, &["name", "bar"]), &opts())
            .await
            .unwrap();
        let cert = ca_crt(&h);
        install_all(&h, &attest_settings(&h, &["ttl", "30m"]), &opts())
            .await
            .unwrap();
        assert!(config_text(&h).contains("certificate_ttl_secs = 1800"));
        assert_eq!(ca_crt(&h), cert);
    }

    // ------------------------------- the CSR route (ADR-0146 Decision 7)

    const PENDING_DIR: &str = "/var/lib/banlieue/attestation-ca-pending";
    const PENDING_KEY: &str = "/var/lib/banlieue/attestation-ca-pending/ca.key";
    const PENDING_CSR: &str = "/var/lib/banlieue/attestation-ca-pending/ca.csr";
    const SIGNED: &str = "/root/signed.pem";

    /// What an administrator's PKI would send back for the pending request.
    fn signed_for_pending(h: &FakeHost) -> Vec<u8> {
        let csr = String::from_utf8(h.read(Path::new(PENDING_CSR)).unwrap()).unwrap();
        banlieue_attestation_ca::testing::upstream_sign_csr(&csr, std::time::SystemTime::now())
            .into_bytes()
    }

    fn put_signed(h: &FakeHost, bytes: &[u8]) {
        h.put_dir("/root");
        h.put_file(SIGNED, bytes, crate::ops::Kind::File);
    }

    #[tokio::test]
    async fn the_csr_route_writes_a_request_and_neither_a_certificate_nor_a_section() {
        let h = fake_host();
        let s = attest_settings(&h, &["csr", "1", "name", "bar"]);
        install_all(&h, &s, &opts()).await.unwrap();
        assert_eq!(
            mode_owner(&h, PENDING_DIR),
            (0o700, "banlieue".into(), "banlieue".into())
        );
        assert_eq!(
            mode_owner(&h, PENDING_KEY),
            (0o600, "banlieue".into(), "banlieue".into())
        );
        assert_eq!(
            mode_owner(&h, PENDING_CSR),
            (0o644, "banlieue".into(), "banlieue".into())
        );
        assert!(!h.exists(Path::new(CA_CRT)), "no certificate yet");
        assert!(!h.exists(Path::new(CA_KEY)), "the live CA is not touched");
        assert!(
            !config_text(&h).contains("[attestation]"),
            "an unsigned request must not look configured, or the provider would stop"
        );
    }

    #[tokio::test]
    async fn importing_the_signed_certificate_installs_the_ca_and_writes_the_section() {
        let h = fake_host();
        install_all(
            &h,
            &attest_settings(&h, &["csr", "1", "name", "bar"]),
            &opts(),
        )
        .await
        .unwrap();
        put_signed(&h, &signed_for_pending(&h));

        let s = attest_settings(&h, &["import", SIGNED, "ttl", "2h"]);
        install_all(&h, &s, &opts()).await.unwrap();

        assert_eq!(h.attestation_ca(Path::new(CA_DIR)).unwrap().name, "bar");
        assert_eq!(
            mode_owner(&h, CA_KEY),
            (0o600, "banlieue".into(), "banlieue".into())
        );
        assert_eq!(
            mode_owner(&h, CA_CRT),
            (0o644, "banlieue".into(), "banlieue".into())
        );
        assert!(
            !h.exists(Path::new(PENDING_KEY)),
            "the key moved, not copied"
        );
        assert!(config_text(&h).contains("certificate_ttl_secs = 7200"));
        assert!(h.exists(Path::new("/var/lib/banlieue/attestation-ca/ca-chain.pem")));
    }

    #[tokio::test]
    async fn a_refused_certificate_installs_nothing_and_leaves_the_request() {
        let h = fake_host();
        install_all(
            &h,
            &attest_settings(&h, &["csr", "1", "name", "bar"]),
            &opts(),
        )
        .await
        .unwrap();
        put_signed(&h, b"not a certificate");

        let s = attest_settings(&h, &["import", SIGNED]);
        let err = install_all(&h, &s, &opts()).await.err().unwrap();

        assert!(err.to_string().contains("rejected"), "{err}");
        assert!(!h.exists(Path::new(CA_CRT)));
        assert!(h.exists(Path::new(PENDING_KEY)), "the request stays");
        assert!(!config_text(&h).contains("[attestation]"));
    }

    #[tokio::test]
    async fn an_unreadable_signed_file_is_an_error_not_a_panic() {
        let h = fake_host();
        install_all(
            &h,
            &attest_settings(&h, &["csr", "1", "name", "bar"]),
            &opts(),
        )
        .await
        .unwrap();
        let s = attest_settings(&h, &["import", "/root/missing.pem"]);
        assert!(install_all(&h, &s, &opts()).await.is_err());
        assert!(h.exists(Path::new(PENDING_KEY)));
    }

    #[tokio::test]
    async fn rotating_by_csr_keeps_the_live_ca_serving_until_the_import() {
        let h = fake_host();
        install_all(&h, &attest_settings(&h, &["name", "bar"]), &opts())
            .await
            .unwrap();
        let (cert, key) = (ca_crt(&h), h.read(Path::new(CA_KEY)).unwrap());

        let s = attest_settings(&h, &["csr", "1", "rotate", "1", "name", "baz"]);
        install_all(&h, &s, &opts()).await.unwrap();
        assert_eq!(
            (ca_crt(&h), h.read(Path::new(CA_KEY)).unwrap()),
            (cert, key),
            "the live CA is untouched while a request is pending"
        );
        assert!(h.exists(Path::new(PENDING_KEY)));

        put_signed(&h, &signed_for_pending(&h));
        let s = attest_settings(&h, &["import", SIGNED, "rotate", "1"]);
        install_all(&h, &s, &opts()).await.unwrap();
        assert_eq!(h.attestation_ca(Path::new(CA_DIR)).unwrap().name, "baz");
        assert!(!h.exists(Path::new(PENDING_KEY)));
    }

    // ------------------------------ OpenBao signing (ADR-0146 Decision 8)

    use crate::error::Error as HostError;
    use crate::openbao::{Credential, Pki, SignRequest};
    use crate::settings::{CaRoute as Route, OpenBaoAuth};

    const SA_TOKEN_PATH: &str = "/etc/banlieue/credentials/token";
    const SA_TOKEN: &str = "sa.jwt.value\n";
    const BAO_TOKEN_FILE: &str = "/root/bao.token";

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Answer {
        Good,
        Refuse,
        Hostile,
    }

    /// What the installer asked, and a scripted answer.
    struct CannedPki {
        answer: std::sync::Mutex<Answer>,
        asked: std::sync::Mutex<Vec<(String, String, String, Credential)>>,
    }

    impl CannedPki {
        fn new(answer: Answer) -> Self {
            Self {
                answer: std::sync::Mutex::new(answer),
                asked: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn set(&self, answer: Answer) {
            *self.answer.lock().unwrap() = answer;
        }

        fn asked(&self) -> Vec<(String, String, String, Credential)> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Pki for CannedPki {
        async fn sign_intermediate(&self, req: &SignRequest<'_>) -> Result<String, HostError> {
            self.asked.lock().unwrap().push((
                req.csr_pem.to_string(),
                req.common_name.to_string(),
                req.ttl.to_string(),
                req.credential.clone(),
            ));
            match *self.answer.lock().unwrap() {
                Answer::Good => Ok(banlieue_attestation_ca::testing::upstream_sign_csr(
                    req.csr_pem,
                    std::time::SystemTime::now(),
                )),
                Answer::Refuse => Err(HostError::Pki("login refused (HTTP 403)".into())),
                Answer::Hostile => {
                    Ok("-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n".into())
                }
            }
        }
    }

    async fn install_via(
        h: &FakeHost,
        pki: &dyn Pki,
        s: &Settings,
        o: &Options,
    ) -> Result<(), HostError> {
        install_with_pki(h, &Canned::good(), pki, &release(), s, o, &all_present).await
    }

    fn bao_settings(h: &FakeHost, extra: &[&str]) -> Settings {
        let mut a = host_args();
        a.attestation_ca_name = Some("bar".into());
        a.attestation_ca_openbao_addr = Some("https://bao.example.com:8200".into());
        a.attestation_ca_openbao_mount = Some("pki-node-attestation".into());
        a.attestation_ca_openbao_auth_mount = Some("computefabric-test-v2".into());
        a.attestation_ca_openbao_role = Some("ch-host-ca-bar".into());
        for pair in extra.chunks(2) {
            match pair[0] {
                "name" => a.attestation_ca_name = Some(pair[1].into()),
                "rotate" => a.rotate_attestation_ca = true,
                "token-file" => {
                    a.attestation_ca_openbao_auth_mount = None;
                    a.attestation_ca_openbao_role = None;
                    a.attestation_ca_openbao_token_file = Some(pair[1].into());
                }
                other => panic!("{other}"),
            }
        }
        resolve(&a, h).unwrap()
    }

    fn with_sa_token(h: &FakeHost) {
        h.put_file(SA_TOKEN_PATH, SA_TOKEN.as_bytes(), crate::ops::Kind::File);
    }

    /// A host that has been installed once (so the credentials directory exists).
    async fn installed(h: &FakeHost) {
        install_all(h, &settings(h), &opts()).await.unwrap();
    }

    #[tokio::test]
    async fn the_openbao_route_logs_in_as_the_service_account_and_installs_the_signed_ca() {
        let h = fake_host();
        installed(&h).await;
        with_sa_token(&h);
        let pki = CannedPki::new(Answer::Good);

        install_via(&h, &pki, &bao_settings(&h, &[]), &opts())
            .await
            .unwrap();

        let asked = pki.asked();
        assert_eq!(asked.len(), 1);
        let (csr, cn, ttl, cred) = &asked[0];
        assert!(csr.contains("BEGIN CERTIFICATE REQUEST"), "{csr}");
        assert_eq!((cn.as_str(), ttl.as_str()), ("bar", "8760h"));
        assert_eq!(
            *cred,
            Credential::Jwt {
                auth_mount: "computefabric-test-v2".into(),
                role: "ch-host-ca-bar".into(),
                jwt: "sa.jwt.value".into(), // trimmed
            }
        );
        assert_eq!(h.attestation_ca(Path::new(CA_DIR)).unwrap().name, "bar");
        assert_eq!(
            mode_owner(&h, CA_KEY),
            (0o600, "banlieue".into(), "banlieue".into())
        );
        assert!(!h.exists(Path::new(PENDING_KEY)), "the request is consumed");
        assert!(config_text(&h).contains("[attestation]"));
    }

    #[tokio::test]
    async fn without_the_service_account_token_the_request_stays_and_the_fix_is_named() {
        let h = fake_host();
        installed(&h).await;
        let pki = CannedPki::new(Answer::Good);

        let err = install_via(&h, &pki, &bao_settings(&h, &[]), &opts())
            .await
            .err()
            .unwrap();

        let text = err.to_string();
        assert!(
            text.contains("bootstrap") && text.contains(SA_TOKEN_PATH),
            "{text}"
        );
        assert!(pki.asked().is_empty(), "no request without an identity");
        assert!(h.exists(Path::new(PENDING_KEY)), "the key is not wasted");
        assert!(!h.exists(Path::new(CA_CRT)));
        assert!(!config_text(&h).contains("[attestation]"));
    }

    #[tokio::test]
    async fn an_empty_or_multi_line_service_account_token_is_refused() {
        for bad in ["", "   \n", "a\nb\n"] {
            let h = fake_host();
            installed(&h).await;
            h.put_file(SA_TOKEN_PATH, bad.as_bytes(), crate::ops::Kind::File);
            let pki = CannedPki::new(Answer::Good);
            assert!(
                install_via(&h, &pki, &bao_settings(&h, &[]), &opts())
                    .await
                    .is_err(),
                "{bad:?}"
            );
            assert!(pki.asked().is_empty(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn a_refused_signing_installs_nothing_and_a_rerun_signs_the_same_request() {
        let h = fake_host();
        installed(&h).await;
        with_sa_token(&h);
        let pki = CannedPki::new(Answer::Refuse);
        assert!(
            install_via(&h, &pki, &bao_settings(&h, &[]), &opts())
                .await
                .is_err()
        );
        assert!(!h.exists(Path::new(CA_CRT)));
        assert!(h.exists(Path::new(PENDING_KEY)));
        assert!(!config_text(&h).contains("[attestation]"));

        pki.set(Answer::Good);
        install_via(&h, &pki, &bao_settings(&h, &[]), &opts())
            .await
            .unwrap();
        let asked = pki.asked();
        assert_eq!(asked.len(), 2);
        assert_eq!(asked[0].0, asked[1].0, "the same request, not a new key");
        assert_eq!(h.attestation_ca(Path::new(CA_DIR)).unwrap().name, "bar");
    }

    #[tokio::test]
    async fn a_hostile_answer_is_refused_by_the_import_and_leaves_the_request() {
        let h = fake_host();
        installed(&h).await;
        with_sa_token(&h);
        let pki = CannedPki::new(Answer::Hostile);
        let err = install_via(&h, &pki, &bao_settings(&h, &[]), &opts())
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("rejected"), "{err}");
        assert!(!h.exists(Path::new(CA_CRT)));
        assert!(h.exists(Path::new(PENDING_KEY)));
        assert!(!config_text(&h).contains("[attestation]"));
    }

    #[tokio::test]
    async fn the_token_file_route_sends_the_token_and_removes_the_file() {
        let h = fake_host();
        installed(&h).await;
        h.put_dir("/root");
        h.put_file(BAO_TOKEN_FILE, b"s.one-use\n", crate::ops::Kind::File);
        let pki = CannedPki::new(Answer::Good);

        install_via(
            &h,
            &pki,
            &bao_settings(&h, &["token-file", BAO_TOKEN_FILE]),
            &opts(),
        )
        .await
        .unwrap();

        assert_eq!(pki.asked()[0].3, Credential::Token("s.one-use".into()));
        assert!(
            !h.exists(Path::new(BAO_TOKEN_FILE)),
            "a spent token is not left lying"
        );
        assert_eq!(h.attestation_ca(Path::new(CA_DIR)).unwrap().name, "bar");
    }

    #[tokio::test]
    async fn the_token_file_is_removed_even_when_the_signing_fails() {
        let h = fake_host();
        installed(&h).await;
        h.put_dir("/root");
        h.put_file(BAO_TOKEN_FILE, b"s.one-use\n", crate::ops::Kind::File);
        let pki = CannedPki::new(Answer::Refuse);
        assert!(
            install_via(
                &h,
                &pki,
                &bao_settings(&h, &["token-file", BAO_TOKEN_FILE]),
                &opts()
            )
            .await
            .is_err()
        );
        assert!(!h.exists(Path::new(BAO_TOKEN_FILE)));
        assert!(h.exists(Path::new(PENDING_KEY)));
    }

    #[tokio::test]
    async fn a_missing_token_file_is_an_error_naming_it() {
        let h = fake_host();
        installed(&h).await;
        let pki = CannedPki::new(Answer::Good);
        let err = install_via(
            &h,
            &pki,
            &bao_settings(&h, &["token-file", BAO_TOKEN_FILE]),
            &opts(),
        )
        .await
        .err()
        .unwrap();
        assert!(err.to_string().contains(BAO_TOKEN_FILE), "{err}");
        assert!(pki.asked().is_empty());
    }

    #[tokio::test]
    async fn rotating_by_openbao_replaces_the_live_ca_under_a_new_name() {
        let h = fake_host();
        install_all(&h, &attest_settings(&h, &["name", "old"]), &opts())
            .await
            .unwrap();
        with_sa_token(&h);
        let pki = CannedPki::new(Answer::Good);
        let s = bao_settings(&h, &["name", "bar", "rotate", "1"]);
        install_via(&h, &pki, &s, &opts()).await.unwrap();
        assert_eq!(h.attestation_ca(Path::new(CA_DIR)).unwrap().name, "bar");
    }

    #[tokio::test]
    async fn a_dry_run_asks_nobody_and_changes_nothing() {
        let h = fake_host();
        installed(&h).await;
        with_sa_token(&h);
        let before = without_serial(h.state.lock().unwrap().clone());
        let pki = CannedPki::new(Answer::Good);
        let dry = DryRun::new(&h);
        let o = Options {
            dry_run: true,
            ..Options::default()
        };
        install_with_pki(
            &dry,
            &Canned::good(),
            &pki,
            &release(),
            &bao_settings(&h, &[]),
            &o,
            &all_present,
        )
        .await
        .unwrap();
        assert!(pki.asked().is_empty(), "a dry run makes no network call");
        assert_eq!(before, without_serial(h.state.lock().unwrap().clone()));
        let _ = Route::SelfSigned;
        let _ = OpenBaoAuth::TokenFile("x".into());
    }

    #[tokio::test]
    async fn the_default_install_has_no_pki_and_says_so() {
        let h = fake_host();
        installed(&h).await;
        with_sa_token(&h);
        // `install` (not `install_with_pki`) carries no OpenBao client.
        let err = install(
            &h,
            &Canned::good(),
            &release(),
            &bao_settings(&h, &[]),
            &opts(),
            &all_present,
        )
        .await
        .err()
        .unwrap();
        assert!(err.to_string().contains("OpenBao"), "{err}");
        assert!(h.exists(Path::new(PENDING_KEY)));
    }

    #[tokio::test]
    async fn a_plain_rerun_ignores_a_pending_request() {
        let h = fake_host();
        install_all(
            &h,
            &attest_settings(&h, &["csr", "1", "name", "bar"]),
            &opts(),
        )
        .await
        .unwrap();
        let before = h.read(Path::new(PENDING_KEY)).unwrap();
        install_all(&h, &settings(&h), &opts()).await.unwrap();
        assert_eq!(before, h.read(Path::new(PENDING_KEY)).unwrap());
        assert!(!h.exists(Path::new(CA_CRT)));
    }

    /// `--force` regenerates the host config and rotates the EK CA; it must
    /// not touch the attestation CA, and the section survives.
    #[tokio::test]
    async fn force_does_not_rotate_the_attestation_ca() {
        let h = fake_host();
        install_all(
            &h,
            &attest_settings(&h, &["name", "bar", "ttl", "2h"]),
            &opts(),
        )
        .await
        .unwrap();
        let (cert, key) = (ca_crt(&h), h.read(Path::new(CA_KEY)).unwrap());
        let fresh = crate::settings::resolve_fresh(&host_args(), &h).unwrap();
        install_all(&h, &fresh, &force()).await.unwrap();
        assert_eq!(
            (ca_crt(&h), h.read(Path::new(CA_KEY)).unwrap()),
            (cert, key)
        );
        assert!(config_text(&h).contains("certificate_ttl_secs = 7200"));
        assert_eq!(h.attestation_ca(Path::new(CA_DIR)).unwrap().name, "bar");
    }

    #[tokio::test]
    async fn rotation_makes_a_new_ca_under_a_new_name() {
        let h = fake_host();
        install_all(&h, &attest_settings(&h, &["name", "bar"]), &opts())
            .await
            .unwrap();
        let (cert, key) = (ca_crt(&h), h.read(Path::new(CA_KEY)).unwrap());
        let s = attest_settings(&h, &["rotate", "", "name", "bar-2"]);
        install_all(&h, &s, &opts()).await.unwrap();
        assert_eq!(h.attestation_ca(Path::new(CA_DIR)).unwrap().name, "bar-2");
        assert_ne!(ca_crt(&h), cert);
        assert_ne!(h.read(Path::new(CA_KEY)).unwrap(), key);
        assert_eq!(
            mode_owner(&h, CA_KEY),
            (0o600, "banlieue".into(), "banlieue".into())
        );
    }

    /// A symlink planted in place of the old key makes the current CA
    /// unreadable, so a rotation is refused before anything is removed or
    /// written, and the link's target is left alone. (The unlinking itself,
    /// `remove_attestation_ca`, is tested in `attestation_tests.rs` and in the
    /// CA crate.)
    #[tokio::test]
    async fn rotation_is_refused_when_a_symlink_stands_in_place_of_the_old_key() {
        let h = fake_host();
        install_all(&h, &attest_settings(&h, &["name", "bar"]), &opts())
            .await
            .unwrap();
        h.put_file("/var/lib/banlieue/victim", b"victim", Kind::File);
        h.put_file(
            CA_KEY,
            b"",
            Kind::Symlink(PathBuf::from("/var/lib/banlieue/victim")),
        );
        let mut a = host_args();
        a.attestation_ca_name = Some("bar-2".into());
        a.rotate_attestation_ca = true;
        let err = resolve(&a, &h).unwrap_err().to_string();
        assert!(err.contains("symlink"), "{err}");
        assert_eq!(
            h.read(Path::new("/var/lib/banlieue/victim")).unwrap(),
            b"victim"
        );
    }

    #[tokio::test]
    async fn only_host_creates_the_ca_too() {
        let h = fake_host();
        let s = attest_settings(&h, &["name", "bar"]);
        let only = Options {
            only: Some(Stage::Host),
            ..Options::default()
        };
        install_all(&h, &s, &only).await.unwrap();
        assert!(h.exists(Path::new(CA_CRT)));
        assert!(config_text(&h).contains("[attestation]"));
    }

    #[tokio::test]
    async fn only_attestation_needs_the_host_stage_first() {
        let h = fake_host();
        let s = attest_settings(&h, &["name", "bar"]);
        let only = Options {
            only: Some(Stage::Attestation),
            ..Options::default()
        };
        match install_all(&h, &s, &only).await {
            Err(Error::Prerequisite { stage, missing }) => {
                assert_eq!(stage, "attestation");
                assert_eq!(missing, vec!["host".to_string()]);
            }
            other => panic!("{other:?}"),
        }
        install_all(&h, &s, &Options::default()).await.unwrap();
        install_all(&h, &s, &only).await.unwrap();
    }

    #[tokio::test]
    async fn dry_run_shows_the_plan_and_changes_nothing() {
        let h = fake_host();
        let s = attest_settings(&h, &["name", "bar"]);
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
        assert!(!h.exists(Path::new(CA_DIR)));
    }

    // preflight and status

    async fn opted_in_host() -> FakeHost {
        let h = fake_host();
        install_all(&h, &attest_settings(&h, &["name", "bar"]), &opts())
            .await
            .unwrap();
        h
    }

    #[tokio::test]
    async fn preflight_passes_with_a_sound_ca_and_reports_it() {
        let h = opted_in_host().await;
        preflight(&h, &attest_settings(&h, &[])).unwrap();
        let info = attestation_state(&h).unwrap().unwrap();
        assert_eq!(info.name, "bar");
        assert!(!info.not_after.is_empty());
        assert!(attestation_status(&h).contains("bar, expires"));
    }

    fn refused(h: &FakeHost) -> String {
        let Err(Error::Preflight(problems)) = preflight(h, &attest_settings(h, &[])) else {
            panic!("preflight passed with a bad attestation CA");
        };
        problems.join("\n")
    }

    #[tokio::test]
    async fn preflight_refuses_a_key_that_is_not_0600() {
        let h = opted_in_host().await;
        h.set_mode(Path::new(CA_KEY), 0o640).unwrap();
        assert!(refused(&h).contains("0600"));
        assert!(attestation_status(&h).starts_with("REFUSED"));
    }

    #[tokio::test]
    async fn preflight_refuses_a_directory_open_to_group_or_world() {
        let h = opted_in_host().await;
        h.set_mode(Path::new(CA_DIR), 0o750).unwrap();
        assert!(refused(&h).contains("group or world"));
    }

    #[tokio::test]
    async fn preflight_refuses_a_key_not_owned_by_the_providers_user() {
        let h = opted_in_host().await;
        h.state
            .lock()
            .unwrap()
            .fs
            .get_mut(Path::new(CA_KEY))
            .unwrap()
            .owner = crate::ops::Owner::root();
        let problems = refused(&h);
        assert!(
            problems.contains("key") && problems.contains("owned by root"),
            "{problems}"
        );
    }

    #[tokio::test]
    async fn preflight_refuses_a_directory_not_owned_by_the_providers_user() {
        let h = opted_in_host().await;
        h.state
            .lock()
            .unwrap()
            .fs
            .get_mut(Path::new(CA_DIR))
            .unwrap()
            .owner = crate::ops::Owner::root();
        assert!(refused(&h).contains("directory"));
    }

    #[tokio::test]
    async fn status_reports_no_ca_before_opting_in() {
        let h = fake_host();
        install_all(&h, &settings(&h), &opts()).await.unwrap();
        assert!(attestation_status(&h).starts_with("none"));
        assert!(attestation_state(&h).is_none());
    }
}
