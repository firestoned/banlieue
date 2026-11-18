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
}
