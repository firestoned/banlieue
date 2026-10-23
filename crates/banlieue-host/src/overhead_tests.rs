// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `overhead.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::fs;

    // ------------------------------------------------------------ discovery

    #[test]
    fn a_banlieue_guest_is_its_vmm_and_its_swtpm_unit() {
        assert_eq!(
            vm_of_cgroup("banlieue-ch@2000017.service"),
            Some(VmId::guest("2000017"))
        );
        assert_eq!(
            vm_of_cgroup("banlieue-swtpm@2000017.service"),
            Some(VmId::guest("2000017"))
        );
    }

    /// The helpers run once and are not a VM's cost.
    #[test]
    fn guest_helpers_are_not_vms() {
        assert_eq!(vm_of_cgroup("banlieue-swtpm-setup@2000017.service"), None);
        assert_eq!(vm_of_cgroup("banlieue-ch-import@abc.service"), None);
        assert_eq!(
            vm_of_cgroup("banlieue-provider-cloud-hypervisor.service"),
            None
        );
    }

    /// A node name may itself contain dashes; `-swtpm` belongs to the node
    /// and the `-net` oneshot is not a VM.
    #[test]
    fn a_bootstrap_node_is_its_vmm_and_swtpm_but_not_its_net_unit() {
        assert_eq!(
            vm_of_cgroup("k0s-vm-node-a.service"),
            Some(VmId::node("node-a"))
        );
        assert_eq!(
            vm_of_cgroup("k0s-vm-node-a-swtpm.service"),
            Some(VmId::node("node-a"))
        );
        assert_eq!(vm_of_cgroup("k0s-vm-node-a-net.service"), None);
        assert_eq!(vm_of_cgroup("k0s-vm-.service"), None);
    }

    /// systemd escapes `-` in scope names as `\x2d`; the first field is
    /// libvirt's domain id.
    #[test]
    fn a_libvirt_domain_is_its_machine_scope() {
        assert_eq!(
            vm_of_cgroup(r"machine-qemu\x2d2\x2dnode\x2da.scope"),
            Some(VmId::libvirt("node-a"))
        );
        assert_eq!(vm_of_cgroup("machine.slice"), None);
        assert_eq!(vm_of_cgroup(r"machine-qemu\x2d2.scope"), None);
    }

    // ------------------------------------------------------------- threads

    #[test]
    fn vcpu_threads_are_recognised_for_both_vmms() {
        assert_eq!(bucket("cloud-hyperviso", "vcpu0"), Bucket::Vcpu);
        assert_eq!(bucket("cloud-hyperviso", "vcpu15"), Bucket::Vcpu);
        assert_eq!(bucket("qemu-system-x86", "CPU 3/KVM"), Bucket::Vcpu);
        assert_eq!(bucket("cloud-hyperviso", "virtio-blk0"), Bucket::Vmm);
        assert_eq!(bucket("cloud-hyperviso", "vcpu"), Bucket::Vmm);
        assert_eq!(bucket("qemu-system-x86", "CPU x/KVM"), Bucket::Vmm);
    }

    /// The emulator's own process name decides, whatever its threads are called.
    #[test]
    fn every_swtpm_thread_is_swtpm() {
        assert_eq!(bucket("swtpm", "swtpm"), Bucket::Swtpm);
        assert_eq!(bucket("swtpm", "vcpu0"), Bucket::Swtpm);
    }

    /// `comm` may contain spaces and parentheses; fields are counted after
    /// the last `)`.
    #[test]
    fn stat_is_parsed_after_the_last_parenthesis() {
        let stat = stat_line("CPU 0/KVM (x)", 700, 300, 450);
        assert_eq!(
            parse_stat(&stat),
            Some(ThreadTimes {
                total: 1000,
                guest: 450
            })
        );
        assert_eq!(parse_stat("garbage"), None);
        assert_eq!(parse_stat("1 (x) S 1 2"), None);
    }

    // --------------------------------------------------------------- usage

    /// guest = vCPU guest_time; vCPU host = the rest of the vCPU threads'
    /// time; VMM and swtpm = all their time. A thread born mid-interval
    /// counts from zero; one that died counts nothing.
    #[test]
    fn usage_splits_the_interval_into_four_parts() {
        let mut before = Snapshot::new();
        before.insert(
            (1, 1),
            (
                Bucket::Vcpu,
                ThreadTimes {
                    total: 100,
                    guest: 60,
                },
            ),
        );
        before.insert(
            (1, 2),
            (
                Bucket::Vmm,
                ThreadTimes {
                    total: 10,
                    guest: 0,
                },
            ),
        );
        before.insert(
            (9, 9),
            (
                Bucket::Vmm,
                ThreadTimes {
                    total: 50,
                    guest: 0,
                },
            ),
        );
        let mut after = Snapshot::new();
        after.insert(
            (1, 1),
            (
                Bucket::Vcpu,
                ThreadTimes {
                    total: 300,
                    guest: 210,
                },
            ),
        );
        after.insert(
            (1, 2),
            (
                Bucket::Vmm,
                ThreadTimes {
                    total: 14,
                    guest: 0,
                },
            ),
        );
        after.insert((2, 2), (Bucket::Swtpm, ThreadTimes { total: 3, guest: 0 }));
        let u = usage(&before, &after);
        assert_eq!(
            u,
            Usage {
                guest: 150,
                vcpu_host: 50,
                vmm: 4,
                swtpm: 3
            }
        );
        assert_eq!(u.total(), 207);
        let ratio = u.overhead_ratio().unwrap();
        assert!((ratio - 57.0 / 150.0).abs() < 1e-9, "{ratio}");
    }

    #[test]
    fn an_idle_guest_has_no_overhead_ratio() {
        assert_eq!(Usage::default().overhead_ratio(), None);
    }

    /// Counters never run backwards, but a reused tid could look like it.
    #[test]
    fn a_counter_that_went_backwards_counts_nothing() {
        let mut before = Snapshot::new();
        before.insert(
            (1, 1),
            (
                Bucket::Vcpu,
                ThreadTimes {
                    total: 100,
                    guest: 90,
                },
            ),
        );
        let mut after = Snapshot::new();
        after.insert(
            (1, 1),
            (
                Bucket::Vcpu,
                ThreadTimes {
                    total: 40,
                    guest: 30,
                },
            ),
        );
        assert_eq!(usage(&before, &after), Usage::default());
    }

    // ------------------------------------------------------------ sampling

    /// A fake host: two cgroup trees and a /proc, as files.
    struct Fixture {
        root: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                root: tempfile::tempdir().unwrap(),
            }
        }
        fn cgroups(&self) -> std::path::PathBuf {
            self.root.path().join("cgroup")
        }
        fn proc(&self) -> std::path::PathBuf {
            self.root.path().join("proc")
        }
        fn cgroup(&self, rel: &str, pids: &[u32]) {
            let dir = self.cgroups().join(rel);
            fs::create_dir_all(&dir).unwrap();
            let procs: Vec<String> = pids.iter().map(u32::to_string).collect();
            fs::write(dir.join("cgroup.procs"), procs.join("\n")).unwrap();
        }
        fn thread(
            &self,
            pid: u32,
            pcomm: &str,
            tid: u32,
            tcomm: &str,
            (user, sys, guest): (u64, u64, u64),
        ) {
            let p = self.proc().join(pid.to_string());
            let t = p.join("task").join(tid.to_string());
            fs::create_dir_all(&t).unwrap();
            fs::write(p.join("comm"), format!("{pcomm}\n")).unwrap();
            fs::write(t.join("comm"), format!("{tcomm}\n")).unwrap();
            fs::write(t.join("stat"), stat_line(tcomm, user, sys, guest)).unwrap();
        }
        fn sampler(&self) -> Sampler {
            Sampler::new(self.cgroups(), self.proc())
        }
    }

    /// A `/proc/<pid>/task/<tid>/stat` line with the three fields we read.
    fn stat_line(comm: &str, utime: u64, stime: u64, guest: u64) -> String {
        // pid (comm) state, then fields 4.. ; utime is field 14, stime 15,
        // guest_time 43 (1-based, as in proc(5)).
        let mut rest: Vec<String> = vec!["S".into()];
        rest.extend((4..=52).map(|_| "0".to_string()));
        rest[14 - 3] = utime.to_string();
        rest[15 - 3] = stime.to_string();
        rest[43 - 3] = guest.to_string();
        format!("1 ({comm}) {}\n", rest.join(" "))
    }

    #[test]
    fn discovery_finds_every_kind_wherever_it_runs() {
        let f = Fixture::new();
        f.cgroup(
            "banlieue.slice/banlieue-guests.slice/banlieue-ch@2000001.service",
            &[10],
        );
        f.cgroup(
            "banlieue.slice/banlieue-guests.slice/banlieue-swtpm@2000001.service",
            &[11],
        );
        // Started before its slice existed: still in system.slice.
        f.cgroup("system.slice/banlieue-ch@2000002.service", &[20]);
        f.cgroup(
            "banlieue.slice/banlieue-nodes.slice/k0s-vm-node-a.service",
            &[30],
        );
        f.cgroup(
            "banlieue.slice/banlieue-nodes.slice/k0s-vm-node-a-net.service",
            &[],
        );
        f.cgroup(
            r"machine.slice/machine-qemu\x2d2\x2dnode\x2db.scope/libvirt/emulator",
            &[40],
        );
        f.cgroup("system.slice/sshd.service", &[50]);
        let vms = f.sampler().discover();
        let names: Vec<String> = vms.keys().map(ToString::to_string).collect();
        assert_eq!(
            names,
            vec![
                "guest:2000001",
                "guest:2000002",
                "libvirt:node-b",
                "node:node-a",
            ]
        );
        assert_eq!(vms[&VmId::guest("2000001")].len(), 2, "VMM and swtpm units");
    }

    #[test]
    fn a_snapshot_reads_every_thread_of_every_process_in_the_tree() {
        let f = Fixture::new();
        let scope = r"machine.slice/machine-qemu\x2d2\x2dnode\x2db.scope";
        f.cgroup(&format!("{scope}/libvirt/emulator"), &[40]);
        f.cgroup(&format!("{scope}/libvirt"), &[41]);
        f.thread(40, "qemu-system-x86", 40, "qemu-system-x86", (5, 5, 0));
        f.thread(40, "qemu-system-x86", 42, "CPU 0/KVM", (80, 20, 70));
        f.thread(41, "swtpm", 41, "swtpm", (1, 1, 0));
        let s = f.sampler();
        let snap = s.snapshot(&[f.cgroups().join(scope)]);
        assert_eq!(snap.len(), 3);
        assert_eq!(
            snap[&(40, 42)],
            (
                Bucket::Vcpu,
                ThreadTimes {
                    total: 100,
                    guest: 70
                }
            )
        );
        assert_eq!(snap[&(41, 41)].0, Bucket::Swtpm);
    }

    /// A process that exits between listing and reading is skipped, not an
    /// error: VMs come and go while we sample.
    #[test]
    fn a_vanished_process_is_skipped() {
        let f = Fixture::new();
        f.cgroup("system.slice/banlieue-ch@2000001.service", &[10, 99]);
        f.thread(10, "cloud-hyperviso", 10, "vcpu0", (1, 1, 1));
        let snap = f
            .sampler()
            .snapshot(&[f.cgroups().join("system.slice/banlieue-ch@2000001.service")]);
        assert_eq!(snap.len(), 1);
    }

    #[test]
    fn the_slice_total_comes_from_its_cpu_stat() {
        let f = Fixture::new();
        let slice = f.cgroups().join(VMS_SLICE_UNIT);
        fs::create_dir_all(&slice).unwrap();
        fs::write(
            slice.join("cpu.stat"),
            "usage_usec 123456\nuser_usec 100000\nsystem_usec 23456\n",
        )
        .unwrap();
        assert_eq!(f.sampler().slice_usage_usec(), Some(123_456));
        let empty = Fixture::new();
        assert_eq!(empty.sampler().slice_usage_usec(), None);
    }

    // -------------------------------------------------------------- report

    #[test]
    fn the_report_is_percent_of_one_core() {
        let rows = vec![(
            VmId::guest("2000001"),
            Usage {
                guest: 100,
                vcpu_host: 50,
                vmm: 10,
                swtpm: 0,
            },
        )];
        // 100 ticks/s for 10 s: 100 ticks is 10% of one core.
        let text = report(&rows, 10.0, 100, Some(2_000_000.0));
        assert!(text.contains("guest:2000001"), "{text}");
        assert!(text.contains("10.0"), "guest column: {text}");
        assert!(text.contains("16.0"), "total column: {text}");
        assert!(text.contains("60.0%"), "overhead: {text}");
        assert!(text.contains("banlieue.slice"), "slice total: {text}");
        assert!(text.contains("20.0"), "2 s of CPU in 10 s is 20%: {text}");
    }

    #[test]
    fn an_empty_host_says_so() {
        let text = report(&[], 10.0, 100, None);
        assert!(text.contains("no VMs found"), "{text}");
    }
}
