// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The real host: the filesystem, NSS through `getent`, and commands.
//!
//! Every ownership and mode change acts on a handle opened `O_NOFOLLOW`,
//! and every write is a temporary file created `O_EXCL` beside its target
//! and renamed over it, so neither a crash nor a symlink the provider's
//! user planted in one of its own directories can make the installer, which
//! runs as root, change a file it did not mean to.

use crate::ops::{CgroupMode, Cmd, Host, Kind, Owner, Probe, Stat};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use rustix::fs::{Gid, Mode, OFlags, Uid};

/// Permission bits, including setuid/setgid/sticky.
const MODE_BITS: u32 = 0o7777;
/// Ancestors the installer has to create.
const ANCESTOR_MODE: u32 = 0o755;
/// A temporary file, before its real mode is set.
const TMP_MODE: u32 = 0o600;
/// Suffix for temporary names beside their target.
const TMP_SUFFIX: &str = ".banlieue-tmp";
/// Root's commands, which a `sudo` or cloud-init environment may not have on
/// `PATH`.
const SBIN_DIRS: [&str; 4] = ["/usr/local/sbin", "/usr/sbin", "/sbin", "/usr/bin"];
/// Any execute bit.
const ANY_EXECUTE: u32 = 0o111;
/// Where systemd, as the running init, leaves this directory.
const SYSTEMD_RUNNING_MARKER: &str = "/run/systemd/system";
/// Present only on the unified (cgroup v2) hierarchy.
const UNIFIED_CGROUP_MARKER: &str = "/sys/fs/cgroup/cgroup.controllers";

/// The host this process runs on.
#[derive(Debug, Default)]
pub struct RealHost {
    names: Mutex<HashMap<(char, u32), String>>,
}

impl RealHost {
    /// A handle on this host.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The name for a uid (`'u'`) or gid (`'g'`), or the number.
    fn name_of(&self, kind: char, id: u32) -> String {
        let mut cache = self.names.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .entry((kind, id))
            .or_insert_with(|| {
                let db = if kind == 'u' { "passwd" } else { "group" };
                self.getent(db, &id.to_string())
                    .and_then(|l| l.split(':').next().map(str::to_string))
                    .unwrap_or_else(|| id.to_string())
            })
            .clone()
    }

    /// Numeric ids for an owner, from NSS.
    fn ids(&self, owner: &Owner) -> io::Result<(Uid, Gid)> {
        let id = |db: &str, name: &str| -> io::Result<u32> {
            self.getent(db, name)
                .and_then(|l| l.split(':').nth(2).and_then(|f| f.parse().ok()))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, format!("no {db} entry for {name}"))
                })
        };
        Ok((
            Uid::from_raw(id("passwd", &owner.user)?),
            Gid::from_raw(id("group", &owner.group)?),
        ))
    }

    /// Open what is at `path` for a metadata change, refusing a symlink.
    fn open_nofollow(path: &Path, dir: bool) -> io::Result<OwnedFd> {
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        if dir {
            flags |= OFlags::DIRECTORY;
        }
        rustix::fs::open(path, flags, Mode::empty()).map_err(|e| with_path(e.into(), path))
    }

    fn own_and_mode(&self, fd: &OwnedFd, path: &Path, mode: u32, owner: &Owner) -> io::Result<()> {
        let (uid, gid) = self.ids(owner)?;
        rustix::fs::fchown(fd, Some(uid), Some(gid)).map_err(|e| with_path(e.into(), path))?;
        rustix::fs::fchmod(fd, Mode::from_raw_mode(mode)).map_err(|e| with_path(e.into(), path))
    }
}

fn with_path(e: io::Error, path: &Path) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

/// `<dir>/.<name>.banlieue-tmp`, beside `path`.
fn tmp_beside(path: &Path) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other(format!("{} has no parent", path.display())))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other(format!("{} has no file name", path.display())))?;
    let mut tmp = OsString::from(".");
    tmp.push(name);
    tmp.push(TMP_SUFFIX);
    Ok(parent.join(tmp))
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(with_path(e, path)),
        _ => Ok(()),
    }
}

impl Probe for RealHost {
    fn stat(&self, path: &Path) -> Option<Stat> {
        let m = fs::symlink_metadata(path).ok()?;
        let ft = m.file_type();
        let kind = if ft.is_symlink() {
            Kind::Symlink(fs::read_link(path).unwrap_or_default())
        } else if ft.is_dir() {
            Kind::Dir
        } else if ft.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        Some(Stat {
            kind,
            mode: m.permissions().mode() & MODE_BITS,
            owner: Owner {
                user: self.name_of('u', m.uid()),
                group: self.name_of('g', m.gid()),
            },
        })
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path).map_err(|e| with_path(e, path))
    }

    fn list(&self, path: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(path)
            .map(|r| {
                r.filter_map(|e| e.ok()?.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn which(&self, command: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path)
            .chain(SBIN_DIRS.iter().map(PathBuf::from))
            .map(|d| d.join(command))
            .find(|p| {
                fs::metadata(p)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & ANY_EXECUTE != 0)
            })
    }

    fn getent(&self, db: &str, key: &str) -> Option<String> {
        self.query(&Cmd::new("getent", &[db, key]))
            .and_then(|out| out.lines().next().map(str::to_string))
    }

    fn getent_all(&self, db: &str) -> Vec<String> {
        self.query(&Cmd::new("getent", &[db]))
            .map(|out| out.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn free_bytes(&self, path: &Path) -> Option<u64> {
        let st = rustix::fs::statvfs(path).ok()?;
        Some(st.f_bavail.saturating_mul(st.f_frsize))
    }

    fn virtualization(&self) -> Option<String> {
        // Exits non-zero, printing `none`, on bare metal.
        self.query(&Cmd::new("systemd-detect-virt", &["--vm"]))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s != "none")
    }

    fn arch(&self) -> String {
        std::env::consts::ARCH.to_string()
    }

    fn hostname(&self) -> String {
        let uname = rustix::system::uname();
        let full = uname.nodename().to_string_lossy().into_owned();
        full.split('.').next().unwrap_or_default().to_string()
    }

    fn systemd_running(&self) -> bool {
        Path::new(SYSTEMD_RUNNING_MARKER).is_dir()
    }

    fn query(&self, cmd: &Cmd) -> Option<String> {
        let out = command(cmd).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn is_root(&self) -> bool {
        rustix::process::geteuid().is_root()
    }

    fn cgroup_mode(&self) -> CgroupMode {
        if Path::new(UNIFIED_CGROUP_MARKER).exists() {
            return CgroupMode::Unified;
        }
        CgroupMode::Legacy
    }

    fn systemd_version(&self) -> Option<u32> {
        self.query(&Cmd::new("systemctl", &["--version"]))
            .and_then(|out| crate::hardening::parse_systemd_version(&out))
    }

    fn attestation_ca(&self, dir: &Path) -> Result<crate::attestation::CaInfo, String> {
        crate::attestation::inspect(dir)
    }

    fn pending_attestation_ca(&self, dir: &Path) -> Result<String, String> {
        banlieue_attestation_ca::PendingCa::load(dir)
            .map(|p| p.common_name().to_string())
            .map_err(|e| e.to_string())
    }
}

/// A `Command` for `cmd`, through `runuser` when it names a user.
fn command(cmd: &Cmd) -> Command {
    let mut c = match &cmd.user {
        Some(user) => {
            let mut c = Command::new("runuser");
            c.args(["-u", user, "--", &cmd.program]);
            c
        }
        None => Command::new(&cmd.program),
    };
    c.args(&cmd.args);
    for (k, v) in &cmd.env {
        c.env(k, v);
    }
    c
}

impl Host for RealHost {
    fn mkdir(&self, path: &Path, mode: u32, owner: &Owner) -> io::Result<()> {
        let mut missing: Vec<&Path> = path
            .ancestors()
            .skip(1)
            .take_while(|a| !a.exists())
            .collect();
        missing.reverse();
        for a in missing {
            fs::create_dir(a).map_err(|e| with_path(e, a))?;
            let fd = Self::open_nofollow(a, true)?;
            self.own_and_mode(&fd, a, ANCESTOR_MODE, &Owner::root())?;
        }
        match fs::create_dir(path) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(with_path(e, path)),
            _ => {}
        }
        // O_DIRECTORY|O_NOFOLLOW refuses a symlink or file planted here.
        let fd = Self::open_nofollow(path, true)?;
        self.own_and_mode(&fd, path, mode, owner)
    }

    fn write(&self, path: &Path, data: &[u8], mode: u32, owner: &Owner) -> io::Result<()> {
        let tmp = tmp_beside(path)?;
        remove_file_if_present(&tmp)?;
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(TMP_MODE)
            .custom_flags(libc_flags())
            .open(&tmp)
            .map_err(|e| with_path(e, &tmp))?;
        f.write_all(data).map_err(|e| with_path(e, &tmp))?;
        f.sync_all().map_err(|e| with_path(e, &tmp))?;
        let fd: OwnedFd = f.into();
        self.own_and_mode(&fd, &tmp, mode, owner)?;
        drop(fd);
        // rename replaces whatever is at `path`, a symlink included, and
        // never follows it.
        fs::rename(&tmp, path).map_err(|e| with_path(e, path))
    }

    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        let tmp = tmp_beside(link)?;
        remove_file_if_present(&tmp)?;
        std::os::unix::fs::symlink(target, &tmp).map_err(|e| with_path(e, &tmp))?;
        fs::rename(&tmp, link).map_err(|e| with_path(e, link))
    }

    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()> {
        let dir = self.is_dir(path);
        let fd = Self::open_nofollow(path, dir)?;
        rustix::fs::fchmod(&fd, Mode::from_raw_mode(mode)).map_err(|e| with_path(e.into(), path))
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        match fs::symlink_metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(with_path(e, path)),
            // remove_dir_all does not follow symlinks inside the tree.
            Ok(m) if m.is_dir() => fs::remove_dir_all(path).map_err(|e| with_path(e, path)),
            Ok(_) => fs::remove_file(path).map_err(|e| with_path(e, path)),
        }
    }

    fn create_attestation_ca(&self, dir: &Path, name: &str, owner: &Owner) -> io::Result<()> {
        let (uid, gid) = self.ids(owner)?;
        banlieue_attestation_ca::AttestationCa::create(
            dir,
            name,
            Some((uid.as_raw(), gid.as_raw())),
        )
        .map(drop)
        .map_err(|e| with_path(io::Error::other(e.to_string()), dir))
    }

    fn create_pending_attestation_ca(
        &self,
        dir: &Path,
        name: &str,
        owner: &Owner,
    ) -> io::Result<()> {
        let (uid, gid) = self.ids(owner)?;
        banlieue_attestation_ca::PendingCa::create(dir, name, Some((uid.as_raw(), gid.as_raw())))
            .map(drop)
            .map_err(|e| with_path(io::Error::other(e.to_string()), dir))
    }

    fn import_attestation_ca(
        &self,
        pending_dir: &Path,
        ca_dir: &Path,
        signed: &[u8],
        replace: bool,
        owner: &Owner,
    ) -> io::Result<()> {
        let (uid, gid) = self.ids(owner)?;
        let pending = banlieue_attestation_ca::PendingCa::load(pending_dir)
            .map_err(|e| with_path(io::Error::other(e.to_string()), pending_dir))?;
        pending
            .import(
                ca_dir,
                &String::from_utf8_lossy(signed),
                std::time::SystemTime::now(),
                replace,
                Some((uid.as_raw(), gid.as_raw())),
            )
            .map(drop)
            .map_err(|e| with_path(io::Error::other(e.to_string()), ca_dir))
    }

    fn remove_attestation_ca(&self, dir: &Path) -> io::Result<()> {
        banlieue_attestation_ca::AttestationCa::remove_files(dir)
            .map_err(|e| with_path(io::Error::other(e.to_string()), dir))
    }

    fn run(&self, cmd: &Cmd) -> io::Result<String> {
        let out = command(cmd)
            .output()
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", cmd.display())))?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "{} failed ({}): {}",
                cmd.display(),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// `O_NOFOLLOW | O_CLOEXEC` for `OpenOptions::custom_flags`.
fn libc_flags() -> i32 {
    (OFlags::NOFOLLOW | OFlags::CLOEXEC).bits() as i32
}
