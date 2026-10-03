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

use crate::error::Error;
use crate::fetch::Fetch;
use crate::ops::{Cmd, Host, Kind, Owner, Probe};
use crate::paths::{
    BANLIEUE_USER, BIN_DIR, CONF_DIR, CREDENTIALS_DIR, EK_CA_CERT, EK_CA_DIR, GUEST_NAME_PREFIX,
    HOST_CONFIG, KUBECONFIG_PATH, POLKIT_RULE, PROVIDER_BINARY, PROVIDER_UNIT,
    REGISTRY_CREDENTIALS_DIR, RUN_ROOT, STATE_ROOT, STATE_SUBDIRS, SWTPM_CONF_DIR,
    SWTPM_SETUP_CONF, SYSTEMD_UNIT_DIR, TMPFILES_CONF, UNIT_GLOBS, USERDB_DIR, VMM_BINARY,
};
use crate::pins::{self, Release};
use crate::render;
use crate::settings::{SYS_CLASS_NET, Settings};
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
fn put(host: &dyn Host, path: &Path, data: &[u8], mode: u32, owner: &Owner) -> Result<(), Error> {
    let same = host
        .stat(path)
        .is_some_and(|s| s.kind == Kind::File && s.mode == mode && &s.owner == owner)
        && host.read(path).is_ok_and(|d| d == data);
    if !same {
        host.write(path, data, mode, owner)?;
    }
    Ok(())
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
    if probe.getent("group", "kvm").is_none() {
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
    let nsswitch = probe
        .read(Path::new("/etc/nsswitch.conf"))
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    for db in ["passwd", "group"] {
        let has_systemd = nsswitch.lines().any(|l| {
            l.trim_start().starts_with(&format!("{db}:"))
                && l.split_whitespace().any(|w| w == "systemd")
        });
        if !has_systemd {
            bad.push(format!(
                "nsswitch.conf {db} has no 'systemd' source: guest uids would not resolve"
            ));
        }
    }
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
    // The provider joins kvm to check /dev/kvm; guests get it themselves.
    if let Some(kvm) = host.getent("group", "kvm") {
        let member =
            getent_field(&kvm, 3).is_some_and(|m| m.split(',').any(|u| u == BANLIEUE_USER));
        if !member {
            host.run(&Cmd::new("usermod", &["-aG", "kvm", BANLIEUE_USER]))?;
        }
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
    let tmpfiles = render::render(&render::TMPFILES, &values)?;
    put(
        host,
        Path::new(TMPFILES_CONF),
        tmpfiles.as_bytes(),
        MODE_PUBLIC_FILE,
        &Owner::root(),
    )?;
    if !host.is_dir(Path::new(RUN_ROOT)) {
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
    refresh_vmm_section(host, release, o)
}

/// An existing host config keeps everything but `[vmm]`, which follows
/// the release just installed, so changing the VMM never needs `--force`
/// and its EK CA rotation (ADR-0084 Decision 5). A file that does not
/// parse is the admin's to fix, and is left as it is.
fn refresh_vmm_section(host: &dyn Host, release: &Release, o: &Options) -> Result<(), Error> {
    let existing = match host.read(Path::new(HOST_CONFIG)) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if o.dry_run => {
            note(format!("{HOST_CONFIG}: {e}; [vmm] not checked"));
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let updated = match render::with_vmm(&existing, release) {
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
         restart {PROVIDER_UNIT} so it reads them",
        release.version, release.firmware_tag
    ));
    Ok(())
}

/// One userdb user and one private group per guest uid; systemd refuses
/// `User=` for a uid NSS does not know (status 217), so the range must
/// resolve before any guest starts (ADR-0063 Decision 3).
fn register_guest_uids(host: &dyn Host, s: &Settings, o: &Options) -> Result<(), Error> {
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
            "NSS does not resolve the guest records in {USERDB_DIR} (is 'systemd' in nsswitch.conf?)"
        )));
    }
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
/// when its binary and kubeconfig both exist (invariant 5).
///
/// # Errors
/// The I/O error, a template error, or `systemctl` failing.
pub fn provider(host: &dyn Host, s: &Settings, o: &Options) -> Result<(), Error> {
    log("Units");
    if let Some(src) = &o.provider_binary {
        let bytes = host.read(src)?;
        shared_dir(host, Path::new(BIN_DIR))?;
        put(
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
    for t in render::UNIT_TEMPLATES
        .iter()
        .chain([&render::PROVIDER_UNIT])
    {
        let text = render::render(t, &values)?;
        put(
            host,
            &units.join(t.name),
            text.as_bytes(),
            MODE_PUBLIC_FILE,
            &Owner::root(),
        )?;
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
        note(format!("{PROVIDER_UNIT} running"));
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
/// the firmware matches the release's digest, guest uids resolve, the provider's
/// user can open `/dev/kvm`, the host config loads and passes the
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
    if host
        .run(&Cmd::new("test", &["-r", "/dev/kvm", "-a", "-w", "/dev/kvm"]).as_user(BANLIEUE_USER))
        .is_ok()
    {
        ok(format!("{BANLIEUE_USER} can open /dev/kvm"));
    } else {
        bad.push(format!("{BANLIEUE_USER} cannot open /dev/kvm"));
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
    if let Some(only) = o.only {
        return vec![only];
    }
    let mut order = vec![
        Stage::Preflight,
        Stage::Vmm,
        Stage::Host,
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
    if let Some(only) = o.only {
        let missing = missing_prerequisites(only, host, release);
        if !missing.is_empty() && !o.dry_run {
            return Err(Error::Prerequisite {
                stage: only.name(),
                missing,
            });
        }
    }
    for stage in plan(o) {
        match stage {
            Stage::Preflight => preflight(host, s)?,
            Stage::Vmm => vmm(host, fetch, release, o).await?,
            Stage::Host => self::host(host, s, release, o)?,
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
