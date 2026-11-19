// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `install --dry-run`: look at the real host, change nothing, and print
//! each change `install` would make. The stages skip what is already in
//! place, so what is printed is the difference.

use crate::ops::{CgroupMode, Cmd, Host, Owner, Probe, Stat};
use std::io;
use std::path::{Path, PathBuf};

/// A host that reports its changes instead of making them.
pub struct DryRun<'a> {
    inner: &'a dyn Probe,
}

impl<'a> DryRun<'a> {
    /// Wrap the host to look at.
    #[must_use]
    pub fn new(inner: &'a dyn Probe) -> Self {
        Self { inner }
    }
}

fn say(what: String) {
    println!("would {what}");
}

impl Probe for DryRun<'_> {
    fn stat(&self, path: &Path) -> Option<Stat> {
        self.inner.stat(path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.inner.read(path)
    }
    fn list(&self, path: &Path) -> Vec<String> {
        self.inner.list(path)
    }
    fn which(&self, command: &str) -> Option<PathBuf> {
        self.inner.which(command)
    }
    fn getent(&self, db: &str, key: &str) -> Option<String> {
        self.inner.getent(db, key)
    }
    fn getent_all(&self, db: &str) -> Vec<String> {
        self.inner.getent_all(db)
    }
    fn free_bytes(&self, path: &Path) -> Option<u64> {
        self.inner.free_bytes(path)
    }
    fn virtualization(&self) -> Option<String> {
        self.inner.virtualization()
    }
    fn arch(&self) -> String {
        self.inner.arch()
    }
    fn hostname(&self) -> String {
        self.inner.hostname()
    }
    fn systemd_running(&self) -> bool {
        self.inner.systemd_running()
    }
    fn query(&self, cmd: &Cmd) -> Option<String> {
        self.inner.query(cmd)
    }
    fn is_root(&self) -> bool {
        self.inner.is_root()
    }
    fn cgroup_mode(&self) -> CgroupMode {
        self.inner.cgroup_mode()
    }
    fn systemd_version(&self) -> Option<u32> {
        self.inner.systemd_version()
    }
}

impl Host for DryRun<'_> {
    fn mkdir(&self, path: &Path, mode: u32, owner: &Owner) -> io::Result<()> {
        say(format!(
            "create {} ({mode:04o} {}:{})",
            path.display(),
            owner.user,
            owner.group
        ));
        Ok(())
    }
    fn write(&self, path: &Path, data: &[u8], mode: u32, owner: &Owner) -> io::Result<()> {
        say(format!(
            "write {} ({} bytes, {mode:04o} {}:{})",
            path.display(),
            data.len(),
            owner.user,
            owner.group
        ));
        Ok(())
    }
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        say(format!("link {} -> {}", link.display(), target.display()));
        Ok(())
    }
    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()> {
        say(format!("chmod {mode:04o} {}", path.display()));
        Ok(())
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        say(format!("remove {}", path.display()));
        Ok(())
    }
    fn run(&self, cmd: &Cmd) -> io::Result<String> {
        say(format!("run {}", cmd.display()));
        Ok(String::new())
    }
}
