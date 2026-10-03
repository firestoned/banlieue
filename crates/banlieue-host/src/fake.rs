// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! An in-memory host for the unit tests.
//!
//! As strict as the real one where it matters (`rules/testing.md`: a fake
//! more permissive than the real thing hides bugs): writing into a missing
//! directory fails, `mkdir` refuses a symlink, changing the mode of a
//! symlink fails, and an owner must be a user and group that exist. The
//! commands the stages run are simulated: `useradd` creates the user,
//! `swtpm_setup` creates the CA
//! and writes an EK certificate, `systemctl enable --now` starts a unit.

use crate::ops::{Cmd, Host, Kind, Owner, Probe, Stat};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// An EK certificate `swtpm_setup` minted for
/// `banlieue-selftest:00000000-0000-4000-8000-000000000000` (DER).
pub const SELFTEST_EK_DER: &[u8] = include_bytes!("fixtures/ek-rsa2048-selftest.der");

/// The provider user's ids when the fake `useradd` creates it.
const FAKE_SYSTEM_ID: u32 = 998;
/// The kvm group's id.
const FAKE_KVM_GID: u32 = 993;

/// One filesystem entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// What it is.
    pub kind: Kind,
    /// A file's bytes.
    pub data: Vec<u8>,
    /// Permission bits.
    pub mode: u32,
    /// Owner.
    pub owner: Owner,
}

/// Everything the fake remembers; `Clone` + `Eq` so a test can snapshot it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// The filesystem.
    pub fs: BTreeMap<PathBuf, Node>,
    /// Users: name → (uid, gid).
    pub users: BTreeMap<String, (u32, u32)>,
    /// Groups: name → (gid, members).
    pub groups: BTreeMap<String, (u32, BTreeSet<String>)>,
    /// Units and their state.
    pub units: BTreeMap<String, String>,
}

/// Knobs that are not state.
#[derive(Clone, Debug)]
pub struct Env {
    /// `systemd-detect-virt --vm`.
    pub virt: Option<String>,
    /// CPU architecture.
    pub arch: String,
    /// Short host name.
    pub hostname: String,
    /// systemd is PID 1.
    pub systemd_running: bool,
    /// The process is root.
    pub root: bool,
    /// NSS consults `/etc/userdb` (the `systemd` module).
    pub nss_systemd: bool,
    /// The provider user can open `/dev/kvm`.
    pub kvm_access: bool,
    /// Free bytes by path prefix.
    pub free: BTreeMap<PathBuf, u64>,
}

impl Default for Env {
    fn default() -> Self {
        Self {
            virt: None,
            arch: "x86_64".into(),
            hostname: "bar".into(),
            systemd_running: true,
            root: true,
            nss_systemd: true,
            kvm_access: true,
            free: BTreeMap::new(),
        }
    }
}

/// The fake host.
#[derive(Debug, Default)]
pub struct FakeHost {
    /// Its state.
    pub state: Mutex<State>,
    /// Its environment.
    pub env: Mutex<Env>,
    /// Every mutating command, in order.
    pub ran: Mutex<Vec<Cmd>>,
}

fn not_found(p: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{}: not found", p.display()),
    )
}

impl FakeHost {
    /// A Debian-like host with root, a kvm group, `/dev/kvm`, a bridge
    /// `virbr0`, systemd in `nsswitch.conf`, and nothing banlieue installed.
    #[must_use]
    pub fn debian() -> Self {
        let h = Self::default();
        {
            let mut s = h.state.lock().unwrap();
            s.users.insert("root".into(), (0, 0));
            s.groups.insert("root".into(), (0, BTreeSet::new()));
            s.groups
                .insert("kvm".into(), (FAKE_KVM_GID, BTreeSet::new()));
        }
        for d in [
            "/",
            "/etc",
            "/var",
            "/var/lib",
            "/run",
            "/usr",
            "/usr/local",
            "/srv",
            "/opt",
            "/dev",
        ] {
            h.put_dir(d);
        }
        h.put_dir("/usr/local/bin");
        h.put_file("/dev/kvm", b"", Kind::Other);
        h.put_dir("/sys/class/net/virbr0/bridge");
        h.put_file(
            "/etc/nsswitch.conf",
            b"passwd:         files systemd\ngroup:          files systemd\n",
            Kind::File,
        );
        h.env.lock().unwrap().free.insert("/srv".into(), 500 << 30);
        h
    }

    /// Put a root-owned directory (and its ancestors) at `path`.
    pub fn put_dir(&self, path: &str) {
        let mut s = self.state.lock().unwrap();
        for a in Path::new(path).ancestors() {
            s.fs.entry(a.to_path_buf()).or_insert_with(|| Node {
                kind: Kind::Dir,
                data: vec![],
                mode: 0o755,
                owner: Owner::root(),
            });
        }
    }

    /// Put a root-owned entry at `path`.
    pub fn put_file(&self, path: &str, data: &[u8], kind: Kind) {
        if let Some(parent) = Path::new(path).parent() {
            self.put_dir(&parent.to_string_lossy());
        }
        self.state.lock().unwrap().fs.insert(
            PathBuf::from(path),
            Node {
                kind,
                data: data.to_vec(),
                mode: 0o644,
                owner: Owner::root(),
            },
        );
    }

    /// What the host's own package manager would have installed: the
    /// commands of `packages`, on `PATH`. banlieue installs none of them
    /// (ADR-0084 Decision 1).
    pub fn install_packages(&self, packages: &[&str]) {
        for p in packages {
            for cmd in commands_of(p) {
                self.put_file(&format!("/usr/bin/{cmd}"), b"", Kind::File);
            }
        }
    }

    /// A copy of the state, for before/after comparison.
    #[must_use]
    pub fn snapshot(&self) -> State {
        self.state.lock().unwrap().clone()
    }

    /// The mutating commands run so far, as strings.
    #[must_use]
    pub fn commands(&self) -> Vec<String> {
        self.ran.lock().unwrap().iter().map(Cmd::display).collect()
    }

    fn check_owner(s: &State, owner: &Owner) -> io::Result<()> {
        if !s.users.contains_key(&owner.user) && !Self::userdb_has(s, &owner.user) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no passwd entry for {}", owner.user),
            ));
        }
        if !s.groups.contains_key(&owner.group) && !Self::userdb_has(s, &owner.group) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no group entry for {}", owner.group),
            ));
        }
        Ok(())
    }

    fn userdb_has(s: &State, name: &str) -> bool {
        s.fs.contains_key(&Path::new("/etc/userdb").join(format!("{name}.user")))
    }

    fn parent_is_dir(s: &State, path: &Path) -> io::Result<()> {
        let parent = path.parent().ok_or_else(|| not_found(path))?;
        match s.fs.get(parent) {
            Some(n) if n.kind == Kind::Dir => Ok(()),
            _ => Err(not_found(parent)),
        }
    }

    /// `getent passwd`/`group` lines from users, groups and (with the
    /// systemd NSS module) userdb records.
    fn nss_lines(&self, db: &str) -> Vec<String> {
        let s = self.state.lock().unwrap();
        let mut lines = Vec::new();
        if db == "passwd" {
            for (name, (uid, gid)) in &s.users {
                lines.push(format!("{name}:x:{uid}:{gid}::/:/usr/sbin/nologin"));
            }
        } else {
            for (name, (gid, members)) in &s.groups {
                let m: Vec<&str> = members.iter().map(String::as_str).collect();
                lines.push(format!("{name}:x:{gid}:{}", m.join(",")));
            }
        }
        if self.env.lock().unwrap().nss_systemd {
            let ext = if db == "passwd" { ".user" } else { ".group" };
            for (p, n) in &s.fs {
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                if !p.starts_with("/etc/userdb") || n.kind != Kind::File || !name.ends_with(ext) {
                    continue;
                }
                let v: serde_json::Value = serde_json::from_slice(&n.data).unwrap_or_default();
                if db == "passwd" {
                    lines.push(format!(
                        "{}:x:{}:{}::/:/usr/sbin/nologin",
                        v["userName"].as_str().unwrap_or_default(),
                        v["uid"],
                        v["gid"]
                    ));
                } else {
                    lines.push(format!(
                        "{}:x:{}:",
                        v["groupName"].as_str().unwrap_or_default(),
                        v["gid"]
                    ));
                }
            }
        }
        lines
    }

    fn simulate(&self, cmd: &Cmd) -> io::Result<String> {
        let args: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
        match (cmd.program.as_str(), args.as_slice()) {
            ("useradd", [.., name]) => {
                self.state
                    .lock()
                    .unwrap()
                    .users
                    .insert((*name).to_string(), (FAKE_SYSTEM_ID, FAKE_SYSTEM_ID));
                self.state
                    .lock()
                    .unwrap()
                    .groups
                    .insert((*name).to_string(), (FAKE_SYSTEM_ID, BTreeSet::new()));
            }
            ("usermod", ["-aG", group, user]) => {
                let mut s = self.state.lock().unwrap();
                let g = s
                    .groups
                    .get_mut(*group)
                    .ok_or_else(|| io::Error::other(format!("usermod: no group {group}")))?;
                g.1.insert((*user).to_string());
            }
            ("systemctl", ["daemon-reload"]) => {}
            ("systemctl", ["enable", "--now", unit]) => {
                self.state
                    .lock()
                    .unwrap()
                    .units
                    .insert((*unit).to_string(), "active".into());
            }
            ("systemd-tmpfiles", ["--create", _]) => {
                let mut s = self.state.lock().unwrap();
                for (dir, mode, user) in [
                    ("/run/banlieue", 0o755, "root"),
                    ("/run/banlieue/ch", 0o711, "banlieue"),
                ] {
                    s.fs.entry(dir.into()).or_insert(Node {
                        kind: Kind::Dir,
                        data: vec![],
                        mode,
                        owner: Owner::new(user, user),
                    });
                }
            }
            ("swtpm_setup", _) => self.swtpm_setup(&args)?,
            ("test", _) => {
                if !self.env.lock().unwrap().kvm_access {
                    return Err(io::Error::other("test failed"));
                }
            }
            (p, _) => return Err(io::Error::other(format!("fake: unexpected command {p}"))),
        }
        Ok(String::new())
    }

    /// `swtpm_setup`, as the provider's user: first use creates the CA it
    /// is configured with; every manufacture advances the CA's serial, as
    /// the real one does; `--write-ek-cert-files` writes the certificate.
    fn swtpm_setup(&self, args: &[&str]) -> io::Result<()> {
        let value = |flag: &str| {
            args.iter()
                .position(|a| *a == flag)
                .and_then(|i| args.get(i + 1))
                .map(|s| PathBuf::from(*s))
        };
        let ca = PathBuf::from(crate::paths::EK_CA_DIR);
        let owner = Owner::new("banlieue", "banlieue");
        let mut s = self.state.lock().unwrap();
        if !s.fs.contains_key(&ca.join("issuercert.pem")) {
            for (f, mode) in [("signkey.pem", 0o640), ("issuercert.pem", 0o640)] {
                s.fs.insert(
                    ca.join(f),
                    Node {
                        kind: Kind::File,
                        data: f.as_bytes().to_vec(),
                        mode,
                        owner: owner.clone(),
                    },
                );
            }
        }
        let serial = ca.join("certserial");
        let next =
            s.fs.get(&serial)
                .and_then(|n| String::from_utf8_lossy(&n.data).trim().parse::<u64>().ok())
                .unwrap_or(0)
                + 1;
        s.fs.insert(
            serial,
            Node {
                kind: Kind::File,
                data: next.to_string().into_bytes(),
                mode: 0o640,
                owner: owner.clone(),
            },
        );
        if let Some(dir) = value("--write-ek-cert-files") {
            s.fs.insert(
                dir.join("ek-rsa2048.crt"),
                Node {
                    kind: Kind::File,
                    data: SELFTEST_EK_DER.to_vec(),
                    mode: 0o640,
                    owner,
                },
            );
        }
        Ok(())
    }
}

/// The commands a package puts on `PATH`.
fn commands_of(package: &str) -> &'static [&'static str] {
    match package {
        "swtpm" => &["swtpm"],
        "swtpm-tools" => &["swtpm_setup", "swtpm_localca"],
        "systemd" => &["systemctl", "systemd-tmpfiles", "systemd-detect-virt"],
        _ => &[],
    }
}

impl Probe for FakeHost {
    fn stat(&self, path: &Path) -> Option<Stat> {
        let s = self.state.lock().unwrap();
        s.fs.get(path).map(|n| Stat {
            kind: n.kind.clone(),
            mode: n.mode,
            owner: n.owner.clone(),
        })
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let s = self.state.lock().unwrap();
        match s.fs.get(path) {
            Some(n) if n.kind == Kind::File || n.kind == Kind::Other => Ok(n.data.clone()),
            _ => Err(not_found(path)),
        }
    }

    fn list(&self, path: &Path) -> Vec<String> {
        let s = self.state.lock().unwrap();
        s.fs.keys()
            .filter(|p| p.parent() == Some(path))
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect()
    }

    fn which(&self, command: &str) -> Option<PathBuf> {
        ["/usr/bin", "/usr/sbin", "/usr/local/bin"]
            .iter()
            .map(|d| Path::new(d).join(command))
            .find(|p| self.exists(p))
    }

    fn getent(&self, db: &str, key: &str) -> Option<String> {
        self.nss_lines(db).into_iter().find(|l| {
            let f: Vec<&str> = l.split(':').collect();
            f.first() == Some(&key) || f.get(2) == Some(&key)
        })
    }

    fn getent_all(&self, db: &str) -> Vec<String> {
        self.nss_lines(db)
    }

    fn free_bytes(&self, path: &Path) -> Option<u64> {
        let env = self.env.lock().unwrap();
        path.ancestors().find_map(|a| env.free.get(a).copied())
    }

    fn virtualization(&self) -> Option<String> {
        self.env.lock().unwrap().virt.clone()
    }

    fn arch(&self) -> String {
        self.env.lock().unwrap().arch.clone()
    }

    fn hostname(&self) -> String {
        self.env.lock().unwrap().hostname.clone()
    }

    fn systemd_running(&self) -> bool {
        self.env.lock().unwrap().systemd_running
    }

    fn query(&self, cmd: &Cmd) -> Option<String> {
        let args: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
        match (cmd.program.as_str(), args.as_slice()) {
            ("systemctl", ["is-active", unit]) => Some(
                self.state
                    .lock()
                    .unwrap()
                    .units
                    .get(*unit)
                    .cloned()
                    .unwrap_or_else(|| "inactive".into()),
            ),
            ("systemctl", ["list-units", ..]) => Some(String::new()),
            (p, ["--version"]) if self.exists(Path::new(p)) => Some(format!("{p} v53.0\n")),
            _ => None,
        }
    }

    fn is_root(&self) -> bool {
        self.env.lock().unwrap().root
    }
}

impl Host for FakeHost {
    fn mkdir(&self, path: &Path, mode: u32, owner: &Owner) -> io::Result<()> {
        let mut s = self.state.lock().unwrap();
        Self::check_owner(&s, owner)?;
        let mut missing: Vec<PathBuf> = path
            .ancestors()
            .skip(1)
            .take_while(|a| !s.fs.contains_key(*a))
            .map(Path::to_path_buf)
            .collect();
        missing.reverse();
        for a in missing {
            s.fs.insert(
                a,
                Node {
                    kind: Kind::Dir,
                    data: vec![],
                    mode: 0o755,
                    owner: Owner::root(),
                },
            );
        }
        match s.fs.get_mut(path) {
            Some(n) if n.kind != Kind::Dir => Err(io::Error::other(format!(
                "{}: not a directory ({:?})",
                path.display(),
                n.kind
            ))),
            Some(n) => {
                n.mode = mode;
                n.owner = owner.clone();
                Ok(())
            }
            None => {
                s.fs.insert(
                    path.to_path_buf(),
                    Node {
                        kind: Kind::Dir,
                        data: vec![],
                        mode,
                        owner: owner.clone(),
                    },
                );
                Ok(())
            }
        }
    }

    fn write(&self, path: &Path, data: &[u8], mode: u32, owner: &Owner) -> io::Result<()> {
        let mut s = self.state.lock().unwrap();
        Self::check_owner(&s, owner)?;
        Self::parent_is_dir(&s, path)?;
        if s.fs.get(path).is_some_and(|n| n.kind == Kind::Dir) {
            return Err(io::Error::other(format!(
                "{}: is a directory",
                path.display()
            )));
        }
        s.fs.insert(
            path.to_path_buf(),
            Node {
                kind: Kind::File,
                data: data.to_vec(),
                mode,
                owner: owner.clone(),
            },
        );
        Ok(())
    }

    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        let mut s = self.state.lock().unwrap();
        Self::parent_is_dir(&s, link)?;
        s.fs.insert(
            link.to_path_buf(),
            Node {
                kind: Kind::Symlink(target.to_path_buf()),
                data: vec![],
                mode: 0o777,
                owner: Owner::root(),
            },
        );
        Ok(())
    }

    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()> {
        let mut s = self.state.lock().unwrap();
        match s.fs.get_mut(path) {
            None => Err(not_found(path)),
            Some(n) if matches!(n.kind, Kind::Symlink(_)) => Err(io::Error::other(format!(
                "{}: refusing a symlink",
                path.display()
            ))),
            Some(n) => {
                n.mode = mode;
                Ok(())
            }
        }
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        let mut s = self.state.lock().unwrap();
        s.fs.retain(|p, _| !p.starts_with(path));
        Ok(())
    }

    fn run(&self, cmd: &Cmd) -> io::Result<String> {
        self.ran.lock().unwrap().push(cmd.clone());
        self.simulate(cmd)
    }
}
