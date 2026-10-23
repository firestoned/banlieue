// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue host cloud-hypervisor overhead` (ADR-0106): what the VMs on
//! this host cost it, per VM, split into guest work and host-side overhead.
//!
//! Every thread of every VM is read twice from `/proc`, `--seconds` apart.
//! Its CPU time in between goes into one of four columns:
//!
//! - **guest**: the vCPU threads' `guest_time`, i.e. guest code running;
//! - **vCPU host**: the rest of the vCPU threads' time (KVM exits, halt
//!   polling, in-kernel emulation);
//! - **VMM**: the VMM's other threads (devices, I/O queues, the API);
//! - **swtpm**: the vTPM emulator.
//!
//! VMs are found by cgroup name wherever they run: banlieue guests
//! (`banlieue-ch@<uid>` with `banlieue-swtpm@<uid>`), the k0s bootstrap's
//! nodes (`k0s-vm-<name>` with `-swtpm`), and libvirt domains
//! (`machine-qemu…<name>.scope`), so a host part-way through a move from
//! libvirt can compare the two. Read-only: it opens files and changes
//! nothing, and needs no root, since `/proc/*/task/*/stat` and cgroup files
//! are world-readable.

use crate::error::Error;
use crate::paths::VMS_SLICE_UNIT;
use clap::Args;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where cgroup v2 is mounted.
pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";
/// Where procfs is mounted.
pub const PROC_ROOT: &str = "/proc";
/// Sampling window when `--seconds` is not given.
pub const DEFAULT_SECONDS: u64 = 60;

/// The escaped `-` in a systemd scope name.
const ESCAPED_DASH: &str = r"\x2d";
const LIBVIRT_SCOPE_PREFIX: &str = r"machine-qemu\x2d";
const SCOPE_SUFFIX: &str = ".scope";
const SERVICE_SUFFIX: &str = ".service";
const GUEST_VMM_PREFIX: &str = "banlieue-ch@";
const GUEST_SWTPM_PREFIX: &str = "banlieue-swtpm@";
const NODE_PREFIX: &str = "k0s-vm-";
const NODE_SWTPM_SUFFIX: &str = "-swtpm";
const NODE_NET_SUFFIX: &str = "-net";
/// Cloud Hypervisor names its vCPU threads `vcpu<N>`.
const CH_VCPU_PREFIX: &str = "vcpu";
/// QEMU names its vCPU threads `CPU <N>/KVM`.
const QEMU_VCPU_PREFIX: &str = "CPU ";
const QEMU_VCPU_SUFFIX: &str = "/KVM";
const SWTPM_COMM: &str = "swtpm";
/// `cpu.stat`'s total, in microseconds.
const CPU_STAT_USAGE: &str = "usage_usec";

/// `/proc/<pid>/task/<tid>/stat` fields, counted from the one after `comm`
/// (proc(5)'s field 3, the state, is index 0 here).
const STAT_FIRST_FIELD: usize = 3;
const STAT_UTIME: usize = 14 - STAT_FIRST_FIELD;
const STAT_STIME: usize = 15 - STAT_FIRST_FIELD;
const STAT_GUEST_TIME: usize = 43 - STAT_FIRST_FIELD;

const PERCENT: f64 = 100.0;
const MICROS_PER_SECOND: f64 = 1_000_000.0;
const NAME_WIDTH: usize = 28;
const COLUMN_WIDTH: usize = 10;

/// `banlieue host cloud-hypervisor overhead`.
#[derive(Debug, Args)]
pub struct OverheadArgs {
    /// How long to sample for. Longer is steadier; a busy guest settles in a
    /// minute.
    #[arg(long, default_value_t = DEFAULT_SECONDS)]
    pub seconds: u64,
}

/// Which kind of VM a cgroup belongs to.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum VmKind {
    /// A banlieue Cloud Hypervisor guest, named by its host uid.
    Guest,
    /// A libvirt domain.
    Libvirt,
    /// A k0s bootstrap management node on Cloud Hypervisor.
    Node,
}

/// One VM, as the report names it: `guest:<uid>`, `node:<name>` or
/// `libvirt:<name>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct VmId {
    /// Its kind.
    pub kind: VmKind,
    /// Its name within that kind.
    pub name: String,
}

impl VmId {
    /// A banlieue guest.
    #[must_use]
    pub fn guest(uid: &str) -> Self {
        Self {
            kind: VmKind::Guest,
            name: uid.to_string(),
        }
    }
    /// A k0s bootstrap node.
    #[must_use]
    pub fn node(name: &str) -> Self {
        Self {
            kind: VmKind::Node,
            name: name.to_string(),
        }
    }
    /// A libvirt domain.
    #[must_use]
    pub fn libvirt(name: &str) -> Self {
        Self {
            kind: VmKind::Libvirt,
            name: name.to_string(),
        }
    }
}

impl fmt::Display for VmId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            VmKind::Guest => "guest",
            VmKind::Libvirt => "libvirt",
            VmKind::Node => "node",
        };
        write!(f, "{kind}:{}", self.name)
    }
}

/// The VM a cgroup directory belongs to, from its name alone, or `None`
/// when it is not a VM's (a helper, a daemon, a slice).
#[must_use]
pub fn vm_of_cgroup(dir_name: &str) -> Option<VmId> {
    if let Some(rest) = dir_name.strip_suffix(SCOPE_SUFFIX) {
        let rest = rest.strip_prefix(LIBVIRT_SCOPE_PREFIX)?;
        let (_id, name) = rest.split_once(ESCAPED_DASH)?;
        return (!name.is_empty()).then(|| VmId::libvirt(&name.replace(ESCAPED_DASH, "-")));
    }
    let unit = dir_name.strip_suffix(SERVICE_SUFFIX)?;
    if let Some(uid) = unit
        .strip_prefix(GUEST_VMM_PREFIX)
        .or_else(|| unit.strip_prefix(GUEST_SWTPM_PREFIX))
    {
        return (!uid.is_empty()).then(|| VmId::guest(uid));
    }
    let node = unit.strip_prefix(NODE_PREFIX)?;
    if node.ends_with(NODE_NET_SUFFIX) {
        return None;
    }
    let node = node.strip_suffix(NODE_SWTPM_SUFFIX).unwrap_or(node);
    (!node.is_empty()).then(|| VmId::node(node))
}

/// Which column a thread's time goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bucket {
    /// A vCPU thread: split into guest and vCPU host.
    Vcpu,
    /// Any other VMM thread.
    Vmm,
    /// The vTPM emulator.
    Swtpm,
}

/// The column for a thread, from its process's and its own `comm`.
#[must_use]
pub fn bucket(process_comm: &str, thread_comm: &str) -> Bucket {
    if process_comm == SWTPM_COMM {
        return Bucket::Swtpm;
    }
    let ch_vcpu = thread_comm
        .strip_prefix(CH_VCPU_PREFIX)
        .is_some_and(all_digits);
    let qemu_vcpu = thread_comm
        .strip_prefix(QEMU_VCPU_PREFIX)
        .and_then(|r| r.strip_suffix(QEMU_VCPU_SUFFIX))
        .is_some_and(all_digits);
    if ch_vcpu || qemu_vcpu {
        return Bucket::Vcpu;
    }
    Bucket::Vmm
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// A thread's cumulative CPU time, in clock ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThreadTimes {
    /// User plus system time (user time includes guest time).
    pub total: u64,
    /// Time spent running guest code.
    pub guest: u64,
}

/// Parse a `stat` line. `comm` may hold spaces and parentheses, so fields
/// are counted from the last `)`.
#[must_use]
pub fn parse_stat(text: &str) -> Option<ThreadTimes> {
    let fields: Vec<&str> = text
        .get(text.rfind(')')? + 1..)?
        .split_whitespace()
        .collect();
    let field = |i: usize| fields.get(i)?.parse::<u64>().ok();
    Some(ThreadTimes {
        total: field(STAT_UTIME)? + field(STAT_STIME)?,
        guest: field(STAT_GUEST_TIME)?,
    })
}

/// Every thread of a VM at one instant, keyed by `(pid, tid)`.
pub type Snapshot = BTreeMap<(u32, u32), (Bucket, ThreadTimes)>;

/// One VM's CPU over the interval, in clock ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Guest code.
    pub guest: u64,
    /// The rest of the vCPU threads' time.
    pub vcpu_host: u64,
    /// The VMM's other threads.
    pub vmm: u64,
    /// The vTPM emulator.
    pub swtpm: u64,
}

impl Usage {
    /// Everything the VM cost the host.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.guest + self.overhead()
    }

    /// The host-side part: everything but guest code.
    #[must_use]
    pub fn overhead(&self) -> u64 {
        self.vcpu_host + self.vmm + self.swtpm
    }

    /// Host-side time per unit of guest time, or `None` for an idle guest.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // tick counts far below 2^52
    pub fn overhead_ratio(&self) -> Option<f64> {
        (self.guest > 0).then(|| self.overhead() as f64 / self.guest as f64)
    }
}

/// What happened between two snapshots. A thread that appeared counts from
/// zero; one that vanished counts nothing; a counter that went backwards (a
/// reused tid) counts nothing.
#[must_use]
pub fn usage(before: &Snapshot, after: &Snapshot) -> Usage {
    let mut u = Usage::default();
    for (key, (bucket, now)) in after {
        let then = before.get(key).map(|(_, t)| *t).unwrap_or_default();
        if now.total < then.total || now.guest < then.guest {
            continue;
        }
        let total = now.total - then.total;
        let guest = now.guest - then.guest;
        match bucket {
            Bucket::Vcpu => {
                u.guest += guest;
                u.vcpu_host += total.saturating_sub(guest);
            }
            Bucket::Vmm => u.vmm += total,
            Bucket::Swtpm => u.swtpm += total,
        }
    }
    u
}

/// Reads VMs from a cgroup tree and a procfs, which tests point at fixtures.
pub struct Sampler {
    cgroup_root: PathBuf,
    proc_root: PathBuf,
}

impl Sampler {
    /// A sampler over these mounts.
    #[must_use]
    pub fn new(cgroup_root: PathBuf, proc_root: PathBuf) -> Self {
        Self {
            cgroup_root,
            proc_root,
        }
    }

    /// Every VM under the cgroup root, with the cgroup directories that make
    /// it up. A VM's directory is not searched for further VMs.
    #[must_use]
    pub fn discover(&self) -> BTreeMap<VmId, Vec<PathBuf>> {
        let mut vms: BTreeMap<VmId, Vec<PathBuf>> = BTreeMap::new();
        let mut pending = vec![self.cgroup_root.clone()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                match vm_of_cgroup(&name) {
                    Some(vm) => vms.entry(vm).or_default().push(path),
                    None => pending.push(path),
                }
            }
        }
        for dirs in vms.values_mut() {
            dirs.sort();
        }
        vms
    }

    /// Every thread of every process in these cgroups and below. Processes
    /// that exit while being read are skipped.
    #[must_use]
    pub fn snapshot(&self, cgroups: &[PathBuf]) -> Snapshot {
        let mut snap = Snapshot::new();
        for pid in cgroups.iter().flat_map(|cg| pids_below(cg)) {
            let proc_dir = self.proc_root.join(pid.to_string());
            let Some(pcomm) = read_comm(&proc_dir.join("comm")) else {
                continue;
            };
            let Ok(tasks) = fs::read_dir(proc_dir.join("task")) else {
                continue;
            };
            for task in tasks.flatten() {
                let Some(tid) = task
                    .file_name()
                    .to_str()
                    .and_then(|t| t.parse::<u32>().ok())
                else {
                    continue;
                };
                let base = task.path();
                let (Some(tcomm), Some(times)) = (
                    read_comm(&base.join("comm")),
                    fs::read_to_string(base.join("stat"))
                        .ok()
                        .as_deref()
                        .and_then(parse_stat),
                ) else {
                    continue;
                };
                snap.insert((pid, tid), (bucket(&pcomm, &tcomm), times));
            }
        }
        snap
    }

    /// `banlieue.slice`'s cumulative CPU, in microseconds, or `None` when the
    /// slice does not exist (no VM has started in a banlieue slice).
    #[must_use]
    pub fn slice_usage_usec(&self) -> Option<u64> {
        let text =
            fs::read_to_string(self.cgroup_root.join(VMS_SLICE_UNIT).join("cpu.stat")).ok()?;
        text.lines().find_map(|l| {
            let (key, value) = l.split_once(' ')?;
            (key == CPU_STAT_USAGE).then(|| value.trim().parse().ok())?
        })
    }
}

/// Every pid in a cgroup and its descendants. Unreadable files (a threaded
/// leaf, a cgroup that vanished) contribute nothing.
fn pids_below(cgroup: &Path) -> Vec<u32> {
    let mut pids = Vec::new();
    let mut pending = vec![cgroup.to_path_buf()];
    while let Some(dir) = pending.pop() {
        if let Ok(text) = fs::read_to_string(dir.join("cgroup.procs")) {
            pids.extend(
                text.split_whitespace()
                    .filter_map(|p| p.parse::<u32>().ok()),
            );
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        pending.extend(entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

fn read_comm(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim_end().to_string())
}

/// The report: one row per VM, every value a percentage of one host core
/// over `seconds`, then the slice's own total when it exists.
#[must_use]
#[allow(clippy::cast_precision_loss)] // tick counts far below 2^52
pub fn report(
    rows: &[(VmId, Usage)],
    seconds: f64,
    ticks_per_second: u64,
    slice_cpu_usec: Option<f64>,
) -> String {
    let pct = |ticks: u64| PERCENT * ticks as f64 / ticks_per_second as f64 / seconds;
    let mut out =
        format!("interval {seconds:.1}s; values are % of ONE host core (100 = one core busy)\n");
    if rows.is_empty() {
        out.push_str("no VMs found (banlieue guests, k0s bootstrap nodes or libvirt domains)\n");
    } else {
        let w = COLUMN_WIDTH;
        out.push_str(&format!(
            "{:<NAME_WIDTH$}{:>w$}{:>w$}{:>w$}{:>w$}{:>w$}{:>w$}\n",
            "vm", "guest", "vcpu-host", "vmm", "swtpm", "total", "overhead"
        ));
        for (vm, u) in rows {
            let ratio = u
                .overhead_ratio()
                .map_or_else(|| "n/a".to_string(), |r| format!("{:.1}%", PERCENT * r));
            out.push_str(&format!(
                "{:<NAME_WIDTH$}{:>w$.1}{:>w$.1}{:>w$.1}{:>w$.1}{:>w$.1}{:>w$}\n",
                vm.to_string(),
                pct(u.guest),
                pct(u.vcpu_host),
                pct(u.vmm),
                pct(u.swtpm),
                pct(u.total()),
                ratio
            ));
        }
        out.push_str("overhead = (vcpu-host + vmm + swtpm) / guest\n");
    }
    if let Some(usec) = slice_cpu_usec {
        out.push_str(&format!(
            "{VMS_SLICE_UNIT} total: {:.1} (every VM running in a banlieue slice)\n",
            PERCENT * usec / MICROS_PER_SECOND / seconds
        ));
    }
    out
}

/// Run `overhead` against this host.
///
/// # Errors
/// [`Error::Setting`] for `--seconds 0`.
pub async fn run(args: &OverheadArgs) -> Result<(), Error> {
    if args.seconds == 0 {
        return Err(Error::Setting {
            name: "--seconds",
            why: "must be at least 1".into(),
        });
    }
    let sampler = Sampler::new(PathBuf::from(CGROUP_ROOT), PathBuf::from(PROC_ROOT));
    let vms = sampler.discover();
    let before: BTreeMap<&VmId, Snapshot> = vms
        .iter()
        .map(|(vm, cgs)| (vm, sampler.snapshot(cgs)))
        .collect();
    let slice_before = sampler.slice_usage_usec();
    let started = Instant::now();
    tokio::time::sleep(Duration::from_secs(args.seconds)).await;
    let seconds = started.elapsed().as_secs_f64();
    let rows: Vec<(VmId, Usage)> = vms
        .iter()
        .map(|(vm, cgs)| (vm.clone(), usage(&before[vm], &sampler.snapshot(cgs))))
        .collect();
    #[allow(clippy::cast_precision_loss)] // microsecond counters far below 2^52
    let slice = slice_before
        .zip(sampler.slice_usage_usec())
        .map(|(a, b)| b.saturating_sub(a) as f64);
    let ticks = rustix::param::clock_ticks_per_second();
    print!("{}", report(&rows, seconds, ticks, slice));
    Ok(())
}

#[cfg(test)]
#[path = "overhead_tests.rs"]
mod overhead_tests;
