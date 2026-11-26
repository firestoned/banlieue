// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The stages of `banlieue host cloud-hypervisor install`, and the read-only verbs
//! (ADR-0067 Decisions 1, 2 and 5).
//!
//! Each stage leaves the host as it should be and changes nothing that
//! already is: files identical in bytes, mode and owner are not rewritten,
//! existing users and records are kept, and the host config and EK CA are
//! regenerated only with `--force`. A second `install` therefore changes
//! nothing (invariant 2).

use crate::accounts::{self, GuestAccounts};
use crate::attestation::{self, CaInfo};
use crate::error::Error;
use crate::fetch::Fetch;
use crate::hardening;
use crate::openbao::{Credential, NoPki, Pki, SignRequest};
use crate::ops::{CgroupMode, Cmd, Host, Kind, Owner, Probe};
use crate::paths::{
    ATTESTATION_CA_DIR, ATTESTATION_CA_PENDING_DIR, BANLIEUE_USER, BIN_DIR, CONF_DIR,
    CREDENTIALS_DIR, EK_CA_CERT, EK_CA_DIR, GUEST_NAME_PREFIX, GUEST_SLICE_UNIT, HOST_CONFIG,
    KUBECONFIG_PATH, POLKIT_RULE, PROVIDER_BINARY, PROVIDER_UNIT, REGISTRY_CREDENTIALS_DIR,
    RUN_ROOT, STATE_ROOT, STATE_SUBDIRS, SWTPM_CONF_DIR, SWTPM_SETUP_CONF, SYSTEMD_UNIT_DIR,
    TMPFILES_CONF, UNIT_GLOBS, USERDB_DIR, VMM_BINARY,
};
use crate::pins::{self, Release};
use crate::render;
use crate::settings::{
    AttestationPlan, CaRoute, OpenBaoAuth, OpenBaoPlan, SYS_CLASS_NET, Settings,
};
use banlieue_provider_cloud_hypervisor::host_config::HostConfig;
use banlieue_provider_cloud_hypervisor::provider::HostFacts;
use clap::ValueEnum;
use std::path::{Path, PathBuf};

/// Directories only root writes.
const MODE_ROOT_DIR: u32 = 0o755;
/// Files anyone may read.
const MODE_PUBLIC_FILE: u32 = 0o644;
/// The host config: root writes, the provider reads.
const MODE_HOST_CONFIG: u32 = 0o640;
/// The provider's private directories.
const MODE_PRIVATE_DIR: u32 = 0o700;
/// The state root: traversable by guests (each reaches its own TPM state),
/// not listable.
const MODE_STATE_ROOT: u32 = 0o751;
/// A storage class root: guests traverse to their own directory, and
/// cannot list the others.
const MODE_STORAGE_ROOT: u32 = 0o711;
/// The image cache and registry credentials: the provider's, not guests'.
const MODE_PROVIDER_GROUP_DIR: u32 = 0o750;
/// EK CA private material.
const MODE_CA_PRIVATE: u32 = 0o600;
/// Executables.
const MODE_EXECUTABLE: u32 = 0o755;
/// The architecture the VMM pin is for.
const PINNED_ARCH: &str = "x86_64";
/// Bytes in a GiB, for free-space reporting.
const BYTES_PER_GIB: u64 = 1 << 30;
/// A scratch directory under the state root, the provider's and removed
/// after each use; never the shared `/tmp`.
const SCRATCH_DIR: &str = ".host-scratch";

/// Under a bridge in sysfs: its member ports.
const BRIDGE_PORTS_DIR: &str = "brif";
/// Under a bridge port in sysfs: present when the kernel can isolate it.
const BRPORT_ISOLATED: &str = "brport/isolated";

/// The host rules of ADR-0144 Decision 6. The recipe in the
/// `cloud-hypervisor-sandbox-segment` guide is the contract: these names and
/// positions must match it, and the other way round.
///
/// The program that lists the ruleset, and the flag that lists it.
const IPTABLES: &str = "iptables";
/// `iptables`' flag selecting a table, and its flag listing rules.
const IPTABLES_TABLE_FLAG: &str = "-t";
/// The list flag.
const IPTABLES_LIST_FLAG: &str = "-S";
/// The filter table, `iptables`' default.
const TABLE_FILTER: &str = "filter";
/// The nat table.
const TABLE_NAT: &str = "nat";
/// The sandbox segment's INPUT chain (filter).
const CHAIN_SBX_INPUT: &str = "BANLIEUE_SBX_INPUT";
/// The sandbox segment's FORWARD chain (filter).
const CHAIN_SBX_FORWARD: &str = "BANLIEUE_SBX_FORWARD";
/// The sandbox segment's NAT chain (nat).
const CHAIN_SBX_NAT: &str = "BANLIEUE_SBX_NAT";
/// Each owned chain, with its table and the built-in chain that must jump to
/// it from position 1.
const SBX_CONTRACT: [(&str, &str, &str); 3] = [
    (TABLE_FILTER, "INPUT", CHAIN_SBX_INPUT),
    (TABLE_FILTER, "FORWARD", CHAIN_SBX_FORWARD),
    (TABLE_NAT, "POSTROUTING", CHAIN_SBX_NAT),
];

/// The device the provider opens to create taps; the tmpfiles.d entry
/// creates it, and the provider unit's `DeviceAllow=` names it.
const TUN_DEVICE: &str = "/dev/net/tun";

/// The group owning `/dev/kvm`: guests join it, the provider must not.
const KVM_GROUP: &str = "kvm";
/// The CN the self-test's vTPM must carry: `<name>:<uuid>`, as ADR-0045
/// checks for a machine.
const SELFTEST_VMID_NAME: &str = "banlieue-selftest";
const SELFTEST_VMID_UUID: &str = "00000000-0000-4000-8000-000000000000";
/// The commands the host supplies, from whatever its OS installs packages
/// with (ADR-0084 Decision 1). banlieue installs none of them.
pub const REQUIRED_COMMANDS: [&str; 5] = [
    "swtpm",
    "swtpm_setup",
    "swtpm_localca",
    "systemctl",
    "systemd-tmpfiles",
];

/// A stage of `install`, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Stage {
    /// Is this host fit for guests, with the commands it must supply?
    /// Changes nothing.
    Preflight,
    /// The VMM, `ch-remote` and firmware, verified.
    Vmm,
    /// The provider's user, guest uid records, directories, host config.
    Host,
    /// The per-host attestation CA, for hosts that opt in (ADR-0146). Runs
    /// with `--only host` as well.
    Attestation,
    /// The per-host EK CA.
    Tpm,
    /// The polkit rule.
    Polkit,
    /// The template units and the provider unit.
    Provider,
    /// Proves the pieces work together. Boots nothing.
    Selftest,
}

impl Stage {
    /// Its name, as on the command line.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Vmm => "vmm",
            Self::Host => "host",
            Self::Attestation => "attestation",
            Self::Tpm => "tpm",
            Self::Polkit => "polkit",
            Self::Provider => "provider",
            Self::Selftest => "selftest",
        }
    }
}

/// How `install` runs.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// One stage only.
    pub only: Option<Stage>,
    /// Regenerate the host config and rotate the EK CA.
    pub force: bool,
    /// A banlieue binary to install as the provider.
    pub provider_binary: Option<PathBuf>,
    /// Print instead of change.
    pub dry_run: bool,
}

/// How the self-test learns what the provider would see on this host: the
/// provider's own `gather_facts`, or a stub in tests.
pub type FactsFn<'a> = &'a dyn Fn(&HostConfig) -> HostFacts;

fn log(line: impl AsRef<str>) {
    eprintln!("==> {}", line.as_ref());
}

fn note(line: impl AsRef<str>) {
    eprintln!("    {}", line.as_ref());
}

fn banlieue() -> Owner {
    Owner::new(BANLIEUE_USER, BANLIEUE_USER)
}

fn root_banlieue() -> Owner {
    Owner::new("root", BANLIEUE_USER)
}

/// Write `data` unless the file is already exactly that.
///
/// # Returns
/// Whether the file was written.
fn put(host: &dyn Host, path: &Path, data: &[u8], mode: u32, owner: &Owner) -> Result<bool, Error> {
    let same = host
        .stat(path)
        .is_some_and(|s| s.kind == Kind::File && s.mode == mode && &s.owner == owner)
        && host.read(path).is_ok_and(|d| d == data);
    if same {
        return Ok(false);
    }
    host.write(path, data, mode, owner)?;
    Ok(true)
}

/// Create or fix a directory unless it already is exactly that.
fn dir(host: &dyn Host, path: &Path, mode: u32, owner: &Owner) -> Result<(), Error> {
    let same = host
        .stat(path)
        .is_some_and(|s| s.kind == Kind::Dir && s.mode == mode && &s.owner == owner);
    if !same {
        host.mkdir(path, mode, owner)?;
    }
    Ok(())
}

/// A directory another package or the OS owns (`/etc/systemd/system`,
/// polkit's rules directory, `/usr/local/bin`…): created `0755 root:root`
/// when missing, otherwise left exactly as it is.
fn shared_dir(host: &dyn Host, path: &Path) -> Result<(), Error> {
    if !host.exists(path) {
        host.mkdir(path, MODE_ROOT_DIR, &Owner::root())?;
    }
    Ok(())
}

fn link(host: &dyn Host, target: &Path, at: &Path) -> Result<(), Error> {
    let same = host
        .stat(at)
        .is_some_and(|s| s.kind == Kind::Symlink(target.to_path_buf()));
    if !same {
        host.symlink(target, at)?;
    }
    Ok(())
}

fn getent_field(line: &str, index: usize) -> Option<&str> {
    line.split(':').nth(index)
}

/// Is the provider's user a member of the kvm group?
fn provider_in_kvm<P: Probe + ?Sized>(probe: &P) -> bool {
    probe.getent("group", KVM_GROUP).is_some_and(|line| {
        getent_field(&line, 3).is_some_and(|m| m.split(',').any(|u| u == BANLIEUE_USER))
    })
}

// ---------------------------------------------------------------- preflight

/// Is this host fit for guests? Every problem, not just the first.
///
/// # Errors
/// [`Error::Preflight`] listing every problem.
pub fn preflight(probe: &dyn Probe, s: &Settings) -> Result<(), Error> {
    log("Preflight");
    let mut bad = Vec::new();
    match probe.virtualization() {
        Some(v) if s.allow_virtualized => {
            note(format!("running inside a VM ({v}); allowed, unsupported"));
        }
        Some(v) => bad.push(format!(
            "running inside a VM ({v}); this provider targets bare-metal KVM \
             (--allow-virtualized-host for a lab)"
        )),
        None => {}
    }
    if !probe.exists(Path::new("/dev/kvm")) {
        bad.push("/dev/kvm is missing (VT-x/AMD-V in firmware? the kvm module?)".into());
    }
    if probe.getent("group", KVM_GROUP).is_none() {
        bad.push("no kvm group".into());
    }
    let arch = probe.arch();
    if arch != PINNED_ARCH {
        bad.push(format!("{arch}: only {PINNED_ARCH} is pinned"));
    }
    for c in REQUIRED_COMMANDS {
        if probe.which(c).is_none() {
            bad.push(format!(
                "{c} is not on PATH: install it with this host's package manager (systemd, swtpm, swtpm-tools)"
            ));
        }
    }
    if s.network.is_empty() {
        bad.push(format!(
            "no network class, and no {} bridge: create a bridge, then --network-class default=<bridge>; \
             banlieue never creates one",
            crate::settings::DEFAULT_BRIDGE
        ));
    }
    for (name, bridge) in &s.network {
        if probe.is_dir(&Path::new(SYS_CLASS_NET).join(bridge).join("bridge")) {
            note(format!("network class {name} -> bridge {bridge}"));
        } else {
            bad.push(format!(
                "network class {name} -> {bridge}: not a bridge on this host"
            ));
        }
    }
    bad.extend(isolation_problems(probe, s));
    bad.extend(firewall_problems(probe, s));
    let (base, end) = (s.uid_base, s.uid_end());
    for line in probe.getent_all("passwd") {
        let (Some(name), Some(uid)) = (
            getent_field(&line, 0),
            getent_field(&line, 2).and_then(|u| u.parse::<u32>().ok()),
        ) else {
            continue;
        };
        // Our own guest records (a re-run) are not a conflict.
        if (base..=end).contains(&uid) && !name.starts_with(GUEST_NAME_PREFIX) {
            bad.push(format!(
                "account {name} uses uid {uid}, inside the guest range {base}-{end}"
            ));
        }
    }
    note(format!(
        "guest accounts: {} ({})",
        s.guest_accounts.name(),
        s.accounts_reason
    ));
    if s.guest_accounts == GuestAccounts::Files {
        note(format!(
            "files mode creates {} users and {} private groups in this host's own account \
             databases (system accounts, no login); lower --guest-uid-count to create fewer",
            s.uid_count, s.uid_count
        ));
    }
    if s.attestation.is_some() {
        match attestation_state(probe) {
            Some(Ok(ca)) if ca.days_remaining < 0 => bad.push(format!(
                "attestation CA {} expired on {}: rotate it with --rotate-attestation-ca and \
                 a new --attestation-ca-name (ADR-0146 Decision 7)",
                ca.name, ca.not_after
            )),
            Some(Ok(ca)) => note(format!(
                "attestation CA {} (expires {}, {})",
                ca.name,
                ca.not_after,
                attestation::expiry_phrase(ca.days_remaining)
            )),
            Some(Err(problems)) => bad.extend(problems),
            None => note("attestation CA: will be created"),
        }
    }
    match pending_request(probe) {
        Some(Ok(name)) => note(format!(
            "attestation CA request for {name} is waiting for its signed certificate \
             (--attestation-ca-import FILE)"
        )),
        Some(Err(why)) => bad.push(format!(
            "pending attestation CA request in {ATTESTATION_CA_PENDING_DIR}: {why}"
        )),
        None => {}
    }
    bad.extend(accounts::problems(probe, s));
    bad.extend(hardening_problems(probe, s));
    for f in ["/etc/subuid", "/etc/subgid"] {
        let text = probe
            .read(Path::new(f))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        for line in text.lines() {
            let mut parts = line.split(':');
            let (Some(_), Some(start), Some(count)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let (Ok(start), Ok(count)) = (start.parse::<u64>(), count.parse::<u64>()) else {
                continue;
            };
            if count > 0 && start <= u64::from(end) && start + count > u64::from(base) {
                bad.push(format!(
                    "{f} range {start}+{count} overlaps guest uids {base}-{end}"
                ));
            }
        }
    }
    if !bad.is_empty() {
        return Err(Error::Preflight(bad));
    }
    note("preflight ok");
    Ok(())
}

/// Whether the kernel can isolate bridge ports, for each class declared
/// `isolate_ports` (ADR-0144 Decision 3): isolation that is configured and not
/// enforced is a refusal, never a silent pass.
///
/// A kernel that supports it exposes `<port>/brport/isolated` on every bridge
/// port, so one member port of the class's bridge is the probe. A bridge with
/// no member port has nothing to probe and is refused too.
fn isolation_problems(probe: &dyn Probe, s: &Settings) -> Vec<String> {
    let mut bad = Vec::new();
    for (name, bridge) in s
        .network
        .iter()
        .filter(|(name, _)| s.isolate_ports.contains(name))
    {
        let net = Path::new(SYS_CLASS_NET);
        let ports = probe.list(&net.join(bridge).join(BRIDGE_PORTS_DIR));
        let Some(port) = ports.first() else {
            bad.push(format!(
                "network class {name} -> {bridge}: isolate_ports is set but the bridge has no member port,                  so the kernel's port isolation cannot be probed: attach a port first (the installed hosts keep                  a dummy anchor port on their bridges)"
            ));
            continue;
        };
        if probe.exists(&net.join(port).join(BRPORT_ISOLATED)) {
            note(format!(
                "network class {name}: kernel port isolation available"
            ));
            continue;
        }
        bad.push(format!(
            "network class {name} -> {bridge}: isolate_ports is set but this kernel has no \
             {BRPORT_ISOLATED} on bridge port {port}: drop isolate_ports for the class or run a kernel \
             with bridge port isolation (Linux 4.18 or later with CONFIG_BRIDGE)"
        ));
    }
    bad
}

/// Whether the host carries the rules of ADR-0144 Decision 6, for each class
/// declared `isolate_ports`: port isolation stops a sandbox reaching another
/// sandbox, and these rules stop it reaching the host and the uplink.
///
/// Read-only (`iptables -S`, through [`Probe::query`]). A ruleset that cannot
/// be read fails closed. A class that is not isolated is never probed.
fn firewall_problems(probe: &dyn Probe, s: &Settings) -> Vec<String> {
    let classes: Vec<&String> = s
        .network
        .iter()
        .map(|(name, _)| name)
        .filter(|name| s.isolate_ports.contains(name))
        .collect();
    if classes.is_empty() {
        return Vec::new();
    }
    let issues = ruleset_issues(probe);
    let mut bad = Vec::new();
    for name in classes {
        if issues.is_empty() {
            note(format!(
                "network class {name}: sandbox segment host rules present"
            ));
        }
        bad.extend(issues.iter().map(|issue| {
            format!(
                "network class {name}: isolate_ports is set but {issue}; install the host rules \
                 of the cloud-hypervisor-sandbox-segment guide first (ADR-0144 Decision 6)"
            )
        }));
    }
    bad
}

/// What is wrong with the host ruleset against [`SBX_CONTRACT`], independent
/// of any class.
fn ruleset_issues(probe: &dyn Probe) -> Vec<String> {
    let mut issues = Vec::new();
    for table in [TABLE_FILTER, TABLE_NAT] {
        let args: Vec<&str> = if table == TABLE_FILTER {
            vec![IPTABLES_LIST_FLAG]
        } else {
            vec![IPTABLES_TABLE_FLAG, table, IPTABLES_LIST_FLAG]
        };
        let Some(text) = probe.query(&Cmd::new(IPTABLES, &args)) else {
            issues.push(format!(
                "the {table} table cannot be read ({IPTABLES} {} failed or {IPTABLES} is not installed), \
                 so the rules cannot be verified",
                args.join(" ")
            ));
            continue;
        };
        for (_, hook, chain) in SBX_CONTRACT.iter().filter(|(t, ..)| *t == table) {
            issues.extend(chain_issue(&text, table, hook, chain));
        }
    }
    issues
}

/// The problem with one owned chain in one table's `iptables -S` output:
/// absent, not jumped to, or jumped to from somewhere other than position 1
/// of its built-in chain.
fn chain_issue(text: &str, table: &str, hook: &str, chain: &str) -> Option<String> {
    let declared = format!("-N {chain}");
    if !text.lines().any(|l| l.trim() == declared) {
        return Some(format!("chain {chain} is absent from the {table} table"));
    }
    let jump = ["-A", hook, "-j", chain];
    let mut hook_rules = text
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .filter(|w| w.first() == Some(&"-A") && w.get(1) == Some(&hook));
    if hook_rules.next().is_some_and(|w| w == jump) {
        return None;
    }
    let jumped = text
        .lines()
        .any(|l| l.split_whitespace().collect::<Vec<_>>() == jump);
    if jumped {
        return Some(format!(
            "the jump to {chain} is not first in {hook} ({table} table)"
        ));
    }
    Some(format!(
        "there is no jump to {chain} from {hook} ({table} table)"
    ))
}

/// What the host's systemd cannot enforce, against what the operator
/// accepted (ADR-0139 Decision 4).
fn hardening_problems(probe: &dyn Probe, s: &Settings) -> Vec<String> {
    hardening::problems(probe.systemd_version(), &s.accepted_reduced)
}

// ---------------------------------------------------------------------- vmm

/// Install the release's VMM, `ch-remote` and firmware. Every artifact is
/// fetched and verified before any is installed; on a mismatch nothing is
/// written and the symlinks stay as they were (invariant 1).
///
/// # Errors
/// [`Error::Pin`] on a digest mismatch, [`Error::Fetch`], or the I/O error.
pub async fn vmm(
    host: &dyn Host,
    fetch: &dyn Fetch,
    release: &Release,
    o: &Options,
) -> Result<(), Error> {
    log(format!(
        "VMM: cloud-hypervisor {}, firmware {}",
        release.version, release.firmware_tag
    ));
    let mut verified = Vec::new();
    for a in &release.artifacts {
        let current = host.read(&a.dest).map(|d| pins::sha256_hex(&d));
        if !o.force && current.as_deref().is_ok_and(|h| h == a.sha256) {
            note(format!("{} already installed", a.dest.display()));
            continue;
        }
        if o.dry_run {
            note(format!("would fetch {} and verify its sha256", a.name));
            continue;
        }
        let bytes = fetch.fetch(a).await?;
        let got = pins::sha256_hex(&bytes);
        if got != a.sha256 {
            return Err(Error::Pin {
                name: a.name,
                got,
                want: a.sha256.clone(),
            });
        }
        verified.push((a, bytes));
    }
    for (a, bytes) in &verified {
        if let Some(parent) = a.dest.parent() {
            dir(host, parent, MODE_ROOT_DIR, &Owner::root())?;
        }
        put(host, &a.dest, bytes, a.mode, &Owner::root())?;
        note(format!("installed {} (sha256 verified)", a.dest.display()));
    }
    shared_dir(host, Path::new(BIN_DIR))?;
    for (at, target) in &release.symlinks {
        link(host, target, at)?;
    }
    if let Some(v) = host.query(&Cmd::new(VMM_BINARY, &["--version"])) {
        note(v.lines().next().unwrap_or_default());
    }
    Ok(())
}

// --------------------------------------------------------------------- host

/// The provider's user, the guest uid records, every directory, and the
/// host config, whose `[vmm]` section names `release` (ADR-0084
/// Decision 5).
///
/// # Errors
/// The I/O error, or [`Error::Config`] if the host config would not load.
pub fn host(host: &dyn Host, s: &Settings, release: &Release, o: &Options) -> Result<(), Error> {
    log("Host");
    if host.getent("passwd", BANLIEUE_USER).is_none() {
        note(format!("creating system user {BANLIEUE_USER}"));
        host.run(&Cmd::new(
            "useradd",
            &[
                "--system",
                "--home-dir",
                STATE_ROOT,
                "--no-create-home",
                "--shell",
                "/usr/sbin/nologin",
                "--user-group",
                BANLIEUE_USER,
            ],
        ))?;
    }
    // The provider only checks that /dev/kvm exists, which needs no access;
    // guests join kvm themselves. Earlier installs added it: take it out.
    if provider_in_kvm(host) {
        host.run(&Cmd::new("gpasswd", &["-d", BANLIEUE_USER, KVM_GROUP]))?;
        note(format!(
            "{BANLIEUE_USER} left the {KVM_GROUP} group: it needs no access to /dev/kvm"
        ));
    }

    dir(host, Path::new(CONF_DIR), MODE_ROOT_DIR, &Owner::root())?;
    // The provider's own: it replaces its token here at half-life.
    dir(
        host,
        Path::new(CREDENTIALS_DIR),
        MODE_PRIVATE_DIR,
        &banlieue(),
    )?;
    if s.registry.is_some() {
        dir(
            host,
            Path::new(REGISTRY_CREDENTIALS_DIR),
            MODE_PROVIDER_GROUP_DIR,
            &root_banlieue(),
        )?;
    }
    dir(host, Path::new(STATE_ROOT), MODE_STATE_ROOT, &banlieue())?;
    for (sub, mode) in STATE_SUBDIRS {
        dir(host, &Path::new(STATE_ROOT).join(sub), mode, &banlieue())?;
    }

    // /run is tmpfs: tmpfiles.d recreates the run root at every boot.
    let values = render::values(s, host);
    if let Some(parent) = Path::new(TMPFILES_CONF).parent() {
        shared_dir(host, parent)?;
    }
    // It also creates the tun node, which the provider unit's DeviceAllow=
    // needs at start: apply it whenever it changed, not only at first boot.
    let tmpfiles = render::render(&render::TMPFILES, &values)?;
    let tmpfiles_changed = put(
        host,
        Path::new(TMPFILES_CONF),
        tmpfiles.as_bytes(),
        MODE_PUBLIC_FILE,
        &Owner::root(),
    )?;
    if tmpfiles_changed || !host.is_dir(Path::new(RUN_ROOT)) || !host.exists(Path::new(TUN_DEVICE))
    {
        host.run(&Cmd::new("systemd-tmpfiles", &["--create", TMPFILES_CONF]))?;
    }

    for (name, path) in &s.storage {
        dir(host, path, MODE_STORAGE_ROOT, &banlieue())?;
        dir(
            host,
            &path.join(banlieue_provider_cloud_hypervisor::plan::IMAGES_DIR),
            MODE_PROVIDER_GROUP_DIR,
            &banlieue(),
        )?;
        let free = host.free_bytes(path).map_or_else(
            || "free space unknown".into(),
            |b| format!("{} GiB free", b / BYTES_PER_GIB),
        );
        note(format!(
            "storage class {name} -> {} ({free})",
            path.display()
        ));
    }

    register_guest_uids(host, s, o)?;

    if !host.exists(Path::new(HOST_CONFIG)) || o.force {
        let text = render::host_config(s, host, release)?;
        put(
            host,
            Path::new(HOST_CONFIG),
            text.as_bytes(),
            MODE_HOST_CONFIG,
            &root_banlieue(),
        )?;
        note(format!("wrote {HOST_CONFIG}"));
        return Ok(());
    }
    note(format!(
        "{HOST_CONFIG} exists, keeping it (--force regenerates)"
    ));
    refresh_managed_sections(host, s, release, o)
}

/// An existing host config keeps everything but `[vmm]`, which follows
/// the release just installed, and `[hardening]`, which follows what the
/// operator accepted now, so neither needs `--force` and its EK CA rotation
/// (ADR-0084 Decision 5, ADR-0139). A file that does not parse is the
/// admin's to fix, and is left as it is.
fn refresh_managed_sections(
    host: &dyn Host,
    s: &Settings,
    release: &Release,
    o: &Options,
) -> Result<(), Error> {
    let existing = match host.read(Path::new(HOST_CONFIG)) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if o.dry_run => {
            note(format!("{HOST_CONFIG}: {e}; [vmm] not checked"));
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let updated = match render::with_managed(&existing, release, &s.accepted_reduced) {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(()),
        Err(e) => {
            note(format!(
                "!!! {HOST_CONFIG} is left as it is, its [vmm] not updated: {e}"
            ));
            return Ok(());
        }
    };
    put(
        host,
        Path::new(HOST_CONFIG),
        updated.as_bytes(),
        MODE_HOST_CONFIG,
        &root_banlieue(),
    )?;
    note(format!(
        "[vmm] in {HOST_CONFIG} now names cloud-hypervisor {} and firmware {}; \
         [hardening] accepts {}; restart {PROVIDER_UNIT} so it reads them",
        release.version,
        release.firmware_tag,
        accepted_display(&s.accepted_reduced)
    ));
    Ok(())
}

/// What was accepted, for a message.
fn accepted_display(accepted: &[String]) -> String {
    if accepted.is_empty() {
        return "nothing".into();
    }
    accepted.join(",")
}

/// One user and one private group per guest uid, registered the way the
/// settings chose (ADR-0139): systemd refuses `User=` for a uid NSS does
/// not know (status 217), so the range must resolve before any guest starts.
fn register_guest_uids(host: &dyn Host, s: &Settings, o: &Options) -> Result<(), Error> {
    match s.guest_accounts {
        GuestAccounts::Userdb => register_userdb(host, s)?,
        GuestAccounts::Files => {
            let added = accounts::register_files(host, s)?;
            note(format!(
                "guest uids {}-{} ({added} new users and groups in this host's own databases)",
                s.uid_base,
                s.uid_end()
            ));
        }
    }
    verify_guest_uids(host, s, o)
}

/// One userdb user and one private group per guest uid, as records in
/// `/etc/userdb` (ADR-0063 Decision 3).
fn register_userdb(host: &dyn Host, s: &Settings) -> Result<(), Error> {
    let db = Path::new(USERDB_DIR);
    dir(host, db, MODE_ROOT_DIR, &Owner::root())?;
    let mut added = 0usize;
    for uid in s.uid_base..=s.uid_end() {
        let name = format!("{GUEST_NAME_PREFIX}{uid}");
        let user_file = db.join(format!("{name}.user"));
        if host.exists(&user_file) {
            continue;
        }
        let (user, group) = render::guest_records(&name, uid);
        put(
            host,
            &user_file,
            user.as_bytes(),
            MODE_PUBLIC_FILE,
            &Owner::root(),
        )?;
        put(
            host,
            &db.join(format!("{name}.group")),
            group.as_bytes(),
            MODE_PUBLIC_FILE,
            &Owner::root(),
        )?;
        link(
            host,
            Path::new(&format!("{name}.user")),
            &db.join(format!("{uid}.user")),
        )?;
        link(
            host,
            Path::new(&format!("{name}.group")),
            &db.join(format!("{uid}.group")),
        )?;
        added += 1;
    }
    note(format!(
        "guest uids {}-{} ({added} new records in {USERDB_DIR})",
        s.uid_base,
        s.uid_end()
    ));
    Ok(())
}

/// The first and last guest uids resolve to their names through NSS.
fn verify_guest_uids(host: &dyn Host, s: &Settings, o: &Options) -> Result<(), Error> {
    if o.dry_run {
        return Ok(());
    }
    let first = host
        .getent("passwd", &s.uid_base.to_string())
        .and_then(|l| getent_field(&l, 0).map(str::to_string));
    let last = host
        .getent("group", &s.uid_end().to_string())
        .and_then(|l| getent_field(&l, 0).map(str::to_string));
    let want_first = format!("{GUEST_NAME_PREFIX}{}", s.uid_base);
    let want_last = format!("{GUEST_NAME_PREFIX}{}", s.uid_end());
    if first.as_deref() != Some(want_first.as_str()) || last.as_deref() != Some(want_last.as_str())
    {
        return Err(Error::Unsupported(format!(
            "NSS does not resolve the guest accounts ({} mode; for userdb, is 'systemd' in nsswitch.conf and are the records in {USERDB_DIR}?)",
            s.guest_accounts.name()
        )));
    }
    Ok(())
}

// -------------------------------------------------------------- attestation

/// The attestation CA on the host: `None` when there is none, its name and
/// expiry when it is sound, and every problem when it is not (ADR-0146
/// Decision 5). Sound is the CA crate's own `load` (key `0600`, a directory
/// closed to group and world, no symlinks, key and certificate matching),
/// plus ownership, which `load` cannot know: the key and the directory must
/// belong to the provider's user, or the provider cannot read its own key or
/// someone else can.
fn attestation_state(probe: &dyn Probe) -> Option<Result<CaInfo, Vec<String>>> {
    let dir = Path::new(ATTESTATION_CA_DIR);
    let key = dir.join(attestation::CA_KEY_FILE);
    let cert = dir.join(attestation::CA_CERT_FILE);
    if !probe.exists(&key) && !probe.exists(&cert) {
        return None;
    }
    let mut problems = Vec::new();
    for (what, path) in [("directory", dir), ("key", key.as_path())] {
        match probe.stat(path) {
            Some(st) if st.owner.user != BANLIEUE_USER => problems.push(format!(
                "attestation CA {what} {} is owned by {}, not {BANLIEUE_USER}",
                path.display(),
                st.owner.user
            )),
            _ => {}
        }
    }
    match probe.attestation_ca(dir) {
        Ok(info) if problems.is_empty() => return Some(Ok(info)),
        Ok(_) => {}
        Err(why) => problems.push(format!("attestation CA {}: {why}", dir.display())),
    }
    Some(Err(problems))
}

/// The attestation CA (ADR-0146 Decision 5): created once, as the
/// provider's user, so its key is born with the right owner; its section
/// written into the host config; and the administrator told what to copy
/// where. A CA that is there is never overwritten: a re-run reports it. It
/// is replaced only by `--rotate-attestation-ca` with a new name, and never
/// by `--force`. A host that did not opt in is left exactly as it is,
/// section included.
///
/// # Errors
/// The I/O error, or [`Error::Config`] if the host config would not load.
pub fn attestation_ca(host: &dyn Host, s: &Settings, o: &Options) -> Result<(), Error> {
    log("Attestation CA");
    let Some(plan) = &s.attestation else {
        note("not opted in (--attestation-ca-name NAME or --attestation); nothing changed");
        return Ok(());
    };
    match &plan.route {
        CaRoute::Csr => return attestation_csr(host, plan, o),
        CaRoute::Import(file) => return attestation_import(host, plan, file, o),
        // Async (it talks to OpenBao), so `install` runs it: see `attestation_openbao`.
        CaRoute::OpenBao(_) => {
            return Err(Error::Unsupported(
                "the OpenBao route runs through install".into(),
            ));
        }
        CaRoute::SelfSigned => {}
    }
    let dir = Path::new(ATTESTATION_CA_DIR);
    let present = host.exists(&dir.join(attestation::CA_KEY_FILE))
        || host.exists(&dir.join(attestation::CA_CERT_FILE));
    // A CA that is there is never overwritten, however the plan was made.
    let create = plan
        .new_ca_name
        .as_ref()
        .filter(|_| plan.rotate || !present);
    if let Some(name) = create {
        if plan.rotate {
            eprintln!(
                "!!! --rotate-attestation-ca: replacing the attestation CA with {name}; every \
                 certificate it signed stops verifying once the new CA is asserted"
            );
            host.remove_attestation_ca(dir)?;
        }
        note(format!("creating the attestation CA {name}"));
        host.create_attestation_ca(dir, name, &banlieue())?;
    }
    write_attestation_section(host, plan.ttl, o)?;
    if o.dry_run && plan.new_ca_name.is_some() {
        return Ok(());
    }
    let info = host
        .attestation_ca(dir)
        .map_err(|why| Error::Config(format!("attestation CA {}: {why}", dir.display())))?;
    note(format!(
        "attestation CA {} in {} (expires {}; kept, never overwritten)",
        info.name,
        dir.display(),
        info.not_after
    ));
    for line in attestation::admin_action(dir, &info.name) {
        note(line);
    }
    Ok(())
}

/// Make the CA's key and a request for an upstream PKI to sign (ADR-0146
/// Decision 7). The key is born owned by the provider's user. Writes no
/// certificate and no `[attestation]` section: the provider loads the CA at
/// start and stops if it cannot, so an unsigned request must not look
/// configured. A live CA is not touched.
fn attestation_csr(host: &dyn Host, plan: &AttestationPlan, o: &Options) -> Result<(), Error> {
    let name = plan
        .new_ca_name
        .as_ref()
        .ok_or_else(|| Error::Config("a request needs a name".into()))?;
    let pending = Path::new(ATTESTATION_CA_PENDING_DIR);
    note(format!(
        "creating the request for the attestation CA {name}"
    ));
    host.create_pending_attestation_ca(pending, name, &banlieue())?;
    // Beside a live CA (rotation) its section must survive a regenerated config.
    if plan.rotate {
        write_attestation_section(host, plan.ttl, o)?;
    }
    for line in attestation::csr_admin_action(pending, name) {
        note(line);
    }
    Ok(())
}

/// Install the certificate an upstream PKI signed for the pending request
/// (ADR-0146 Decision 7), after the CA crate's checks; then write
/// `[attestation]`. A refusal installs nothing and leaves the request.
fn attestation_import(
    host: &dyn Host,
    plan: &AttestationPlan,
    file: &Path,
    o: &Options,
) -> Result<(), Error> {
    let signed = host
        .read(file)
        .map_err(|e| Error::Config(format!("{}: {e}", file.display())))?;
    if plan.rotate {
        eprintln!(
            "!!! --rotate-attestation-ca: replacing the live attestation CA with the signed one; \
             every certificate the old CA signed stops verifying once the new CA is asserted"
        );
    }
    let pending = Path::new(ATTESTATION_CA_PENDING_DIR);
    let dir = Path::new(ATTESTATION_CA_DIR);
    note(format!(
        "importing {} as the attestation CA",
        file.display()
    ));
    host.import_attestation_ca(pending, dir, &signed, plan.rotate, &banlieue())?;
    write_attestation_section(host, plan.ttl, o)?;
    if o.dry_run {
        return Ok(());
    }
    let info = host
        .attestation_ca(dir)
        .map_err(|why| Error::Config(format!("attestation CA {}: {why}", dir.display())))?;
    note(format!(
        "attestation CA {} in {} (expires {})",
        info.name,
        dir.display(),
        info.not_after
    ));
    for line in attestation::admin_action(dir, &info.name) {
        note(line);
    }
    Ok(())
}

/// The most a ServiceAccount or OpenBao token file may weigh: a JWT is a few
/// KiB; anything near this is not one.
const MAX_CREDENTIAL_BYTES: usize = 32 * 1024;

/// A one-line secret from `path`: trimmed, never empty, never more than a
/// line, never absurdly long. Never echoed back in an error.
fn read_secret_line(host: &dyn Host, path: &Path) -> Result<String, std::io::Error> {
    let raw = host.read(path)?;
    let bad = |why: &str| std::io::Error::other(format!("{}: {why}", path.display()));
    if raw.len() > MAX_CREDENTIAL_BYTES {
        return Err(bad("too large to be a token"));
    }
    let text = String::from_utf8(raw).map_err(|_| bad("not text"))?;
    let line = text.trim();
    if line.is_empty() {
        return Err(bad("empty"));
    }
    if line.contains('\n') || line.contains('\r') {
        return Err(bad("more than one line"));
    }
    Ok(line.to_string())
}

/// What the installer logs in to OpenBao with (ADR-0146 Decision 8): the
/// host's own ServiceAccount token, or the one-use token file.
fn openbao_credential(host: &dyn Host, bao: &OpenBaoPlan) -> Result<Credential, Error> {
    match &bao.auth {
        OpenBaoAuth::Jwt { auth_mount, role } => {
            let path = Path::new(CREDENTIALS_DIR).join("token");
            let jwt = read_secret_line(host, &path).map_err(|e| {
                Error::Pki(format!(
                    "the host's ServiceAccount token: {e}; `banlieue bootstrap \
                     cloud-hypervisor-host` writes it, so copy its output to {CREDENTIALS_DIR} \
                     first and run this again (the request is kept)"
                ))
            })?;
            Ok(Credential::Jwt {
                auth_mount: auth_mount.clone(),
                role: role.clone(),
                jwt,
            })
        }
        OpenBaoAuth::TokenFile(file) => read_secret_line(host, file)
            .map(Credential::Token)
            .map_err(|e| Error::Pki(format!("the OpenBao token file: {e}"))),
    }
}

/// Make (or reuse) the request, have OpenBao sign it as the host's own
/// ServiceAccount, and install the result (ADR-0146 Decision 8). Everything
/// OpenBao returns goes through the same import checks as a certificate an
/// administrator carried over, so a wrong answer installs nothing. Any failure
/// leaves the request, so the next run signs the same key. A token file is
/// removed once read, whether or not the signing worked: a spent token is
/// useless and a leftover one is a lure.
async fn attestation_openbao(
    host: &dyn Host,
    pki: &dyn Pki,
    plan: &AttestationPlan,
    bao: &OpenBaoPlan,
    o: &Options,
) -> Result<(), Error> {
    log("Attestation CA");
    let name = plan
        .new_ca_name
        .as_ref()
        .ok_or_else(|| Error::Config("a request needs a name".into()))?;
    let pending = Path::new(ATTESTATION_CA_PENDING_DIR);
    let dir = Path::new(ATTESTATION_CA_DIR);
    if host.exists(&pending.join(attestation::CA_KEY_FILE)) {
        note(format!(
            "reusing the pending request for the attestation CA {name}"
        ));
    } else {
        note(format!(
            "creating the request for the attestation CA {name}"
        ));
        host.create_pending_attestation_ca(pending, name, &banlieue())?;
    }
    if o.dry_run {
        note(format!(
            "would have {} sign it on {} ({})",
            bao.addr,
            bao.mount,
            match &bao.auth {
                OpenBaoAuth::Jwt { .. } => "logging in as the host's ServiceAccount",
                OpenBaoAuth::TokenFile(_) => "with the token file",
            }
        ));
        return Ok(());
    }
    let credential = openbao_credential(host, bao)?;
    let csr = String::from_utf8(host.read(&pending.join(attestation::CA_CSR_FILE))?)
        .map_err(|_| Error::Pki("the pending request is not text".into()))?;
    note(format!("asking {} to sign it on {}", bao.addr, bao.mount));
    let outcome = pki
        .sign_intermediate(&SignRequest {
            csr_pem: &csr,
            common_name: name,
            ttl: &bao.ttl,
            credential: &credential,
        })
        .await;
    if let OpenBaoAuth::TokenFile(file) = &bao.auth {
        // Spent either way. A failed removal is reported, never silent.
        if let Err(e) = host.remove(file) {
            eprintln!(
                "!!! could not remove the spent OpenBao token file {}: {e}",
                file.display()
            );
        }
    }
    let bundle = outcome?;
    if plan.rotate {
        eprintln!(
            "!!! --rotate-attestation-ca: replacing the live attestation CA with the signed one; \
             every certificate the old CA signed stops verifying once the new CA is asserted"
        );
    }
    host.import_attestation_ca(pending, dir, bundle.as_bytes(), plan.rotate, &banlieue())?;
    write_attestation_section(host, plan.ttl, o)?;
    let info = host
        .attestation_ca(dir)
        .map_err(|why| Error::Config(format!("attestation CA {}: {why}", dir.display())))?;
    note(format!(
        "attestation CA {} in {} (expires {})",
        info.name,
        dir.display(),
        info.not_after
    ));
    for line in attestation::admin_action(dir, &info.name) {
        note(line);
    }
    Ok(())
}

/// Set `[attestation]` in the host config to `ttl` and the CA directory,
/// unless it already says exactly that.
fn write_attestation_section(
    host: &dyn Host,
    ttl: std::time::Duration,
    o: &Options,
) -> Result<(), Error> {
    let existing = match host.read(Path::new(HOST_CONFIG)) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if o.dry_run => {
            note(format!("{HOST_CONFIG}: {e}; [attestation] not checked"));
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let section = attestation::section(ttl);
    let Some(updated) = render::with_attestation(&existing, &section)? else {
        return Ok(());
    };
    put(
        host,
        Path::new(HOST_CONFIG),
        updated.as_bytes(),
        MODE_HOST_CONFIG,
        &root_banlieue(),
    )?;
    note(format!(
        "[attestation] in {HOST_CONFIG}: certificate_ttl_secs {}; restart {PROVIDER_UNIT} so it \
         reads it",
        section.certificate_ttl_secs
    ));
    Ok(())
}

// ---------------------------------------------------------------------- tpm

/// The per-host EK CA (ADR-0065 Decision 6): created once, as the
/// provider's user, so its key is born with the right owner. `--force`
/// rotates it, which invalidates every EK certificate this host issued.
///
/// # Errors
/// The I/O error, or `swtpm_setup` failing.
pub fn tpm(host: &dyn Host, o: &Options) -> Result<(), Error> {
    log("EK CA");
    let ca = Path::new(EK_CA_DIR);
    dir(host, ca, MODE_PRIVATE_DIR, &banlieue())?;
    dir(
        host,
        Path::new(SWTPM_CONF_DIR),
        MODE_ROOT_DIR,
        &Owner::root(),
    )?;
    if !host.exists(Path::new(SWTPM_SETUP_CONF)) || o.force {
        for (path, text) in render::swtpm_config(host) {
            put(
                host,
                &path,
                text.as_bytes(),
                MODE_PUBLIC_FILE,
                &Owner::root(),
            )?;
        }
    }
    if o.force {
        eprintln!(
            "!!! --force: rotating the EK CA; every EK certificate issued here stops verifying"
        );
        for f in host.list(ca) {
            if f.ends_with(".pem") || f == "certserial" {
                host.remove(&ca.join(f))?;
            }
        }
    }
    if !host.exists(Path::new(EK_CA_CERT)) || o.force {
        note("creating the per-host EK CA");
        with_scratch(host, |scratch| {
            host.run(
                &Cmd::new(
                    "swtpm_setup",
                    &[
                        "--tpm2",
                        "--tpmstate",
                        &scratch.display().to_string(),
                        "--create-ek-cert",
                        "--config",
                        SWTPM_SETUP_CONF,
                    ],
                )
                .as_user(BANLIEUE_USER),
            )
            .map(drop)
            .map_err(Error::from)
        })?;
    }
    for f in host.list(ca) {
        let path = ca.join(&f);
        let mode = if path == Path::new(EK_CA_CERT) {
            MODE_PUBLIC_FILE
        } else if f.ends_with(".pem") {
            MODE_CA_PRIVATE
        } else {
            continue;
        };
        if host.stat(&path).is_some_and(|s| s.mode != mode) {
            host.set_mode(&path, mode)?;
        }
    }
    note(format!("EK CA certificate: {EK_CA_CERT}"));
    Ok(())
}

/// Run `f` with an empty scratch directory the provider's user owns, under
/// the state root, and remove it afterwards whatever happened.
fn with_scratch<T>(host: &dyn Host, f: impl FnOnce(&Path) -> Result<T, Error>) -> Result<T, Error> {
    let scratch = Path::new(STATE_ROOT).join(SCRATCH_DIR);
    host.remove(&scratch)?;
    host.mkdir(&scratch, MODE_PRIVATE_DIR, &banlieue())?;
    let out = f(&scratch);
    host.remove(&scratch)?;
    out
}

// ------------------------------------------------------------------- polkit

/// The rule letting the provider's user start, stop and reset only
/// instances of its templates, for uids in the guest range.
///
/// # Errors
/// The I/O error, or a template error.
pub fn polkit(host: &dyn Host, s: &Settings) -> Result<(), Error> {
    log("polkit rule");
    let rule = Path::new(POLKIT_RULE);
    if let Some(parent) = rule.parent() {
        shared_dir(host, parent)?;
    }
    let text = render::render(&render::POLKIT_RULE, &render::values(s, host))?;
    put(
        host,
        rule,
        text.as_bytes(),
        MODE_PUBLIC_FILE,
        &Owner::root(),
    )?;
    note(format!(
        "{POLKIT_RULE} (uids {}-{})",
        s.uid_base,
        s.uid_end()
    ));
    Ok(())
}

// ----------------------------------------------------------------- provider

/// The template units and the provider unit. The provider is enabled only
/// when its binary and kubeconfig both exist (invariant 5), and a running
/// provider is restarted when its unit or binary changed, so neither waits
/// for a reboot. Guests keep running: `KillMode=process` (ADR-0063
/// Decision 1).
///
/// # Errors
/// The I/O error, a template error, or `systemctl` failing.
pub fn provider(host: &dyn Host, s: &Settings, o: &Options) -> Result<(), Error> {
    log("Units");
    let mut provider_changed = false;
    if let Some(src) = &o.provider_binary {
        let bytes = host.read(src)?;
        shared_dir(host, Path::new(BIN_DIR))?;
        provider_changed |= put(
            host,
            Path::new(PROVIDER_BINARY),
            &bytes,
            MODE_EXECUTABLE,
            &Owner::root(),
        )?;
        note(format!("installed {} as {PROVIDER_BINARY}", src.display()));
    }
    let units = Path::new(SYSTEMD_UNIT_DIR);
    shared_dir(host, units)?;
    let values = render::values(s, host);
    let mut templates_changed = false;
    for t in render::UNIT_TEMPLATES.iter().chain([&render::GUEST_SLICE]) {
        let text = hardening::drop_directives(&render::render(t, &values)?, &s.accepted_reduced);
        templates_changed |= put(
            host,
            &units.join(t.name),
            text.as_bytes(),
            MODE_PUBLIC_FILE,
            &Owner::root(),
        )?;
    }
    let unit = render::render(&render::PROVIDER_UNIT, &values)?;
    provider_changed |= put(
        host,
        &units.join(render::PROVIDER_UNIT.name),
        unit.as_bytes(),
        MODE_PUBLIC_FILE,
        &Owner::root(),
    )?;
    if templates_changed {
        note(
            "guest unit templates updated: a running guest keeps the sandbox it started with \
             until its next start",
        );
    }
    if !s.accepted_reduced.is_empty() {
        note(format!(
            "reduced hardening accepted: {} not enforced by this host's systemd, dropped from \
             the guest units and recorded in {HOST_CONFIG}",
            s.accepted_reduced.join(",")
        ));
    }
    if s.guest_slice.cpu_quota.is_some() && s.guest_slice.cpus.is_none() {
        note(
            "--guest-cpu-quota is a share of CPU time, not a set of cores: guests are not kept \
             off other workloads' cores and share SMT siblings with them (--guest-cpus pins, on \
             cgroup v2)",
        );
    }
    if s.guest_slice.is_unbounded() {
        note(format!(
            "{GUEST_SLICE_UNIT} sets no limit: guests may take all of this host's CPU and memory \
             (--guest-cpus, --guest-cpu-weight, --guest-cpu-quota, --guest-memory-max, --guest-tasks-max)"
        ));
    }
    let systemd = host.systemd_running();
    if systemd {
        host.run(&Cmd::new("systemctl", &["daemon-reload"]))?;
    } else {
        note("systemd is not running here: units installed, not loaded");
    }
    let runnable =
        host.exists(Path::new(PROVIDER_BINARY)) && host.exists(Path::new(KUBECONFIG_PATH));
    if !runnable {
        note(format!(
            "{PROVIDER_UNIT} installed, NOT enabled: it needs {PROVIDER_BINARY} and {KUBECONFIG_PATH}"
        ));
        note(format!(
            "issue the credential in the cluster: banlieue bootstrap cloud-hypervisor-host --provider {} \
             --output-dir <dir>, then copy both files to {CREDENTIALS_DIR}",
            s.provider_name
        ));
        return Ok(());
    }
    if !systemd {
        note(format!(
            "enable it once systemd runs: systemctl enable --now {PROVIDER_UNIT}"
        ));
        return Ok(());
    }
    if host
        .query(&Cmd::new("systemctl", &["is-active", PROVIDER_UNIT]))
        .is_some_and(|st| st.trim() == "active")
    {
        if !provider_changed {
            note(format!("{PROVIDER_UNIT} running"));
            return Ok(());
        }
        host.run(&Cmd::new("systemctl", &["restart", PROVIDER_UNIT]))?;
        note(format!(
            "{PROVIDER_UNIT} restarted: its unit or binary changed (guests keep running)"
        ));
        return Ok(());
    }
    host.run(&Cmd::new("systemctl", &["enable", "--now", PROVIDER_UNIT]))?;
    note(format!("{PROVIDER_UNIT} enabled and started"));
    Ok(())
}

// ----------------------------------------------------------------- selftest

/// The subject CN of a DER certificate.
#[must_use]
pub fn certificate_cn(der: &[u8]) -> Option<String> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    cert.subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(str::to_string)
}

/// Prove the pieces work together without booting a guest: the VMM runs,
/// the firmware matches the release's digest, guest uids resolve, the tun
/// device the provider opens exists, the provider's user is not in the kvm
/// group (it needs no access to `/dev/kvm`), the host config loads and passes the
/// provider's own host checks, and a vTPM can be manufactured with the EK
/// CN a machine's would carry. Leaves nothing behind.
///
/// # Errors
/// [`Error::Selftest`] listing every failure.
pub fn selftest(
    host: &dyn Host,
    release: &Release,
    s: &Settings,
    facts: FactsFn<'_>,
) -> Result<(), Error> {
    log("Self-test");
    let mut bad = Vec::new();
    let ok = |line: String| note(line);

    if host.query(&Cmd::new(VMM_BINARY, &["--version"])).is_some() {
        ok(format!("{VMM_BINARY} runs"));
    } else {
        bad.push(format!("{VMM_BINARY} does not run"));
    }
    match host.read(&release.firmware) {
        Ok(d) if pins::sha256_hex(&d) == release.firmware_sha256 => {
            ok("firmware matches its pin".into())
        }
        Ok(_) => bad.push("firmware does not match its pin".into()),
        Err(e) => bad.push(format!("firmware: {e}")),
    }
    let base = s.uid_base.to_string();
    if host
        .getent("passwd", &base)
        .is_some_and(|l| getent_field(&l, 2) == Some(base.as_str()))
    {
        ok(format!("guest uid {base} resolves"));
    } else {
        bad.push(format!(
            "guest uid {base} does not resolve (systemd would fail it with 217/USER)"
        ));
    }
    if host.exists(Path::new(TUN_DEVICE)) {
        ok(format!("{TUN_DEVICE} exists"));
    } else {
        bad.push(format!(
            "{TUN_DEVICE} is missing (systemd-tmpfiles --create {TMPFILES_CONF})"
        ));
    }
    if provider_in_kvm(host) {
        bad.push(format!(
            "{BANLIEUE_USER} is in the {KVM_GROUP} group; it needs no access to /dev/kvm (re-run install)"
        ));
    } else {
        ok(format!("{BANLIEUE_USER} is not in the {KVM_GROUP} group"));
    }

    // The provider's own view of this host.
    match host
        .read(Path::new(HOST_CONFIG))
        .map_err(|e| e.to_string())
        .and_then(|b| HostConfig::parse(&String::from_utf8_lossy(&b)).map_err(|e| e.to_string()))
    {
        Ok(config) => {
            let f = facts(&config);
            let mut gaps = Vec::new();
            for (present, what) in [
                (f.kvm, "/dev/kvm"),
                (f.vmm_binary, "the VMM binary"),
                (f.firmware, "the firmware"),
                (f.vtpm, "a usable vTPM ([tpm])"),
            ] {
                if !present {
                    gaps.push(what.to_string());
                }
            }
            for c in config
                .storage_classes
                .keys()
                .filter(|c| !f.storage_present.contains(*c))
            {
                gaps.push(format!("storage class {c}"));
            }
            for c in config
                .network_classes
                .keys()
                .filter(|c| !f.bridges_present.contains(*c))
            {
                gaps.push(format!("network class {c}"));
            }
            if gaps.is_empty() {
                ok("the provider's host checks pass".into());
            } else {
                bad.push(format!(
                    "the provider would find missing: {}",
                    gaps.join(", ")
                ));
            }
        }
        Err(e) => bad.push(format!("{HOST_CONFIG}: {e}")),
    }

    let want = banlieue_provider_sdk::ek::expected_ek_cn(SELFTEST_VMID_NAME, SELFTEST_VMID_UUID);
    let tpm = with_scratch(host, |scratch| {
        let state = scratch.join("state");
        host.mkdir(&state, MODE_PRIVATE_DIR, &banlieue())?;
        host.run(
            &Cmd::new(
                "swtpm_setup",
                &[
                    "--tpm2",
                    "--tpmstate",
                    &state.display().to_string(),
                    "--create-ek-cert",
                    "--vmid",
                    &want,
                    "--config",
                    SWTPM_SETUP_CONF,
                    "--write-ek-cert-files",
                    &scratch.display().to_string(),
                ],
            )
            .as_user(BANLIEUE_USER),
        )?;
        Ok(certificate_cn(&host.read(&scratch.join("ek-rsa2048.crt"))?))
    });
    match tpm {
        Ok(Some(cn)) if cn == want => ok(format!("vTPM manufactured, EK CN = {cn}")),
        Ok(cn) => bad.push(format!("EK CN is {cn:?}, want {want:?}")),
        Err(e) => bad.push(format!("swtpm_setup as {BANLIEUE_USER}: {e}")),
    }

    if !bad.is_empty() {
        return Err(Error::Selftest(bad));
    }
    note("self-test ok");
    Ok(())
}

// ------------------------------------------------------------------- status

/// Whether `path` exists. Without root, a file in a directory only the
/// provider's user can enter is unknowable, not missing.
fn present(probe: &dyn Probe, path: &str) -> &'static str {
    if probe.exists(Path::new(path)) {
        "present"
    } else if !probe.is_root() {
        "unknown (run as root)"
    } else {
        "MISSING"
    }
}

/// One line for `status`: the attestation CA's name and expiry, or why it
/// is not sound.
fn attestation_status(probe: &dyn Probe) -> String {
    match attestation_state(probe) {
        None if probe.is_root() => "none (opt in with --attestation-ca-name)".into(),
        None => "none, or unknown (run as root)".into(),
        Some(Ok(ca)) => format!(
            "{}, expires {} ({})",
            ca.name,
            ca.not_after,
            attestation::expiry_phrase(ca.days_remaining)
        ),
        Some(Err(problems)) => format!("REFUSED: {}", problems.join("; ")),
    }
}

/// An unsigned attestation CA request on the host (ADR-0146 Decision 7): its
/// name when there is one waiting, `None` when there is not, and why when it is
/// there but cannot be read.
fn pending_request(probe: &dyn Probe) -> Option<Result<String, String>> {
    let dir = Path::new(ATTESTATION_CA_PENDING_DIR);
    if !probe.exists(&dir.join(attestation::CA_KEY_FILE)) {
        return None;
    }
    Some(probe.pending_attestation_ca(dir))
}

fn pending_status(probe: &dyn Probe) -> String {
    match pending_request(probe) {
        None => "none".into(),
        Some(Ok(name)) => {
            format!("{name} awaiting a signed certificate (--attestation-ca-import FILE)")
        }
        Some(Err(why)) => format!("REFUSED: {why}"),
    }
}

/// What is installed. Changes nothing.
pub fn status(probe: &dyn Probe) {
    let row = |k: &str, v: &str| println!("  {k:<18} {v}");
    println!("--- host ---");
    row("arch", &probe.arch());
    row(
        "virtualization",
        &probe.virtualization().unwrap_or_else(|| "none".into()),
    );
    row(
        "systemd",
        if probe.systemd_running() {
            "running"
        } else {
            "not running"
        },
    );
    row(
        "systemd-version",
        &probe
            .systemd_version()
            .map_or_else(|| "unknown".into(), |v| v.to_string()),
    );
    row(
        "cgroup",
        match probe.cgroup_mode() {
            CgroupMode::Unified => "v2 (unified)",
            CgroupMode::Legacy => "v1 (AllowedCPUs= is not enforced)",
        },
    );
    println!("--- vmm ---");
    row(
        "cloud-hypervisor",
        probe
            .query(&Cmd::new(VMM_BINARY, &["--version"]))
            .as_deref()
            .map_or("MISSING", |v| v.lines().next().unwrap_or_default()),
    );
    // What the host config says was installed; unreadable without root.
    let configured = probe
        .read(Path::new(HOST_CONFIG))
        .ok()
        .and_then(|b| HostConfig::parse(&String::from_utf8_lossy(&b)).ok())
        .map(|c| c.vmm);
    row(
        "installed",
        configured
            .as_ref()
            .map_or("unknown (no readable host config)", |v| v.version.as_str()),
    );
    row("pinned", pins::VMM_VERSION);
    let firmware =
        configured.map_or_else(|| pins::firmware_path(pins::FIRMWARE_TAG), |v| v.firmware);
    row(
        "firmware",
        &format!(
            "{} ({})",
            present(probe, &firmware.display().to_string()),
            firmware.display()
        ),
    );
    row(
        "swtpm",
        probe
            .query(&Cmd::new("swtpm", &["--version"]))
            .as_deref()
            .map_or("MISSING", |v| v.lines().next().unwrap_or_default()),
    );
    println!("--- config ---");
    row("host-config", present(probe, HOST_CONFIG));
    row("ek-ca", present(probe, EK_CA_CERT));
    row("attestation-ca", &attestation_status(probe));
    row("attestation-csr", &pending_status(probe));
    row("polkit-rule", present(probe, POLKIT_RULE));
    row(
        "kubeconfig",
        match present(probe, KUBECONFIG_PATH) {
            "MISSING" => "absent (banlieue bootstrap cloud-hypervisor-host)",
            other => other,
        },
    );
    println!("--- provider ---");
    row(
        "unit",
        probe
            .query(&Cmd::new("systemctl", &["is-active", PROVIDER_UNIT]))
            .as_deref()
            .map_or("unknown", str::trim),
    );
    println!("--- guests ---");
    let mut args = vec!["list-units", "--no-legend", "--plain"];
    args.extend(UNIT_GLOBS);
    for line in probe
        .query(&Cmd::new("systemctl", &args))
        .unwrap_or_default()
        .lines()
    {
        println!("  {line}");
    }
}

// -------------------------------------------------------- order and install

/// What `stage` builds on, if it is missing (ADR-0067 Decision 2).
#[must_use]
pub fn missing_prerequisites(stage: Stage, probe: &dyn Probe, release: &Release) -> Vec<String> {
    let commands = || REQUIRED_COMMANDS.iter().all(|c| probe.which(c).is_some());
    let vmm = || probe.exists(Path::new(VMM_BINARY)) && probe.exists(&release.firmware);
    let host =
        || probe.getent("passwd", BANLIEUE_USER).is_some() && probe.exists(Path::new(HOST_CONFIG));
    let tpm = || probe.exists(Path::new(EK_CA_CERT));
    let polkit = || probe.exists(Path::new(POLKIT_RULE));
    let needs: &[(Stage, &dyn Fn() -> bool)] = match stage {
        Stage::Preflight | Stage::Vmm | Stage::Host => &[],
        Stage::Attestation => &[(Stage::Host, &host)],
        Stage::Tpm | Stage::Polkit => &[(Stage::Preflight, &commands), (Stage::Host, &host)],
        Stage::Provider => &[(Stage::Host, &host)],
        Stage::Selftest => &[
            (Stage::Vmm, &vmm),
            (Stage::Host, &host),
            (Stage::Tpm, &tpm),
            (Stage::Polkit, &polkit),
        ],
    };
    needs
        .iter()
        .filter(|(_, done)| !done())
        .map(|(s, _)| s.name().to_string())
        .collect()
}

/// The stages `install` runs, in order.
#[must_use]
pub fn plan(o: &Options) -> Vec<Stage> {
    if o.only == Some(Stage::Host) {
        // The host config is the host stage's, and so is the section in it:
        // a host that opted in gets its CA with it.
        return vec![Stage::Host, Stage::Attestation];
    }
    if let Some(only) = o.only {
        return vec![only];
    }
    let mut order = vec![
        Stage::Preflight,
        Stage::Vmm,
        Stage::Host,
        Stage::Attestation,
        Stage::Tpm,
        Stage::Polkit,
        Stage::Provider,
        Stage::Selftest,
    ];
    if o.dry_run {
        // The self-test manufactures a TPM: an action, not a report.
        order.retain(|s| *s != Stage::Selftest);
    }
    order
}

/// `banlieue host cloud-hypervisor install`.
///
/// # Errors
/// The first stage's error; or [`Error::Prerequisite`] for `--only` a
/// stage whose prerequisites are missing.
pub async fn install(
    host: &dyn Host,
    fetch: &dyn Fetch,
    release: &Release,
    s: &Settings,
    o: &Options,
    facts: FactsFn<'_>,
) -> Result<(), Error> {
    install_with_pki(host, fetch, &NoPki, release, s, o, facts).await
}

/// As [`install`], with an OpenBao client for the attestation CA's OpenBao
/// route (ADR-0146 Decision 8). Every other route never calls it.
///
/// # Errors
/// As [`install`]; the OpenBao route's failures are [`Error::Pki`].
pub async fn install_with_pki(
    host: &dyn Host,
    fetch: &dyn Fetch,
    pki: &dyn Pki,
    release: &Release,
    s: &Settings,
    o: &Options,
    facts: FactsFn<'_>,
) -> Result<(), Error> {
    if let Some(only) = o.only {
        let missing = missing_prerequisites(only, host, release);
        if !missing.is_empty() && !o.dry_run {
            return Err(Error::Prerequisite {
                stage: only.name(),
                missing,
            });
        }
        // A single stage skips the preflight stage, not this refusal.
        let reduced = hardening_problems(host, s);
        if only != Stage::Preflight && !reduced.is_empty() {
            return Err(Error::Preflight(reduced));
        }
    }
    for stage in plan(o) {
        match stage {
            Stage::Preflight => preflight(host, s)?,
            Stage::Vmm => vmm(host, fetch, release, o).await?,
            Stage::Host => self::host(host, s, release, o)?,
            Stage::Attestation => match s.attestation.as_ref().map(|p| (&p.route, p)) {
                Some((CaRoute::OpenBao(bao), plan)) => {
                    attestation_openbao(host, pki, plan, bao, o).await?;
                }
                _ => attestation_ca(host, s, o)?,
            },
            Stage::Tpm => tpm(host, o)?,
            Stage::Polkit => polkit(host, s)?,
            Stage::Provider => provider(host, s, o)?,
            Stage::Selftest => selftest(host, release, s, facts)?,
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "stages_tests.rs"]
mod stages_tests;
