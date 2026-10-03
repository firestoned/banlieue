// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! What the installer may do to a host, split in two (ADR-0067 Decision 1).
//!
//! [`Probe`] only looks. `preflight` and `status` are written against it and
//! nothing else, so they cannot change a host however they are edited later.
//! [`Host`] adds the mutating operations `install` needs. Both have a real
//! implementation (`real.rs`), an in-memory fake for the unit tests
//! (`fake.rs`), and `install --dry-run` wraps the real one (`dryrun.rs`).

use std::io;
use std::path::{Path, PathBuf};

/// Who owns a file, by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    /// User name.
    pub user: String,
    /// Group name.
    pub group: String,
}

impl Owner {
    /// `user:group`.
    #[must_use]
    pub fn new(user: &str, group: &str) -> Self {
        Self {
            user: user.to_string(),
            group: group.to_string(),
        }
    }

    /// `root:root`.
    #[must_use]
    pub fn root() -> Self {
        Self::new("root", "root")
    }
}

/// What is at a path, without following a symlink there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symlink, and where it points.
    Symlink(PathBuf),
    /// Anything else (device, socket, fifo).
    Other,
}

/// A path's kind, permission bits and owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stat {
    /// What it is.
    pub kind: Kind,
    /// Permission bits, `0o7777` masked.
    pub mode: u32,
    /// Owner, by name (the id, when it has no name).
    pub owner: Owner,
}

/// A command to run: program, arguments, extra environment, and the user
/// to run it as (through `runuser`) when not root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cmd {
    /// The program.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
    /// Run as this user instead of root.
    pub user: Option<String>,
}

impl Cmd {
    /// `program args…` as root.
    #[must_use]
    pub fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(ToString::to_string).collect(),
            ..Self::default()
        }
    }

    /// The same command, run as `user`.
    #[must_use]
    pub fn as_user(mut self, user: &str) -> Self {
        self.user = Some(user.to_string());
        self
    }

    /// The same command, with `key=value` in its environment.
    #[must_use]
    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    /// For logs and errors.
    #[must_use]
    pub fn display(&self) -> String {
        let mut s = String::new();
        if let Some(u) = &self.user {
            s.push_str(&format!("(as {u}) "));
        }
        s.push_str(&self.program);
        for a in &self.args {
            s.push(' ');
            s.push_str(a);
        }
        s
    }
}

/// Looking at a host. Nothing here changes it.
pub trait Probe {
    /// What is at `path`, without following a final symlink; `None` if
    /// nothing is.
    fn stat(&self, path: &Path) -> Option<Stat>;
    /// A file's bytes.
    ///
    /// # Errors
    /// The I/O error.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// A directory's entry names, sorted; empty if it does not exist.
    fn list(&self, path: &Path) -> Vec<String>;
    /// The full path of a command on root's `PATH`.
    fn which(&self, command: &str) -> Option<PathBuf>;
    /// One `getent <db> <key>` line.
    fn getent(&self, db: &str, key: &str) -> Option<String>;
    /// Every line of `getent <db>`.
    fn getent_all(&self, db: &str) -> Vec<String>;
    /// Bytes available to an unprivileged writer on `path`'s filesystem.
    fn free_bytes(&self, path: &Path) -> Option<u64>;
    /// The virtualization this host runs under, if any (`kvm`, `vmware`…).
    fn virtualization(&self) -> Option<String>;
    /// The CPU architecture (`x86_64`).
    fn arch(&self) -> String;
    /// This host's short name.
    fn hostname(&self) -> String;
    /// Whether systemd is the running init.
    fn systemd_running(&self) -> bool;
    /// Standard output of a command that only reports (`--version`,
    /// `systemctl is-active`); `None` if it cannot run or
    /// fails.
    fn query(&self, cmd: &Cmd) -> Option<String>;
    /// Whether the process runs as root.
    fn is_root(&self) -> bool;

    /// Whether anything is at `path`.
    fn exists(&self, path: &Path) -> bool {
        self.stat(path).is_some()
    }

    /// Whether a directory is at `path` (not a symlink to one).
    fn is_dir(&self, path: &Path) -> bool {
        self.stat(path).is_some_and(|s| s.kind == Kind::Dir)
    }
}

/// Changing a host: everything `install` does goes through here.
///
/// Every operation acts on what is at the path without following a
/// symlink there: several directories the installer touches as root belong
/// to the provider's user, who could otherwise plant a link and turn a
/// re-run of the installer against another file.
pub trait Host: Probe {
    /// Create `path` (missing ancestors `0755 root:root`) and set its mode
    /// and owner. An existing directory is re-moded and re-owned; a symlink
    /// or file there is an error.
    ///
    /// # Errors
    /// The I/O error.
    fn mkdir(&self, path: &Path, mode: u32, owner: &Owner) -> io::Result<()>;
    /// Write `data` to `path` through a temporary name and a rename, with
    /// `mode` and `owner` set before the rename. The parent must exist.
    ///
    /// # Errors
    /// The I/O error.
    fn write(&self, path: &Path, data: &[u8], mode: u32, owner: &Owner) -> io::Result<()>;
    /// Point the symlink `link` at `target`, replacing what is there by a
    /// rename.
    ///
    /// # Errors
    /// The I/O error.
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()>;
    /// Set a file's or directory's mode, refusing a symlink.
    ///
    /// # Errors
    /// The I/O error.
    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()>;
    /// Remove a file, symlink or directory tree. Nothing there is success.
    ///
    /// # Errors
    /// The I/O error.
    fn remove(&self, path: &Path) -> io::Result<()>;
    /// Run a command that may change the host; its standard output.
    ///
    /// # Errors
    /// The command could not start or exited non-zero (with its stderr).
    fn run(&self, cmd: &Cmd) -> io::Result<String>;
}
