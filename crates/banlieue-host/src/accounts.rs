// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! How guest uids become accounts NSS resolves (ADR-0139 Decision 1).
//!
//! A unit with `User=<uid>` fails with status 217 when NSS does not know the
//! uid, so every uid in the guest range needs a user and a private group.
//! `userdb` (ADR-0063 Decision 3) registers them as systemd userdb records,
//! which needs systemd 245 and the `systemd` NSS source. `files` creates
//! them in the host's own databases with its own `groupadd` and `useradd`,
//! never by editing `passwd` or `group`. The installer never edits
//! `nsswitch.conf` in either mode.

use crate::error::Error;
use crate::ops::{Cmd, Host, Probe};
use crate::paths::GUEST_NAME_PREFIX;
use crate::settings::Settings;
use clap::ValueEnum;
use std::path::Path;

/// The first systemd with userdb (`/etc/userdb`, `userdbctl`).
pub const MIN_SYSTEMD_USERDB: u32 = 245;
/// The shell of a guest account: none.
const NOLOGIN_SHELL: &str = "/usr/sbin/nologin";
/// A guest account's home directory: none that exists.
const NO_HOME: &str = "/";
/// The name service switch file; read, never written.
const NSSWITCH_CONF: &str = "/etc/nsswitch.conf";
/// The databases a guest account lives in.
const NSS_DATABASES: [&str; 2] = ["passwd", "group"];

/// How guest accounts are registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum GuestAccounts {
    /// systemd userdb records in `/etc/userdb` (systemd 245 or newer).
    Userdb,
    /// Accounts in the host's own passwd and group databases.
    Files,
}

impl GuestAccounts {
    /// Its name, as on the command line.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Userdb => "userdb",
            Self::Files => "files",
        }
    }
}

/// Whether `db`'s line in `nsswitch` lists any of `sources`, as a whole word.
#[must_use]
pub fn nss_has_source(nsswitch: &str, db: &str, sources: &[&str]) -> bool {
    nsswitch.lines().any(|l| {
        let l = l.trim_start();
        l.strip_prefix(db)
            .and_then(|rest| rest.strip_prefix(':'))
            .is_some_and(|rest| rest.split_whitespace().any(|w| sources.contains(&w)))
    })
}

fn nsswitch(probe: &dyn Probe) -> String {
    probe
        .read(Path::new(NSSWITCH_CONF))
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// Why this host cannot do userdb, or `None` when it can.
fn userdb_blocker(probe: &dyn Probe) -> Option<String> {
    match probe.systemd_version() {
        None => return Some("the systemd version cannot be read".into()),
        Some(v) if v < MIN_SYSTEMD_USERDB => {
            return Some(format!(
                "systemd {v} is older than {MIN_SYSTEMD_USERDB}, which introduced userdb"
            ));
        }
        Some(_) => {}
    }
    let text = nsswitch(probe);
    NSS_DATABASES
        .iter()
        .find(|db| !nss_has_source(&text, db, &["systemd"]))
        .map(|db| format!("nsswitch.conf {db} has no 'systemd' source"))
}

/// The mode to use and why: `explicit` when given (checked against the
/// host's facts), else derived from them.
///
/// # Errors
/// [`Error::Setting`] when `userdb` is asked for on a host that cannot do it.
pub fn choose(
    explicit: Option<GuestAccounts>,
    probe: &dyn Probe,
) -> Result<(GuestAccounts, String), Error> {
    let blocker = userdb_blocker(probe);
    match (explicit, blocker) {
        (Some(GuestAccounts::Userdb), Some(why)) => Err(Error::Setting {
            name: "--guest-accounts",
            why: format!("userdb: {why}; use --guest-accounts files"),
        }),
        (Some(mode), _) => Ok((mode, "chosen with --guest-accounts".into())),
        (None, Some(why)) => Ok((
            GuestAccounts::Files,
            format!("{why}, so guest accounts go in the host's own databases"),
        )),
        (None, None) => {
            let v = probe.systemd_version().unwrap_or_default();
            Ok((
                GuestAccounts::Userdb,
                format!("systemd {v} has userdb and nsswitch.conf has the 'systemd' source"),
            ))
        }
    }
}

/// What the chosen mode needs of this host that it does not have: the
/// mode-specific `nsswitch.conf` sources, and, for `files`, a foreign group
/// inside the guest gid range.
#[must_use]
pub fn problems(probe: &dyn Probe, s: &Settings) -> Vec<String> {
    let text = nsswitch(probe);
    let mut bad = Vec::new();
    let (sources, why): (&[&str], &str) = match s.guest_accounts {
        GuestAccounts::Userdb => (
            &["systemd"],
            "no 'systemd' source: guest uids would not resolve",
        ),
        GuestAccounts::Files => (
            &["files", "compat"],
            "has neither 'files' nor 'compat': local accounts would not resolve",
        ),
    };
    for db in NSS_DATABASES {
        if !nss_has_source(&text, db, sources) {
            bad.push(format!("nsswitch.conf {db} {why}"));
        }
    }
    if s.guest_accounts == GuestAccounts::Files {
        for line in probe.getent_all("group") {
            let mut f = line.split(':');
            let (Some(name), Some(gid)) = (f.next(), f.nth(1).and_then(|g| g.parse::<u32>().ok()))
            else {
                continue;
            };
            if (s.uid_base..=s.uid_end()).contains(&gid) && !name.starts_with(GUEST_NAME_PREFIX) {
                bad.push(format!(
                    "group {name} uses gid {gid}, inside the guest range {}-{}",
                    s.uid_base,
                    s.uid_end()
                ));
            }
        }
    }
    bad
}

/// The field of a `getent` line.
fn field(line: &str, index: usize) -> Option<&str> {
    line.split(':').nth(index)
}

/// Create one user `banlieue-g<uid>` and one private group of the same
/// number per guest uid, with the host's `groupadd` and `useradd`. A record
/// already there with the right name and number is kept.
///
/// # Returns
/// How many users were created.
///
/// # Errors
/// [`Error::Unsupported`] when a guest name or number belongs to another
/// account; the I/O error when a command fails.
pub fn register_files(host: &dyn Host, s: &Settings) -> Result<usize, Error> {
    let mut created = 0usize;
    for uid in s.uid_base..=s.uid_end() {
        let name = format!("{GUEST_NAME_PREFIX}{uid}");
        let id = uid.to_string();
        ensure_group(host, &name, &id)?;
        if let Some(line) = host.getent("passwd", &name) {
            if field(&line, 2) == Some(id.as_str()) {
                continue;
            }
            return Err(Error::Unsupported(format!(
                "user {name} exists with another uid ({line}); not touching it"
            )));
        }
        if let Some(line) = host.getent("passwd", &id) {
            return Err(Error::Unsupported(format!(
                "uid {uid} belongs to another account ({line}); not touching it"
            )));
        }
        host.run(&Cmd::new(
            "useradd",
            &[
                "--system",
                "--uid",
                &id,
                "--gid",
                &id,
                "--no-create-home",
                "--no-log-init",
                "--home-dir",
                NO_HOME,
                "--shell",
                NOLOGIN_SHELL,
                &name,
            ],
        ))?;
        created += 1;
    }
    Ok(created)
}

fn ensure_group(host: &dyn Host, name: &str, gid: &str) -> Result<(), Error> {
    if let Some(line) = host.getent("group", name) {
        if field(&line, 2) == Some(gid) {
            return Ok(());
        }
        return Err(Error::Unsupported(format!(
            "group {name} exists with another gid ({line}); not touching it"
        )));
    }
    if let Some(line) = host.getent("group", gid) {
        return Err(Error::Unsupported(format!(
            "gid {gid} belongs to another group ({line}); not touching it"
        )));
    }
    host.run(&Cmd::new("groupadd", &["--system", "--gid", gid, name]))?;
    Ok(())
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod accounts_tests;
