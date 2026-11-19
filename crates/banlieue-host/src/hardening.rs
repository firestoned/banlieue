// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit directives that need a minimum systemd, and the acceptance an
//! operator records when a host is older (ADR-0139 Decision 4).
//!
//! A directive an old systemd does not know is not an error there: it logs
//! `Unknown lvalue` once and the unit starts without it. The sandbox is then
//! weaker than the template says, silently. So every directive with a minimum
//! version is a named constant here, the installer lists the ones the host
//! will not enforce, and it refuses to continue unless the operator names
//! exactly that list, which is then written into the host config.

/// The oldest systemd every other directive in the units was measured on
/// (ADR-0139: RHEL 8, systemd 239).
pub const BASELINE_SYSTEMD: u32 = 239;

/// A unit directive and the first systemd that enforces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Directive {
    /// The directive, without `=`.
    pub name: &'static str,
    /// The first systemd major version that knows it.
    pub min_systemd: u32,
}

/// `PrivateIPC=`: a private SysV IPC and POSIX message queue namespace per
/// unit. Unknown to systemd before 248.
pub const PRIVATE_IPC: Directive = Directive {
    name: "PrivateIPC",
    min_systemd: 248,
};

/// Every directive in the templates that needs more than
/// [`BASELINE_SYSTEMD`].
pub const VERSIONED_DIRECTIVES: [Directive; 1] = [PRIVATE_IPC];

/// Every other directive the installed units use. Each was accepted by
/// systemd 239 and checked for its effect there (ADR-0139, Context).
pub const BASELINE_DIRECTIVES: [&str; 29] = [
    "After",
    "AmbientCapabilities",
    "AssertPathExists",
    "CapabilityBoundingSet",
    "CollectMode",
    "ConditionPathExists",
    "Description",
    "DeviceAllow",
    "DevicePolicy",
    "Documentation",
    "EnvironmentFile",
    "ExecStart",
    "Group",
    "KillMode",
    "NoNewPrivileges",
    "PrivateTmp",
    "ProtectHome",
    "ProtectSystem",
    "ReadOnlyPaths",
    "ReadWritePaths",
    "Restart",
    "RestartSec",
    "Slice",
    "SupplementaryGroups",
    "TasksMax",
    "Type",
    "User",
    "WantedBy",
    "Wants",
];

/// The major version in `systemctl --version`'s first line
/// (`systemd 239 (239-82.el8)`, `systemd 256~rc3 (...)`).
#[must_use]
pub fn parse_systemd_version(output: &str) -> Option<u32> {
    let word = output.lines().next()?.strip_prefix("systemd ")?;
    let digits: String = word.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The directives a systemd of `version` would not enforce.
#[must_use]
pub fn unenforced(version: u32) -> Vec<&'static str> {
    VERSIONED_DIRECTIVES
        .iter()
        .filter(|d| version < d.min_systemd)
        .map(|d| d.name)
        .collect()
}

/// Why `accepted` does not match what a host running `version` lacks;
/// empty when it matches exactly.
#[must_use]
pub fn problems(version: Option<u32>, accepted: &[String]) -> Vec<String> {
    let Some(version) = version else {
        return vec![
            "cannot read the systemd version (`systemctl --version`): the hardening this host \
             enforces cannot be told"
                .into(),
        ];
    };
    let missing = unenforced(version);
    let same =
        missing.len() == accepted.len() && missing.iter().all(|m| accepted.iter().any(|a| a == m));
    if same {
        return vec![];
    }
    let list = missing.join(",");
    if missing.is_empty() {
        return vec![format!(
            "--accept-reduced-hardening={}: systemd {version} enforces every directive, so there \
             is nothing to accept; remove the flag",
            accepted.join(",")
        )];
    }
    vec![format!(
        "systemd {version} will not enforce {list} (a sandbox weaker than the unit templates say); \
         pass --accept-reduced-hardening={list} to accept that, recorded in the host config"
    )]
}

/// `raw` as a sorted, deduplicated list of known directive names.
///
/// # Errors
/// The message naming the first unknown directive.
pub fn accepted(raw: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for name in raw.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if !VERSIONED_DIRECTIVES.iter().any(|d| d.name == name) {
            let known: Vec<&str> = VERSIONED_DIRECTIVES.iter().map(|d| d.name).collect();
            return Err(format!(
                "{name:?} has no minimum systemd version to accept; known: {}",
                known.join(", ")
            ));
        }
        out.push(name.to_string());
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// `text` without the lines that set any of `names` (`Name=...`). Comments
/// and every other line are kept as they are.
#[must_use]
pub fn drop_directives(text: &str, names: &[String]) -> String {
    text.split_inclusive('\n')
        .filter(|line| {
            let key = line.split_once('=').map(|(k, _)| k);
            !key.is_some_and(|k| names.iter().any(|n| n == k))
        })
        .collect()
}

#[cfg(test)]
#[path = "hardening_tests.rs"]
mod hardening_tests;
