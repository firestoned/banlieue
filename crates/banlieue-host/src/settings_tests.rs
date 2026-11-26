// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `settings.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::fake::FakeHost;
    use crate::ops::Kind;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        args: HostArgs,
    }

    fn args(argv: &[&str]) -> HostArgs {
        T::parse_from(std::iter::once("t").chain(argv.iter().copied())).args
    }

    /// A bare host: storage on the roomiest candidate, the libvirt bridge
    /// if there is one, the host's name, the documented uid range.
    #[test]
    fn defaults_come_from_the_host() {
        let h = FakeHost::debian();
        h.put_dir("/data");
        {
            let mut env = h.env.lock().unwrap();
            env.free.insert("/data".into(), 900 << 30);
        }
        let s = resolve(&args(&[]), &h).unwrap();
        assert_eq!(s.provider_name, "bar");
        assert_eq!(s.namespace, DEFAULT_NAMESPACE);
        assert_eq!(
            s.storage,
            vec![("default".into(), PathBuf::from("/data/banlieue/ch"))]
        );
        assert_eq!(s.network, vec![("default".into(), "virbr0".into())]);
        assert_eq!(
            (s.uid_base, s.uid_count),
            (DEFAULT_GUEST_UID_BASE, DEFAULT_GUEST_UID_COUNT)
        );
        assert_eq!(
            s.uid_end(),
            DEFAULT_GUEST_UID_BASE + DEFAULT_GUEST_UID_COUNT - 1
        );
        assert!(s.registry.is_none());
    }

    #[test]
    fn no_bridge_means_no_default_network_class() {
        let h = FakeHost::debian();
        h.state
            .lock()
            .unwrap()
            .fs
            .retain(|p, _| !p.starts_with("/sys/class/net"));
        assert!(resolve(&args(&[]), &h).unwrap().network.is_empty());
    }

    #[test]
    fn classes_are_taken_in_order_from_flags_or_a_list() {
        let h = FakeHost::debian();
        let s = resolve(
            &args(&[
                "--storage-class",
                "fast=/nvme/ch,slow=/srv/ch",
                "--network-class",
                "lan=br0",
                "--provider-name",
                "baz",
                "--registry-repository",
                "registry.internal:5000/banlieue/disks",
            ]),
            &h,
        )
        .unwrap();
        assert_eq!(
            s.storage,
            vec![
                ("fast".into(), PathBuf::from("/nvme/ch")),
                ("slow".into(), PathBuf::from("/srv/ch"))
            ]
        );
        assert_eq!(s.network, vec![("lan".into(), "br0".into())]);
        assert_eq!(s.provider_name, "baz");
        let r = s.registry.unwrap();
        assert_eq!(r.repository, "registry.internal:5000/banlieue/disks");
        assert_eq!(r.keep_unreferenced, DEFAULT_KEEP_UNREFERENCED);
        assert!(!r.plain_http);
    }

    /// Values end up in a TOML file and in unit files: anything that could
    /// break either is refused, not quoted.
    #[test]
    fn unusable_values_are_refused() {
        let h = FakeHost::debian();
        for bad in [
            vec!["--storage-class", "nopath"],
            vec!["--storage-class", "default=relative/dir"],
            vec!["--storage-class", "Upper=/srv/x"],
            vec!["--storage-class", "a=/srv/x,a=/srv/y"],
            vec!["--network-class", "lan=br 0"],
            vec!["--network-class", "lan=br\"0"],
            vec!["--guest-uid-count", "0"],
            vec!["--guest-uid-base", "4294967295", "--guest-uid-count", "2"],
        ] {
            assert!(
                matches!(resolve(&args(&bad), &h), Err(Error::Setting { .. })),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_default_storage_falls_back_to_var_lib() {
        let h = FakeHost::default();
        h.put_file("/sys/class/net/eth0", b"", Kind::Other);
        assert_eq!(default_storage(&h), PathBuf::from("/var/lib/banlieue/ch"));
    }

    // ------------------------------------------------ guest slice (ADR-0105)

    #[test]
    fn no_budget_flags_means_an_unbounded_slice() {
        let h = FakeHost::debian();
        let s = resolve(&args(&[]), &h).unwrap();
        assert_eq!(s.guest_slice, GuestSlice::default());
        assert!(s.guest_slice.is_unbounded());
        assert_eq!(s.guest_slice.limits(), "");
    }

    #[test]
    fn the_budget_flags_become_slice_settings_in_a_fixed_order() {
        let h = FakeHost::debian();
        let s = resolve(
            &args(&[
                "--guest-cpus",
                "0-15,32-47",
                "--guest-cpu-weight",
                "200",
                "--guest-memory-max",
                "400G",
                "--guest-tasks-max",
                "30000",
            ]),
            &h,
        )
        .unwrap();
        assert!(!s.guest_slice.is_unbounded());
        assert_eq!(
            s.guest_slice.limits(),
            "AllowedCPUs=0-15,32-47\nCPUWeight=200\nMemoryMax=400G\nTasksMax=30000\n"
        );
    }

    #[test]
    fn a_percentage_is_a_valid_memory_budget() {
        let h = FakeHost::debian();
        let s = resolve(&args(&["--guest-memory-max", "60%"]), &h).unwrap();
        assert_eq!(s.guest_slice.limits(), "MemoryMax=60%\n");
    }

    /// A value is rendered into a root-owned unit: nothing that could add a
    /// directive, and nothing systemd would reject at load time.
    #[test]
    fn budget_values_that_could_corrupt_a_unit_are_refused() {
        let h = FakeHost::debian();
        for bad in [
            &["--guest-cpus", "0-3\nExecStart=/bin/sh"][..],
            &["--guest-cpus", "0-3;1"],
            &["--guest-cpus", "3-1"],
            &["--guest-cpus", "a"],
            &["--guest-cpus", ""],
            &["--guest-cpu-weight", "0"],
            &["--guest-cpu-weight", "10001"],
            &["--guest-memory-max", "10X"],
            &["--guest-memory-max", "101%"],
            &["--guest-memory-max", "0"],
            &["--guest-memory-max", "1G\nUser=root"],
            &["--guest-memory-max", "infinity"],
            &["--guest-tasks-max", "0"],
        ] {
            match resolve(&args(bad), &h) {
                Err(Error::Setting { .. }) => {}
                other => panic!("{bad:?}: {other:?}"),
            }
        }
    }
    // ------------------------------------- the host config install keeps

    /// A host config as `install` wrote it, then edited by its admin: the
    /// storage moved to `/srv/banlieue/ch`, a second bridge, a registry.
    const KEPT: &str = r#"
[provider]
name = "baz"
namespace = "platform"
kubeconfig = "/etc/banlieue/credentials/kubeconfig"

[vmm]
binary = "/usr/local/bin/cloud-hypervisor"
version = "v53.0"
firmware = "/opt/banlieue/firmware/ch-97eeb7b09/CLOUDHV.fd"

[paths]
run_root = "/run/banlieue/ch"
state_root = "/var/lib/banlieue"

[guests]
uid_base = 3000000
uid_count = 512

[registry]
repository = "registry.internal:5000/banlieue/images"
plain_http = true
keep_unreferenced = 2

[storage_classes]
default = "/srv/banlieue/ch"
fast = "/data/banlieue/ch"

[network_classes]
default = "virbr0"
lan = "br0"
"#;

    /// The case found live: `/home` has the most free space, so the probe would
    /// pick `/home/banlieue/ch`, while the config `install` keeps says
    /// `/srv/banlieue/ch`.
    fn kept_host() -> FakeHost {
        let h = FakeHost::debian();
        h.put_dir("/home");
        h.env.lock().unwrap().free.insert("/home".into(), 900 << 30);
        h.put_file(crate::paths::HOST_CONFIG, KEPT.as_bytes(), Kind::File);
        h
    }

    /// A re-run without flags renders units from the config it keeps, not
    /// from a fresh guess, so `ReadWritePaths=` names the real storage.
    #[test]
    fn a_kept_host_config_supplies_every_setting_it_records() {
        let h = kept_host();
        assert_eq!(default_storage(&h), PathBuf::from("/home/banlieue/ch"));
        let s = resolve(&args(&[]), &h).unwrap();
        assert_eq!(s.provider_name, "baz");
        assert_eq!(s.namespace, "platform");
        assert_eq!(
            s.storage,
            vec![
                ("default".into(), PathBuf::from("/srv/banlieue/ch")),
                ("fast".into(), PathBuf::from("/data/banlieue/ch")),
            ]
        );
        assert_eq!(
            s.network,
            vec![
                ("default".into(), "virbr0".into()),
                ("lan".into(), "br0".into())
            ]
        );
        assert_eq!((s.uid_base, s.uid_count), (3_000_000, 512));
        assert_eq!(
            s.registry,
            Some(Registry {
                repository: "registry.internal:5000/banlieue/images".into(),
                plain_http: true,
                keep_unreferenced: 2,
            })
        );
    }

    /// Flags that say what the config says are accepted, in any order.
    #[test]
    fn flags_that_agree_with_the_kept_config_are_accepted() {
        let h = kept_host();
        let s = resolve(
            &args(&[
                "--provider-name",
                "baz",
                "--provider-namespace",
                "platform",
                "--storage-class",
                "fast=/data/banlieue/ch,default=/srv/banlieue/ch",
                "--network-class",
                "lan=br0",
                "--network-class",
                "default=virbr0",
                "--guest-uid-base",
                "3000000",
                "--guest-uid-count",
                "512",
                "--registry-repository",
                "registry.internal:5000/banlieue/images",
                "--registry-plain-http",
                "--registry-keep-unreferenced",
                "2",
            ]),
            &h,
        )
        .unwrap();
        // The kept order, not the order given: the same units either way.
        assert_eq!(s, resolve(&args(&[]), &h).unwrap());
    }

    /// The config is kept, so a flag that contradicts it would render units
    /// that disagree with what the provider reads: refused, naming the flag.
    #[test]
    fn a_flag_that_contradicts_the_kept_config_is_refused() {
        let h = kept_host();
        for (bad, flag) in [
            (
                &["--storage-class", "default=/home/banlieue/ch"][..],
                "--storage-class",
            ),
            (
                &["--storage-class", "default=/srv/banlieue/ch"],
                "--storage-class",
            ),
            (&["--network-class", "default=virbr0"], "--network-class"),
            (&["--provider-name", "bar"], "--provider-name"),
            (
                &["--provider-namespace", "banlieue-system"],
                "--provider-namespace",
            ),
            (&["--guest-uid-base", "2000000"], "--guest-uid-base"),
            (&["--guest-uid-count", "1024"], "--guest-uid-count"),
            (
                &[
                    "--registry-repository",
                    "registry.internal:5000/banlieue/images",
                ],
                "--registry-repository",
            ),
        ] {
            match resolve(&args(bad), &h) {
                Err(Error::Setting { name, why }) => {
                    assert_eq!(name, flag, "{bad:?}");
                    assert!(why.contains(crate::paths::HOST_CONFIG), "{why}");
                    assert!(why.contains("--force"), "{why}");
                }
                other => panic!("{bad:?}: {other:?}"),
            }
        }
    }

    /// `--force` regenerates the config, so the flags and the probe decide
    /// and the kept file is not consulted.
    #[test]
    fn a_fresh_resolve_ignores_the_kept_config() {
        let h = kept_host();
        let s = resolve_fresh(&args(&["--guest-uid-base", "2000000"]), &h).unwrap();
        assert_eq!(s.provider_name, "bar");
        assert_eq!(s.namespace, DEFAULT_NAMESPACE);
        assert_eq!(
            s.storage,
            vec![("default".into(), PathBuf::from("/home/banlieue/ch"))]
        );
        assert_eq!(s.uid_base, DEFAULT_GUEST_UID_BASE);
        assert!(s.registry.is_none());
    }

    /// A kept config that does not load would leave the units guessing:
    /// refused, with the way out.
    #[test]
    fn a_kept_config_that_does_not_load_is_refused() {
        let h = FakeHost::debian();
        h.put_file(
            crate::paths::HOST_CONFIG,
            b"[provider]\nnope = 1\n",
            Kind::File,
        );
        match resolve(&args(&[]), &h) {
            Err(Error::KeptConfig(why)) => assert!(!why.is_empty()),
            other => panic!("{other:?}"),
        }
        assert!(resolve_fresh(&args(&[]), &h).is_ok());
    }

    // ------------------------------------------- ADR-0139: quota and refusal

    fn legacy(h: &FakeHost) {
        let mut env = h.env.lock().unwrap();
        env.cgroup_unified = false;
        env.systemd_version = Some(239);
    }

    #[test]
    fn a_cpu_quota_is_a_slice_setting_after_the_weight_and_before_memory() {
        let h = FakeHost::debian();
        let s = resolve(
            &args(&[
                "--guest-cpus",
                "0-3",
                "--guest-cpu-weight",
                "200",
                "--guest-cpu-quota",
                "4800%",
                "--guest-memory-max",
                "400G",
                "--guest-tasks-max",
                "30000",
            ]),
            &h,
        )
        .unwrap();
        assert_eq!(s.guest_slice.cpu_quota.as_deref(), Some("4800%"));
        assert_eq!(
            s.guest_slice.limits(),
            "AllowedCPUs=0-3\nCPUWeight=200\nCPUQuota=4800%\nMemoryMax=400G\nTasksMax=30000\n"
        );
    }

    #[test]
    fn a_quota_alone_is_a_bounded_slice() {
        let h = FakeHost::debian();
        let s = resolve(&args(&["--guest-cpu-quota", "100%"]), &h).unwrap();
        assert!(!s.guest_slice.is_unbounded());
        assert_eq!(s.guest_slice.limits(), "CPUQuota=100%\n");
    }

    /// A positive integer percentage, nothing else: the value goes into a
    /// root-owned unit.
    #[test]
    fn a_cpu_quota_outside_the_grammar_is_refused() {
        let h = FakeHost::debian();
        for bad in [
            "",
            "%",
            "0%",
            "00%",
            "48",
            "4800",
            "-5%",
            "+5%",
            "1.5%",
            "4800 %",
            " 4800%",
            "4800%%",
            "50%\nExecStart=/bin/sh",
            "50%;1",
            "18446744073709551616%",
            "infinity",
        ] {
            // The `=` form: a value that starts with `-` must reach `resolve`
            // as a value, not be read by clap as a flag (which would exit the
            // whole test process).
            let flag = format!("--guest-cpu-quota={bad}");
            match resolve(&args(&[flag.as_str()]), &h) {
                Err(Error::Setting { name, .. }) => {
                    assert_eq!(name, "--guest-cpu-quota", "{bad:?}")
                }
                other => panic!("{bad:?}: {other:?}"),
            }
        }
    }

    /// ADR-0139 Decision 2, measured on systemd 239 with cgroup v1: no
    /// cpuset cgroup exists, so `AllowedCPUs=` would be accepted, shown and
    /// never enforced. It is refused, and the message names the way out.
    #[test]
    fn guest_cpus_is_refused_on_cgroup_v1() {
        let h = FakeHost::debian();
        legacy(&h);
        let Err(Error::Setting { name, why }) = resolve(&args(&["--guest-cpus", "0-3"]), &h) else {
            panic!("--guest-cpus accepted on cgroup v1");
        };
        assert_eq!(name, "--guest-cpus");
        assert!(why.contains("cgroup v1"), "{why}");
        assert!(why.contains("--guest-cpu-quota"), "{why}");
    }

    #[test]
    fn guest_cpus_is_refused_below_the_systemd_that_has_the_setting() {
        let h = FakeHost::debian();
        h.env.lock().unwrap().systemd_version = Some(MIN_SYSTEMD_ALLOWED_CPUS - 1);
        assert!(matches!(
            resolve(&args(&["--guest-cpus", "0-3"]), &h),
            Err(Error::Setting {
                name: "--guest-cpus",
                ..
            })
        ));
        h.env.lock().unwrap().systemd_version = Some(MIN_SYSTEMD_ALLOWED_CPUS);
        resolve(&args(&["--guest-cpus", "0-3"]), &h).unwrap();
    }

    #[test]
    fn guest_cpus_is_accepted_on_the_unified_hierarchy() {
        let h = FakeHost::debian();
        resolve(&args(&["--guest-cpus", "0-3"]), &h).unwrap();
    }

    /// Measured: systemd 239 maps `CPUWeight=` to `cpu.shares`, and
    /// `MemoryMax=`, `TasksMax=` and `CPUQuota=` are enforced on v1 too.
    #[test]
    fn every_other_budget_flag_works_on_cgroup_v1() {
        let h = FakeHost::debian();
        legacy(&h);
        let s = resolve(
            &args(&[
                "--guest-cpu-weight",
                "50",
                "--guest-cpu-quota",
                "4800%",
                "--guest-memory-max",
                "250G",
                "--guest-tasks-max",
                "1000",
            ]),
            &h,
        )
        .unwrap();
        assert_eq!(
            s.guest_slice.limits(),
            "CPUWeight=50\nCPUQuota=4800%\nMemoryMax=250G\nTasksMax=1000\n"
        );
    }

    #[test]
    fn the_guest_accounts_are_derived_and_the_reason_kept() {
        let modern = FakeHost::debian();
        let s = resolve(&args(&[]), &modern).unwrap();
        assert_eq!(s.guest_accounts, GuestAccounts::Userdb);
        assert!(!s.accounts_reason.is_empty());

        let old = FakeHost::debian();
        legacy(&old);
        let s = resolve(&args(&[]), &old).unwrap();
        assert_eq!(s.guest_accounts, GuestAccounts::Files);
        assert!(s.accounts_reason.contains("239"), "{}", s.accounts_reason);
    }

    #[test]
    fn an_explicit_guest_accounts_wins_but_is_checked() {
        let modern = FakeHost::debian();
        let s = resolve(&args(&["--guest-accounts", "files"]), &modern).unwrap();
        assert_eq!(s.guest_accounts, GuestAccounts::Files);

        let old = FakeHost::debian();
        legacy(&old);
        assert!(matches!(
            resolve(&args(&["--guest-accounts", "userdb"]), &old),
            Err(Error::Setting {
                name: "--guest-accounts",
                ..
            })
        ));
        assert_eq!(
            resolve(&args(&["--guest-accounts", "files"]), &old)
                .unwrap()
                .guest_accounts,
            GuestAccounts::Files
        );
    }

    #[test]
    fn accepted_hardening_is_normalized_and_checked_for_known_names() {
        let h = FakeHost::debian();
        let s = resolve(
            &args(&["--accept-reduced-hardening", "PrivateIPC,PrivateIPC"]),
            &h,
        )
        .unwrap();
        assert_eq!(s.accepted_reduced, vec!["PrivateIPC".to_string()]);
        assert!(resolve(&args(&[]), &h).unwrap().accepted_reduced.is_empty());
        assert!(matches!(
            resolve(&args(&["--accept-reduced-hardening", "ProtectSystem"]), &h),
            Err(Error::Setting {
                name: "--accept-reduced-hardening",
                ..
            })
        ));
    }

    // ------------------------ ADR-0139 against the config install keeps

    /// `KEPT`, with the acceptance ADR-0139 records.
    fn kept_accepting_private_ipc() -> FakeHost {
        let h = kept_host();
        let text = format!("{KEPT}\n[hardening]\naccepted_reduced = [\"PrivateIPC\"]\n");
        h.put_file(crate::paths::HOST_CONFIG, text.as_bytes(), Kind::File);
        h
    }

    /// A config written before ADR-0139 has no `[hardening]` and must load.
    #[test]
    fn a_kept_config_from_before_the_hardening_section_resolves_to_nothing_accepted() {
        let h = kept_host();
        let s = resolve(&args(&[]), &h).unwrap();
        assert!(s.accepted_reduced.is_empty());
    }

    /// The acceptance is recorded in the config, so a plain re-run on the
    /// host that still needs it does not demand the flag again.
    #[test]
    fn a_kept_acceptance_stands_when_no_flag_is_given_and_the_host_still_lacks_it() {
        let h = kept_accepting_private_ipc();
        legacy(&h);
        let s = resolve(&args(&[]), &h).unwrap();
        assert_eq!(s.accepted_reduced, vec!["PrivateIPC".to_string()]);
    }

    /// A systemd upgrade makes the acceptance stale; it is dropped, never
    /// carried as a claim the host no longer needs.
    #[test]
    fn a_kept_acceptance_the_host_no_longer_needs_is_dropped() {
        let h = kept_accepting_private_ipc();
        let s = resolve(&args(&[]), &h).unwrap();
        assert!(s.accepted_reduced.is_empty());
    }

    /// The flag is the operator's statement for this run and is checked
    /// against the host's facts by the preflight, so it replaces the kept
    /// list instead of contradicting it.
    #[test]
    fn a_hardening_flag_replaces_the_kept_acceptance_and_is_not_a_contradiction() {
        let h = kept_host();
        legacy(&h);
        let s = resolve(&args(&["--accept-reduced-hardening", "PrivateIPC"]), &h).unwrap();
        assert_eq!(s.accepted_reduced, vec!["PrivateIPC".to_string()]);

        let h = kept_accepting_private_ipc();
        let s = resolve(&args(&["--accept-reduced-hardening", "PrivateIPC"]), &h).unwrap();
        assert_eq!(s.accepted_reduced, vec!["PrivateIPC".to_string()]);
    }

    /// `--force` regenerates from the flags alone.
    #[test]
    fn a_fresh_resolve_ignores_the_kept_acceptance() {
        let h = kept_accepting_private_ipc();
        legacy(&h);
        let s = resolve_fresh(&args(&[]), &h).unwrap();
        assert!(s.accepted_reduced.is_empty());
    }

    /// Account mode and the CPU quota are not recorded in the host config,
    /// so they cannot contradict it.
    #[test]
    fn the_adr_0139_flags_are_not_contradictions_of_a_kept_config() {
        let h = kept_accepting_private_ipc();
        legacy(&h);
        let s = resolve(
            &args(&["--guest-accounts", "files", "--guest-cpu-quota", "400%"]),
            &h,
        )
        .unwrap();
        assert_eq!(s.guest_accounts, GuestAccounts::Files);
        assert_eq!(s.guest_slice.cpu_quota.as_deref(), Some("400%"));
        assert_eq!(s.provider_name, "baz");
    }

    // ------------------------------------------------ attestation (ADR-0146)

    use crate::ops::{Host, Owner};

    const CA_DIR: &str = "/var/lib/banlieue/attestation-ca";
    const SECTION_TEXT: &str = "[attestation]\nca_dir = \"/var/lib/banlieue/attestation-ca\"\ncertificate_ttl_secs = 7200\n";

    fn provider_user(h: &FakeHost) {
        let mut s = h.state.lock().unwrap();
        s.users.insert("banlieue".into(), (998, 998));
        s.groups
            .insert("banlieue".into(), (998, Default::default()));
        drop(s);
        h.put_dir("/var/lib/banlieue");
    }

    /// A host that already has a CA named `name`.
    fn host_with_ca(name: &str) -> FakeHost {
        let h = FakeHost::debian();
        provider_user(&h);
        h.create_attestation_ca(Path::new(CA_DIR), name, &Owner::new("banlieue", "banlieue"))
            .unwrap();
        h
    }

    fn why(r: Result<Settings, Error>) -> String {
        match r {
            Err(Error::Setting { why, .. }) => why,
            other => panic!("expected a refused setting, got {other:?}"),
        }
    }

    fn flag_of(r: Result<Settings, Error>) -> &'static str {
        match r {
            Err(Error::Setting { name, .. }) => name,
            other => panic!("expected a refused setting, got {other:?}"),
        }
    }

    // ------------------------------------ the CSR route (ADR-0146 Decision 7)

    const PENDING_DIR: &str = "/var/lib/banlieue/attestation-ca-pending";

    use crate::settings::CaRoute;

    /// A host with an unsigned request waiting, named `name`.
    fn host_with_pending(name: &str) -> FakeHost {
        let h = FakeHost::debian();
        provider_user(&h);
        h.create_pending_attestation_ca(
            Path::new(PENDING_DIR),
            name,
            &Owner::new("banlieue", "banlieue"),
        )
        .unwrap();
        h
    }

    fn plan_of(r: Result<Settings, Error>) -> crate::settings::AttestationPlan {
        r.unwrap().attestation.expect("opted in")
    }

    #[test]
    fn the_self_signed_route_is_the_default() {
        let h = FakeHost::debian();
        let plan = plan_of(resolve(&args(&["--attestation-ca-name", "bar"]), &h));
        assert_eq!(plan.route, CaRoute::SelfSigned);
    }

    #[test]
    fn csr_with_a_name_plans_a_request_and_opts_in() {
        let h = FakeHost::debian();
        let plan = plan_of(resolve(
            &args(&["--attestation-ca-csr", "--attestation-ca-name", "sandbox-1"]),
            &h,
        ));
        assert_eq!(plan.route, CaRoute::Csr);
        assert_eq!(plan.new_ca_name.as_deref(), Some("sandbox-1"));
        assert!(!plan.rotate);
    }

    #[test]
    fn csr_with_attestation_alone_names_the_request_after_the_short_host_name() {
        let h = FakeHost::debian();
        let plan = plan_of(resolve(
            &args(&["--attestation-ca-csr", "--attestation"]),
            &h,
        ));
        assert_eq!(plan.route, CaRoute::Csr);
        assert_eq!(plan.new_ca_name.as_deref(), Some(h.hostname().as_str()));
    }

    #[test]
    fn csr_without_a_name_is_refused() {
        let h = FakeHost::debian();
        let r = resolve(&args(&["--attestation-ca-csr"]), &h);
        assert_eq!(flag_of(r), "--attestation-ca-csr");
    }

    #[test]
    fn csr_refuses_a_second_request_while_one_is_pending() {
        let h = host_with_pending("old");
        let r = resolve(
            &args(&["--attestation-ca-csr", "--attestation-ca-name", "new"]),
            &h,
        );
        assert_eq!(flag_of(r), "--attestation-ca-csr");
    }

    #[test]
    fn csr_refuses_a_host_that_already_has_a_ca_unless_it_rotates() {
        let h = host_with_ca("bar");
        let r = resolve(
            &args(&["--attestation-ca-csr", "--attestation-ca-name", "baz"]),
            &h,
        );
        assert_eq!(flag_of(r), "--attestation-ca-csr");
        assert!(
            why(resolve(
                &args(&["--attestation-ca-csr", "--attestation-ca-name", "baz"]),
                &h
            ))
            .contains("--rotate-attestation-ca")
        );
    }

    #[test]
    fn csr_with_rotate_and_a_new_name_is_planned_beside_the_live_ca() {
        let h = host_with_ca("bar");
        let plan = plan_of(resolve(
            &args(&[
                "--attestation-ca-csr",
                "--rotate-attestation-ca",
                "--attestation-ca-name",
                "baz",
            ]),
            &h,
        ));
        assert_eq!(plan.route, CaRoute::Csr);
        assert!(plan.rotate);
    }

    #[test]
    fn csr_with_rotate_refuses_the_current_name() {
        let h = host_with_ca("bar");
        let r = resolve(
            &args(&[
                "--attestation-ca-csr",
                "--rotate-attestation-ca",
                "--attestation-ca-name",
                "bar",
            ]),
            &h,
        );
        assert_eq!(flag_of(r), "--attestation-ca-name");
    }

    #[test]
    fn csr_and_import_together_are_refused() {
        let h = host_with_pending("bar");
        let r = resolve(
            &args(&[
                "--attestation-ca-csr",
                "--attestation-ca-name",
                "bar",
                "--attestation-ca-import",
                "/root/signed.pem",
            ]),
            &h,
        );
        assert_eq!(flag_of(r), "--attestation-ca-import");
    }

    #[test]
    fn import_without_a_pending_request_is_refused() {
        let h = FakeHost::debian();
        let r = resolve(&args(&["--attestation-ca-import", "/root/signed.pem"]), &h);
        assert_eq!(flag_of(r), "--attestation-ca-import");
    }

    #[test]
    fn import_with_a_pending_request_is_the_opt_in_and_needs_no_name() {
        let h = host_with_pending("bar");
        let plan = plan_of(resolve(
            &args(&["--attestation-ca-import", "/root/signed.pem"]),
            &h,
        ));
        assert_eq!(plan.route, CaRoute::Import("/root/signed.pem".into()));
        assert_eq!(plan.new_ca_name, None);
        assert!(!plan.rotate);
    }

    #[test]
    fn import_refuses_a_name_that_is_not_the_pending_requests() {
        let h = host_with_pending("bar");
        let r = resolve(
            &args(&[
                "--attestation-ca-import",
                "/root/signed.pem",
                "--attestation-ca-name",
                "other",
            ]),
            &h,
        );
        assert_eq!(flag_of(r), "--attestation-ca-name");
    }

    #[test]
    fn import_over_a_live_ca_needs_rotate() {
        let h = host_with_ca("old");
        h.create_pending_attestation_ca(
            Path::new(PENDING_DIR),
            "new",
            &Owner::new("banlieue", "banlieue"),
        )
        .unwrap();
        let r = resolve(&args(&["--attestation-ca-import", "/root/signed.pem"]), &h);
        assert_eq!(flag_of(r), "--attestation-ca-import");
        let plan = plan_of(resolve(
            &args(&[
                "--attestation-ca-import",
                "/root/signed.pem",
                "--rotate-attestation-ca",
            ]),
            &h,
        ));
        assert!(plan.rotate);
        assert_eq!(plan.route, CaRoute::Import("/root/signed.pem".into()));
    }

    #[test]
    fn import_over_a_live_ca_of_the_same_name_is_refused() {
        let h = host_with_ca("same");
        h.create_pending_attestation_ca(
            Path::new(PENDING_DIR),
            "same",
            &Owner::new("banlieue", "banlieue"),
        )
        .unwrap();
        let r = resolve(
            &args(&[
                "--attestation-ca-import",
                "/root/signed.pem",
                "--rotate-attestation-ca",
            ]),
            &h,
        );
        assert_eq!(flag_of(r), "--attestation-ca-name");
    }

    #[test]
    fn a_pending_request_alone_does_not_opt_a_host_in() {
        let h = host_with_pending("bar");
        assert!(resolve(&args(&[]), &h).unwrap().attestation.is_none());
    }

    // ---------------------------------- OpenBao signing (ADR-0146 Decision 8)

    use crate::settings::{OpenBaoAuth, OpenBaoPlan};

    const BAO: &str = "https://bao.example.com:8200";

    /// The flags that make a complete, service-account OpenBao route.
    fn bao_args(extra: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = [
            "--attestation-ca-name",
            "sandbox-1",
            "--attestation-ca-openbao-addr",
            BAO,
            "--attestation-ca-openbao-mount",
            "pki-node-attestation",
            "--attestation-ca-openbao-auth-mount",
            "computefabric-test-v2",
            "--attestation-ca-openbao-role",
            "ch-host-ca-sandbox-1",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        v.extend(extra.iter().map(|s| (*s).to_string()));
        v
    }

    fn resolve_bao(extra: &[&str], h: &FakeHost) -> Result<Settings, Error> {
        let owned = bao_args(extra);
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        resolve(&args(&refs), h)
    }

    fn bao_plan_of(r: Result<Settings, Error>) -> OpenBaoPlan {
        match plan_of(r).route {
            CaRoute::OpenBao(p) => p,
            other => panic!("expected the OpenBao route, got {other:?}"),
        }
    }

    #[test]
    fn a_service_account_route_is_planned_with_the_pinned_default_ttl() {
        let h = FakeHost::debian();
        let plan = plan_of(resolve_bao(&[], &h));
        assert_eq!(plan.new_ca_name.as_deref(), Some("sandbox-1"));
        let bao = bao_plan_of(resolve_bao(&[], &h));
        assert_eq!(bao.addr, BAO);
        assert_eq!(bao.mount, "pki-node-attestation");
        assert_eq!(bao.ttl, "8760h");
        assert_eq!(
            bao.auth,
            OpenBaoAuth::Jwt {
                auth_mount: "computefabric-test-v2".into(),
                role: "ch-host-ca-sandbox-1".into()
            }
        );
    }

    #[test]
    fn a_token_file_route_is_planned_instead_of_a_role() {
        let h = FakeHost::debian();
        let r = resolve(
            &args(&[
                "--attestation-ca-name",
                "sandbox-1",
                "--attestation-ca-openbao-addr",
                BAO,
                "--attestation-ca-openbao-mount",
                "pki-node-attestation",
                "--attestation-ca-openbao-token-file",
                "/root/bao.token",
            ]),
            &h,
        );
        assert_eq!(
            bao_plan_of(r).auth,
            OpenBaoAuth::TokenFile("/root/bao.token".into())
        );
    }

    #[test]
    fn the_ca_ttl_must_match_what_the_policy_pins_so_it_is_sent_as_given() {
        let h = FakeHost::debian();
        let bao = bao_plan_of(resolve_bao(
            &["--attestation-ca-openbao-ca-ttl", "4380h"],
            &h,
        ));
        assert_eq!(bao.ttl, "4380h");
        for bad in [
            "",
            "8760",
            "h",
            "1.5h",
            "-1h",
            "1 h",
            "1d",
            "9999999999999999999999h",
        ] {
            // `--flag=value`: a value like `-1h` would be read as a flag.
            let given = format!("--attestation-ca-openbao-ca-ttl={bad}");
            let r = resolve_bao(&[given.as_str()], &h);
            assert_eq!(flag_of(r), "--attestation-ca-openbao-ca-ttl", "{bad:?}");
        }
    }

    #[test]
    fn the_address_must_be_a_bare_https_origin() {
        let h = FakeHost::debian();
        for bad in [
            "http://bao.example.com",
            "https://u:p@bao.example.com",
            "https://bao.example.com/x?y=1",
            "bao.example.com",
            "https://",
        ] {
            let mut owned = bao_args(&[]);
            let i = owned.iter().position(|a| a == BAO).unwrap();
            owned[i] = bad.to_string();
            let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
            assert_eq!(
                flag_of(resolve(&args(&refs), &h)),
                "--attestation-ca-openbao-addr",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_mount_or_role_that_could_leave_its_place_is_refused() {
        let h = FakeHost::debian();
        for (flag, bad) in [
            ("--attestation-ca-openbao-mount", "../x"),
            ("--attestation-ca-openbao-mount", "/pki"),
            ("--attestation-ca-openbao-auth-mount", "a/../b"),
            ("--attestation-ca-openbao-role", "a b"),
            ("--attestation-ca-openbao-role", "r?x=1"),
        ] {
            let mut owned = bao_args(&[]);
            let i = owned.iter().position(|a| a == flag).unwrap();
            owned[i + 1] = bad.to_string();
            let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
            assert_eq!(flag_of(resolve(&args(&refs), &h)), flag, "{flag} {bad:?}");
        }
    }

    #[test]
    fn any_openbao_flag_without_the_address_is_refused() {
        let h = FakeHost::debian();
        let r = resolve(
            &args(&[
                "--attestation-ca-name",
                "sandbox-1",
                "--attestation-ca-openbao-mount",
                "pki-node-attestation",
            ]),
            &h,
        );
        assert_eq!(flag_of(r), "--attestation-ca-openbao-addr");
    }

    #[test]
    fn the_mount_is_required() {
        let h = FakeHost::debian();
        let r = resolve(
            &args(&[
                "--attestation-ca-name",
                "sandbox-1",
                "--attestation-ca-openbao-addr",
                BAO,
                "--attestation-ca-openbao-token-file",
                "/root/t",
            ]),
            &h,
        );
        assert_eq!(flag_of(r), "--attestation-ca-openbao-mount");
    }

    #[test]
    fn exactly_one_way_to_log_in_is_required() {
        let h = FakeHost::debian();
        let neither = resolve(
            &args(&[
                "--attestation-ca-name",
                "sandbox-1",
                "--attestation-ca-openbao-addr",
                BAO,
                "--attestation-ca-openbao-mount",
                "m",
            ]),
            &h,
        );
        assert_eq!(flag_of(neither), "--attestation-ca-openbao-role");
        let role_only = resolve(
            &args(&[
                "--attestation-ca-name",
                "sandbox-1",
                "--attestation-ca-openbao-addr",
                BAO,
                "--attestation-ca-openbao-mount",
                "m",
                "--attestation-ca-openbao-role",
                "r",
            ]),
            &h,
        );
        assert_eq!(flag_of(role_only), "--attestation-ca-openbao-auth-mount");
        let both = resolve_bao(&["--attestation-ca-openbao-token-file", "/root/t"], &h);
        assert_eq!(flag_of(both), "--attestation-ca-openbao-token-file");
    }

    #[test]
    fn the_openbao_route_needs_a_name() {
        let h = FakeHost::debian();
        let r = resolve(
            &args(&[
                "--attestation-ca-openbao-addr",
                BAO,
                "--attestation-ca-openbao-mount",
                "m",
                "--attestation-ca-openbao-token-file",
                "/root/t",
            ]),
            &h,
        );
        assert_eq!(flag_of(r), "--attestation-ca-name");
        // `--attestation` names it after the host, as for the other routes.
        let ok = resolve(
            &args(&[
                "--attestation",
                "--attestation-ca-openbao-addr",
                BAO,
                "--attestation-ca-openbao-mount",
                "m",
                "--attestation-ca-openbao-token-file",
                "/root/t",
            ]),
            &h,
        );
        assert_eq!(
            plan_of(ok).new_ca_name.as_deref(),
            Some(h.hostname().as_str())
        );
    }

    #[test]
    fn the_openbao_route_excludes_the_csr_and_import_routes() {
        let h = FakeHost::debian();
        assert_eq!(
            flag_of(resolve_bao(&["--attestation-ca-csr"], &h)),
            "--attestation-ca-csr"
        );
        let h = host_with_pending("sandbox-1");
        assert_eq!(
            flag_of(resolve_bao(
                &["--attestation-ca-import", "/root/signed.pem"],
                &h
            )),
            "--attestation-ca-import"
        );
    }

    #[test]
    fn a_pending_request_of_the_same_name_is_reused_and_another_name_is_refused() {
        let h = host_with_pending("sandbox-1");
        assert_eq!(
            plan_of(resolve_bao(&[], &h)).new_ca_name.as_deref(),
            Some("sandbox-1")
        );
        let h = host_with_pending("other");
        assert_eq!(flag_of(resolve_bao(&[], &h)), "--attestation-ca-name");
    }

    #[test]
    fn a_live_ca_needs_rotate_and_a_new_name() {
        let h = host_with_ca("bar");
        assert_eq!(
            flag_of(resolve_bao(&[], &h)),
            "--attestation-ca-openbao-addr"
        );
        let plan = plan_of(resolve_bao(&["--rotate-attestation-ca"], &h));
        assert!(plan.rotate);
        let h = host_with_ca("sandbox-1");
        assert_eq!(
            flag_of(resolve_bao(&["--rotate-attestation-ca"], &h)),
            "--attestation-ca-name"
        );
    }

    #[test]
    fn no_attestation_flag_means_not_opted_in() {
        let h = FakeHost::debian();
        assert!(resolve(&args(&[]), &h).unwrap().attestation.is_none());
    }

    #[test]
    fn a_ca_name_is_the_opt_in_with_a_one_hour_default() {
        let h = FakeHost::debian();
        let plan = resolve(&args(&["--attestation-ca-name", "sandbox-1.foo_2"]), &h)
            .unwrap()
            .attestation
            .unwrap();
        assert_eq!(plan.new_ca_name.as_deref(), Some("sandbox-1.foo_2"));
        assert_eq!(plan.ttl, Duration::from_secs(3600));
        assert!(!plan.rotate);
    }

    #[test]
    fn an_invalid_ca_name_is_refused_never_replaced() {
        let h = FakeHost::debian();
        for bad in ["bad name", "a/b", "", "a=b", "x\"y", "caf\u{e9}"] {
            let r = resolve(&args(&["--attestation-ca-name", bad]), &h);
            assert_eq!(flag_of(r), "--attestation-ca-name", "{bad:?}");
        }
        let long = "a".repeat(65);
        assert_eq!(
            flag_of(resolve(&args(&["--attestation-ca-name", &long]), &h)),
            "--attestation-ca-name"
        );
    }

    #[test]
    fn attestation_alone_names_the_ca_after_the_short_host_name() {
        let h = FakeHost::debian();
        let plan = resolve(&args(&["--attestation"]), &h)
            .unwrap()
            .attestation
            .unwrap();
        assert_eq!(plan.new_ca_name.as_deref(), Some("bar"));
    }

    #[test]
    fn attestation_refuses_a_short_name_outside_the_alphabet_unless_a_name_is_given() {
        let h = FakeHost::debian();
        h.env.lock().unwrap().hostname = "b@r host".into();
        let r = resolve(&args(&["--attestation"]), &h);
        assert!(why(r).contains("--attestation-ca-name"));
        let plan = resolve(
            &args(&["--attestation", "--attestation-ca-name", "sandbox"]),
            &h,
        )
        .unwrap()
        .attestation
        .unwrap();
        assert_eq!(plan.new_ca_name.as_deref(), Some("sandbox"));
    }

    #[test]
    fn the_certificate_ttl_is_parsed_and_bounded() {
        let h = FakeHost::debian();
        for (raw, secs) in [("30m", 1800), ("1h", 3600), ("12h", 43_200)] {
            let s = resolve(
                &args(&["--attestation", "--attestation-certificate-ttl", raw]),
                &h,
            )
            .unwrap();
            assert_eq!(s.attestation.unwrap().ttl, Duration::from_secs(secs));
        }
        for bad in ["4m", "8d", "soon", "0"] {
            let r = resolve(
                &args(&["--attestation", "--attestation-certificate-ttl", bad]),
                &h,
            );
            assert_eq!(flag_of(r), "--attestation-certificate-ttl", "{bad:?}");
        }
    }

    // ------------------------------------- port isolation (ADR-0144)

    /// The kept config with `lan` declared isolated.
    fn kept_isolated_host() -> FakeHost {
        let h = kept_host();
        let text = KEPT.replace(
            "lan = \"br0\"",
            "lan = { bridge = \"br0\", isolate_ports = true }",
        );
        h.put_file(crate::paths::HOST_CONFIG, text.as_bytes(), Kind::File);
        h
    }

    #[test]
    fn isolation_is_off_unless_the_operator_declares_it() {
        let h = FakeHost::debian();
        let s = resolve(&args(&["--network-class", "lan=br0"]), &h).unwrap();
        assert!(s.isolate_ports.is_empty());
    }

    #[test]
    fn isolate_ports_marks_a_declared_class() {
        let h = FakeHost::debian();
        let s = resolve(
            &args(&[
                "--network-class",
                "default=virbr0,sandbox=br1",
                "--isolate-ports",
                "sandbox",
            ]),
            &h,
        )
        .unwrap();
        assert_eq!(s.isolate_ports, vec!["sandbox".to_string()]);
        assert_eq!(s.network.len(), 2);
    }

    #[test]
    fn isolate_ports_takes_a_comma_list() {
        let h = FakeHost::debian();
        let s = resolve(
            &args(&["--network-class", "a=br1,b=br2", "--isolate-ports", "b,a"]),
            &h,
        )
        .unwrap();
        assert_eq!(s.isolate_ports, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn isolate_ports_refuses_an_undeclared_class() {
        let h = FakeHost::debian();
        let Err(Error::Setting { name, why }) = resolve(
            &args(&["--network-class", "lan=br0", "--isolate-ports", "ghost"]),
            &h,
        ) else {
            panic!("accepted");
        };
        assert_eq!(name, "--isolate-ports");
        assert!(why.contains("ghost"), "{why}");
    }

    #[test]
    fn isolate_ports_refuses_a_malformed_or_repeated_name() {
        let h = FakeHost::debian();
        for bad in ["Upper", "a=b", "x,x"] {
            let r = resolve(
                &args(&["--network-class", "x=br0", "--isolate-ports", bad]),
                &h,
            );
            assert!(
                matches!(&r, Err(Error::Setting { name, .. }) if *name == "--isolate-ports"),
                "{bad}: {r:?}"
            );
        }
    }

    #[test]
    fn a_ttl_without_an_opt_in_is_refused() {
        let h = FakeHost::debian();
        let r = resolve(&args(&["--attestation-certificate-ttl", "1h"]), &h);
        assert_eq!(flag_of(r), "--attestation-certificate-ttl");
    }

    #[test]
    fn rotation_needs_an_explicit_new_name() {
        let h = host_with_ca("bar");
        let r = resolve(&args(&["--rotate-attestation-ca"]), &h);
        assert_eq!(flag_of(r), "--rotate-attestation-ca");
        let r = resolve(&args(&["--rotate-attestation-ca", "--attestation"]), &h);
        assert_eq!(flag_of(r), "--rotate-attestation-ca");
    }

    /// A name must never be reused for a different key.
    #[test]
    fn rotation_refuses_the_current_name() {
        let h = host_with_ca("bar");
        let r = resolve(
            &args(&["--rotate-attestation-ca", "--attestation-ca-name", "bar"]),
            &h,
        );
        assert!(why(r).contains("never reused"));
    }

    #[test]
    fn rotation_with_a_new_name_is_planned() {
        let h = host_with_ca("bar");
        let plan = resolve(
            &args(&["--rotate-attestation-ca", "--attestation-ca-name", "bar-2"]),
            &h,
        )
        .unwrap()
        .attestation
        .unwrap();
        assert!(plan.rotate);
        assert_eq!(plan.new_ca_name.as_deref(), Some("bar-2"));
    }

    #[test]
    fn rotation_with_no_ca_to_rotate_is_refused() {
        let h = FakeHost::debian();
        let r = resolve(
            &args(&["--rotate-attestation-ca", "--attestation-ca-name", "bar"]),
            &h,
        );
        assert!(why(r).contains("no attestation CA"));
    }

    #[test]
    fn rotation_of_an_unreadable_ca_is_refused() {
        let h = host_with_ca("bar");
        h.set_mode(Path::new("/var/lib/banlieue/attestation-ca/ca.key"), 0o640)
            .unwrap();
        let r = resolve(
            &args(&["--rotate-attestation-ca", "--attestation-ca-name", "bar-2"]),
            &h,
        );
        assert!(why(r).contains("cannot be read"));
    }

    #[test]
    fn another_name_over_an_existing_ca_needs_rotation() {
        let h = host_with_ca("bar");
        let r = resolve(&args(&["--attestation-ca-name", "baz"]), &h);
        assert!(why(r).contains("--rotate-attestation-ca"));
        let same = resolve(&args(&["--attestation-ca-name", "bar"]), &h)
            .unwrap()
            .attestation
            .unwrap();
        assert_eq!(same.new_ca_name, None, "the CA that is there is kept");
    }

    /// The kept-config rule: no flag keeps the section, and its ttl.
    #[test]
    fn an_existing_section_keeps_the_host_opted_in() {
        let h = host_with_ca("bar");
        let text = format!("{KEPT}\n{SECTION_TEXT}");
        h.put_file(crate::paths::HOST_CONFIG, text.as_bytes(), Kind::File);
        let plan = resolve(&args(&[]), &h).unwrap().attestation.unwrap();
        assert_eq!(plan.new_ca_name, None);
        assert_eq!(plan.ttl, Duration::from_secs(7200));
        let changed = resolve(&args(&["--attestation-certificate-ttl", "30m"]), &h)
            .unwrap()
            .attestation
            .unwrap();
        assert_eq!(changed.ttl, Duration::from_secs(1800));
    }

    /// `--force` regenerates the file; it never rotates the CA, and the host
    /// stays opted in.
    #[test]
    fn a_fresh_resolve_keeps_the_attestation_plan_and_never_rotates() {
        let h = host_with_ca("bar");
        let text = format!("{KEPT}\n{SECTION_TEXT}");
        h.put_file(crate::paths::HOST_CONFIG, text.as_bytes(), Kind::File);
        let plan = resolve_fresh(&args(&[]), &h).unwrap().attestation.unwrap();
        assert!(!plan.rotate);
        assert_eq!(plan.new_ca_name, None);
    }

    #[test]
    fn a_kept_isolated_class_stays_isolated_without_the_flag() {
        let h = kept_isolated_host();
        let s = resolve(&args(&[]), &h).unwrap();
        assert_eq!(s.isolate_ports, vec!["lan".to_string()]);
    }

    #[test]
    fn the_flag_agreeing_with_the_kept_isolation_is_accepted() {
        let h = kept_isolated_host();
        let s = resolve(&args(&["--isolate-ports", "lan"]), &h).unwrap();
        assert_eq!(s, resolve(&args(&[]), &h).unwrap());
    }

    #[test]
    fn the_flag_contradicting_the_kept_isolation_is_refused() {
        for (h, flag) in [
            (kept_host(), "lan"),
            (kept_isolated_host(), "default"),
            (kept_isolated_host(), "default,lan"),
        ] {
            let Err(Error::Setting { name, why }) = resolve(&args(&["--isolate-ports", flag]), &h)
            else {
                panic!("{flag}: accepted");
            };
            assert_eq!(name, "--isolate-ports");
            assert!(why.contains(crate::paths::HOST_CONFIG), "{why}");
            assert!(why.contains("--force"), "{why}");
        }
    }

    #[test]
    fn a_fresh_resolve_ignores_the_kept_isolation() {
        let h = kept_isolated_host();
        let s = resolve_fresh(
            &args(&["--network-class", "lan=br0", "--isolate-ports", "lan"]),
            &h,
        )
        .unwrap();
        assert_eq!(s.isolate_ports, vec!["lan".to_string()]);
        let none = resolve_fresh(&args(&["--network-class", "lan=br0"]), &h).unwrap();
        assert!(none.isolate_ports.is_empty());
    }
}
