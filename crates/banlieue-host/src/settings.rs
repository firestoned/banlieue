// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! What a host is to be (ADR-0067 Decision 6): flags, each with a
//! `BANLIEUE_HOST_*` environment variable, resolved against the host for
//! the defaults.

use crate::accounts::{self, GuestAccounts};
use crate::attestation;
use crate::error::Error;
use crate::hardening;
use crate::ops::{CgroupMode, Probe};
use crate::paths::HOST_CONFIG;
use banlieue_provider_cloud_hypervisor::host_config::HostConfig;
use clap::Args;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// First guest uid (ADR-0063 Decision 3).
pub const DEFAULT_GUEST_UID_BASE: u32 = 2_000_000;
/// How many guest uids.
pub const DEFAULT_GUEST_UID_COUNT: u32 = 1024;
/// Superseded pulls kept for a rollback (ADR-0064 Decision 4).
pub const DEFAULT_KEEP_UNREFERENCED: u32 =
    banlieue_provider_cloud_hypervisor::host_config::DEFAULT_KEEP_UNREFERENCED;
/// The Provider's default namespace.
pub const DEFAULT_NAMESPACE: &str = "banlieue-system";
/// Where the default storage class may go: the candidate with the most
/// free space wins, since disk images are the largest thing a host stores.
pub const STORAGE_CANDIDATES: [&str; 5] = ["/srv", "/data", "/home", "/opt", "/var/lib"];
/// The default class's directory under the winning candidate.
const STORAGE_SUBDIR: &str = "banlieue/ch";
/// The bridge the default network class uses when it exists.
pub const DEFAULT_BRIDGE: &str = "virbr0";
/// The name of the default storage and network classes.
pub const DEFAULT_CLASS: &str = "default";
/// Interfaces, as the kernel lists them.
pub const SYS_CLASS_NET: &str = "/sys/class/net";
/// The smallest `CPUWeight` systemd accepts.
pub const MIN_CPU_WEIGHT: u32 = 1;
/// The largest `CPUWeight` systemd accepts.
pub const MAX_CPU_WEIGHT: u32 = 10_000;
/// The largest percentage a `MemoryMax=N%` budget may name.
pub const MAX_MEMORY_PERCENT: u32 = 100;
/// The first systemd with `AllowedCPUs=` (the cpuset controller on v2).
pub const MIN_SYSTEMD_ALLOWED_CPUS: u32 = 244;
/// Size suffixes systemd reads as powers of 1024.
const SIZE_SUFFIXES: [char; 4] = ['K', 'M', 'G', 'T'];

/// Settings shared by every `banlieue host cloud-hypervisor` verb.
///
/// On a host whose config `install` keeps (no `--force`), every setting
/// that config records (provider name and namespace, storage and network
/// classes, the guest uid range, the registry) comes from it when the flag
/// is not given, and a flag that contradicts it is refused: the config is
/// not rewritten, so units rendered from the flag would disagree with what
/// the provider reads.
#[derive(Clone, Debug, Default, Args)]
pub struct HostArgs {
    /// The `Provider` this host is. Default: the kept host config's, else
    /// the host's short name.
    #[arg(long, env = "BANLIEUE_HOST_PROVIDER_NAME")]
    pub provider_name: Option<String>,
    /// The `Provider`'s namespace. Default: the kept host config's, else
    /// `banlieue-system`.
    #[arg(long, env = "BANLIEUE_HOST_PROVIDER_NAMESPACE")]
    pub provider_namespace: Option<String>,
    /// A storage class, `name=/path`. Repeat, or comma-separate. Default:
    /// the kept host config's, else `default` on the candidate mount with
    /// the most free space.
    #[arg(
        long = "storage-class",
        env = "BANLIEUE_HOST_STORAGE_CLASSES",
        value_delimiter = ',',
        value_name = "NAME=PATH"
    )]
    pub storage_classes: Vec<String>,
    /// A network class, `name=bridge`. The bridge must exist; banlieue never
    /// creates one. Default: the kept host config's, else `default=virbr0`
    /// when it exists.
    #[arg(
        long = "network-class",
        env = "BANLIEUE_HOST_NETWORK_CLASSES",
        value_delimiter = ',',
        value_name = "NAME=BRIDGE"
    )]
    pub network_classes: Vec<String>,
    /// First uid a guest runs as. Default: the kept host config's, else
    /// 2000000.
    #[arg(long, env = "BANLIEUE_HOST_GUEST_UID_BASE")]
    pub guest_uid_base: Option<u32>,
    /// How many guest uids. Default: the kept host config's, else 1024.
    #[arg(long, env = "BANLIEUE_HOST_GUEST_UID_COUNT")]
    pub guest_uid_count: Option<u32>,
    /// The one registry repository `Url` images are pulled from, by digest
    /// (ADR-0064). Unset: the kept host config's registry, else
    /// `BackingFile` images only.
    #[arg(long, env = "BANLIEUE_HOST_REGISTRY_REPOSITORY")]
    pub registry_repository: Option<String>,
    /// Speak `http://` to the registry (a private network or a test only).
    #[arg(long, env = "BANLIEUE_HOST_REGISTRY_PLAIN_HTTP")]
    pub registry_plain_http: bool,
    /// Superseded pulls to keep for a rollback.
    #[arg(long, env = "BANLIEUE_HOST_REGISTRY_KEEP_UNREFERENCED", default_value_t = DEFAULT_KEEP_UNREFERENCED)]
    pub registry_keep_unreferenced: u32,
    /// Accept a host that is itself a VM (nested virtualization): a lab
    /// only, unsupported.
    #[arg(long, env = "BANLIEUE_HOST_ALLOW_VIRTUALIZED_HOST")]
    pub allow_virtualized_host: bool,
    /// CPUs the guests may run on together (`AllowedCPUs=` on the guests
    /// slice, ADR-0105), a cpuset list such as `0-15,32-47`. Unset: any.
    /// Refused on cgroup v1 and on systemd before 244, which would accept
    /// it and never enforce it (ADR-0139): use `--guest-cpu-quota` there.
    #[arg(long, env = "BANLIEUE_HOST_GUEST_CPUS", value_name = "CPUSET")]
    pub guest_cpus: Option<String>,
    /// The guests' CPU share against other workloads (`CPUWeight=`, 1-10000;
    /// systemd's default is 100). Unset: systemd's default.
    #[arg(long, env = "BANLIEUE_HOST_GUEST_CPU_WEIGHT", value_name = "WEIGHT")]
    pub guest_cpu_weight: Option<u32>,
    /// A ceiling on all guests' memory together (`MemoryMax=`): bytes with
    /// an optional K/M/G/T suffix, or a percentage such as `60%`. Unset:
    /// only each guest's own limit applies.
    #[arg(long, env = "BANLIEUE_HOST_GUEST_MEMORY_MAX", value_name = "SIZE")]
    pub guest_memory_max: Option<String>,
    /// A ceiling on all guests' processes and threads together
    /// (`TasksMax=`). Unset: each guest's own limit applies.
    #[arg(long, env = "BANLIEUE_HOST_GUEST_TASKS_MAX", value_name = "COUNT")]
    pub guest_tasks_max: Option<u64>,
    /// A ceiling on the CPU time all guests get together (`CPUQuota=`, ADR-0139):
    /// a positive percentage such as `4800%`, meaning 48 CPUs' worth of
    /// time. A share of time, not a set of cores; works on cgroup v1 and v2.
    #[arg(long, env = "BANLIEUE_HOST_GUEST_CPU_QUOTA", value_name = "PERCENT")]
    pub guest_cpu_quota: Option<String>,
    /// How guest uids become accounts (ADR-0139). Unset: derived from the
    /// host (systemd 245 or newer with the `systemd` NSS source: userdb;
    /// otherwise files).
    #[arg(long, env = "BANLIEUE_HOST_GUEST_ACCOUNTS", value_enum)]
    pub guest_accounts: Option<GuestAccounts>,
    /// Unit directives this host's systemd cannot enforce, which the
    /// operator accepts running without (ADR-0139). Must name exactly the
    /// set the host lacks; recorded in the host config.
    #[arg(
        long,
        env = "BANLIEUE_HOST_ACCEPT_REDUCED_HARDENING",
        value_delimiter = ',',
        value_name = "DIRECTIVE"
    )]
    pub accept_reduced_hardening: Vec<String>,
    /// Opt in to x509pop attestation (ADR-0146): create this host's
    /// attestation CA, named by the host's short name. Refused when that
    /// name has characters outside letters, digits, dots, dashes and
    /// underscores and no `--attestation-ca-name` is given.
    #[arg(long, env = "BANLIEUE_HOST_ATTESTATION")]
    pub attestation: bool,
    /// Opt in to x509pop attestation with this CA common name (letters,
    /// digits, dots, dashes, underscores). It is in every agent ID the host
    /// signs, so it must be unique across the SPIRE server's bundle.
    #[arg(long, env = "BANLIEUE_HOST_ATTESTATION_CA_NAME", value_name = "NAME")]
    pub attestation_ca_name: Option<String>,
    /// How long a certificate the host signs is valid: `30m`, `1h`, `12h`
    /// (5 minutes to 7 days). Default: the kept host config's, else 1 hour.
    #[arg(
        long,
        env = "BANLIEUE_HOST_ATTESTATION_CERTIFICATE_TTL",
        value_name = "DURATION"
    )]
    pub attestation_certificate_ttl: Option<String>,
    /// Replace the attestation CA with a NEW one. Needs a new
    /// `--attestation-ca-name`, different from the current one: a name is
    /// never reused for a different key. `--force` never rotates it.
    #[arg(long, env = "BANLIEUE_HOST_ROTATE_ATTESTATION_CA")]
    pub rotate_attestation_ca: bool,
}

/// The guests' shared budget on a host that runs other workloads
/// (ADR-0105). Each field is one `banlieue-guests.slice` setting; unset
/// emits no line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GuestSlice {
    /// `AllowedCPUs=`.
    pub cpus: Option<String>,
    /// `CPUWeight=`.
    pub cpu_weight: Option<u32>,
    /// `CPUQuota=`, a percentage such as `4800%`.
    pub cpu_quota: Option<String>,
    /// `MemoryMax=`.
    pub memory_max: Option<String>,
    /// `TasksMax=`.
    pub tasks_max: Option<u64>,
}

impl GuestSlice {
    /// Whether no limit at all is declared.
    #[must_use]
    pub fn is_unbounded(&self) -> bool {
        *self == Self::default()
    }

    /// The `[Slice]` lines for what is declared, in a fixed order, each
    /// newline-terminated; empty when nothing is.
    #[must_use]
    pub fn limits(&self) -> String {
        let mut out = String::new();
        if let Some(v) = &self.cpus {
            out.push_str(&format!("AllowedCPUs={v}\n"));
        }
        if let Some(v) = self.cpu_weight {
            out.push_str(&format!("CPUWeight={v}\n"));
        }
        if let Some(v) = &self.cpu_quota {
            out.push_str(&format!("CPUQuota={v}\n"));
        }
        if let Some(v) = &self.memory_max {
            out.push_str(&format!("MemoryMax={v}\n"));
        }
        if let Some(v) = self.tasks_max {
            out.push_str(&format!("TasksMax={v}\n"));
        }
        out
    }
}

/// The registry section, when there is one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registry {
    /// `registry/repository`.
    pub repository: String,
    /// `http://`.
    pub plain_http: bool,
    /// Superseded pulls to keep.
    pub keep_unreferenced: u32,
}

impl Registry {
    /// One line, for a message.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} (plain_http {}, keep_unreferenced {})",
            self.repository, self.plain_http, self.keep_unreferenced
        )
    }
}

/// What this run does about the attestation CA (ADR-0146 Decision 5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestationPlan {
    /// The name of the CA this run creates; `None` keeps the CA that is
    /// there.
    pub new_ca_name: Option<String>,
    /// How long a certificate the host signs is valid.
    pub ttl: Duration,
    /// Whether the CA that is there is replaced (never without a new name).
    pub rotate: bool,
}

/// Settings, resolved and validated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The Provider's name.
    pub provider_name: String,
    /// Its namespace.
    pub namespace: String,
    /// Storage classes, in the order given.
    pub storage: Vec<(String, PathBuf)>,
    /// Network classes, in the order given.
    pub network: Vec<(String, String)>,
    /// First guest uid.
    pub uid_base: u32,
    /// Guest uid count.
    pub uid_count: u32,
    /// The registry, if any.
    pub registry: Option<Registry>,
    /// Accept a virtualized host.
    pub allow_virtualized: bool,
    /// The guests' shared budget.
    pub guest_slice: GuestSlice,
    /// How guest accounts are registered (ADR-0139).
    pub guest_accounts: GuestAccounts,
    /// Why that mode: the facts it was derived from, or the flag.
    pub accounts_reason: String,
    /// Directives the operator accepted this host not enforcing, sorted.
    pub accepted_reduced: Vec<String>,
    /// The attestation CA, when the host opted in (now or earlier).
    pub attestation: Option<AttestationPlan>,
}

impl Settings {
    /// The last guest uid.
    #[must_use]
    pub fn uid_end(&self) -> u32 {
        self.uid_base + self.uid_count - 1
    }
}

fn valid_class_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn pairs(name: &'static str, raw: &[String]) -> Result<Vec<(String, String)>, Error> {
    let mut out: Vec<(String, String)> = Vec::new();
    for item in raw.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let (k, v) = item.split_once('=').ok_or_else(|| Error::Setting {
            name,
            why: format!("{item:?} is not name=value"),
        })?;
        if !valid_class_name(k) {
            return Err(Error::Setting {
                name,
                why: format!("class name {k:?}: lowercase letters, digits and '-' only"),
            });
        }
        if v.is_empty() || v.contains(char::is_whitespace) || v.contains('"') {
            return Err(Error::Setting {
                name,
                why: format!("{item:?}: empty, or has whitespace or a quote"),
            });
        }
        if out.iter().any(|(existing, _)| existing == k) {
            return Err(Error::Setting {
                name,
                why: format!("class {k} given twice"),
            });
        }
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

fn bad(name: &'static str, why: impl Into<String>) -> Error {
    Error::Setting {
        name,
        why: why.into(),
    }
}

/// A cpuset list: comma-separated `N` or `N-M` with `N <= M`.
fn cpuset(raw: &str) -> Result<String, Error> {
    const NAME: &str = "--guest-cpus";
    let number = |t: &str| t.parse::<u32>().ok().filter(|_| !t.starts_with('+'));
    for item in raw.split(',') {
        let ok = match item.split_once('-') {
            None => number(item).is_some(),
            Some((lo, hi)) => matches!((number(lo), number(hi)), (Some(a), Some(b)) if a <= b),
        };
        if !ok {
            return Err(bad(
                NAME,
                format!("{raw:?}: not a cpuset list like 0-15,32"),
            ));
        }
    }
    Ok(raw.to_string())
}

/// A memory ceiling: a positive size with an optional `K`/`M`/`G`/`T`
/// suffix, or `1%`-`100%`.
fn memory_max(raw: &str) -> Result<String, Error> {
    const NAME: &str = "--guest-memory-max";
    let positive = |t: &str| {
        !t.is_empty()
            && t.chars().all(|c| c.is_ascii_digit())
            && t.parse::<u64>().is_ok_and(|n| n > 0)
    };
    let ok = match raw.strip_suffix('%') {
        Some(p) => positive(p) && p.parse::<u32>().is_ok_and(|n| n <= MAX_MEMORY_PERCENT),
        None => positive(raw.strip_suffix(SIZE_SUFFIXES).unwrap_or(raw)),
    };
    if ok {
        return Ok(raw.to_string());
    }
    Err(bad(
        NAME,
        format!("{raw:?}: not a size like 400G, 1048576 or 60%"),
    ))
}

/// A CPU quota: a positive integer and `%`, nothing else.
fn cpu_quota(raw: &str) -> Result<String, Error> {
    const NAME: &str = "--guest-cpu-quota";
    let ok = raw.strip_suffix('%').is_some_and(|p| {
        !p.starts_with('0')
            && p.chars().all(|c| c.is_ascii_digit())
            && p.parse::<u64>().is_ok_and(|n| n > 0)
    });
    if ok {
        return Ok(raw.to_string());
    }
    Err(bad(
        NAME,
        format!("{raw:?}: not a positive percentage like 4800%"),
    ))
}

/// Refuse `--guest-cpus` where the host would accept it and never enforce it
/// (ADR-0139 Decision 2).
fn check_cpuset_enforced(probe: &dyn Probe) -> Result<(), Error> {
    const NAME: &str = "--guest-cpus";
    if probe.cgroup_mode() == CgroupMode::Legacy {
        return Err(bad(
            NAME,
            "AllowedCPUs= is not enforced on cgroup v1 (no cpuset cgroup is created, so \
             nothing would be pinned); use --guest-cpu-quota for a ceiling, a share of time \
             and not cores",
        ));
    }
    if let Some(v) = probe.systemd_version()
        && v < MIN_SYSTEMD_ALLOWED_CPUS
    {
        return Err(bad(
            NAME,
            format!(
                "AllowedCPUs= needs systemd {MIN_SYSTEMD_ALLOWED_CPUS} or newer, this host has {v}; \
                 use --guest-cpu-quota"
            ),
        ));
    }
    Ok(())
}

/// Validate the budget flags (ADR-0105): each value ends up in a root-owned
/// unit file, so it is checked before it is rendered.
fn guest_slice(args: &HostArgs, probe: &dyn Probe) -> Result<GuestSlice, Error> {
    if let Some(w) = args.guest_cpu_weight
        && !(MIN_CPU_WEIGHT..=MAX_CPU_WEIGHT).contains(&w)
    {
        return Err(bad(
            "--guest-cpu-weight",
            format!("{w}: {MIN_CPU_WEIGHT} to {MAX_CPU_WEIGHT}"),
        ));
    }
    if args.guest_tasks_max == Some(0) {
        return Err(bad("--guest-tasks-max", "0 would forbid every guest"));
    }
    if args.guest_cpus.is_some() {
        check_cpuset_enforced(probe)?;
    }
    Ok(GuestSlice {
        cpus: args.guest_cpus.as_deref().map(cpuset).transpose()?,
        cpu_weight: args.guest_cpu_weight,
        cpu_quota: args.guest_cpu_quota.as_deref().map(cpu_quota).transpose()?,
        memory_max: args
            .guest_memory_max
            .as_deref()
            .map(memory_max)
            .transpose()?,
        tasks_max: args.guest_tasks_max,
    })
}

/// The candidate mount with the most free space, as a storage class path.
#[must_use]
pub fn default_storage(probe: &dyn Probe) -> PathBuf {
    STORAGE_CANDIDATES
        .iter()
        .map(Path::new)
        .filter(|p| probe.is_dir(p))
        .filter_map(|p| Some((probe.free_bytes(p)?, p)))
        .max_by_key(|(free, _)| *free)
        .map_or_else(|| PathBuf::from("/var/lib"), |(_, p)| p.to_path_buf())
        .join(STORAGE_SUBDIR)
}

/// Resolve `args` against `probe` and the host config `install` keeps,
/// if there is one: what that config records is used where no flag is
/// given, and a flag that contradicts it is refused.
///
/// # Errors
/// [`Error::Setting`] naming the first unusable value or contradicting
/// flag; [`Error::KeptConfig`] when the kept config does not load.
pub fn resolve(args: &HostArgs, probe: &dyn Probe) -> Result<Settings, Error> {
    let kept = kept_host_config(probe)?;
    resolve_with(args, probe, kept.as_ref())
}

/// Resolve `args` against `probe` alone, as for a config `install
/// --force` regenerates: the kept config, if any, is not consulted.
///
/// # Errors
/// [`Error::Setting`] naming the first unusable value.
pub fn resolve_fresh(args: &HostArgs, probe: &dyn Probe) -> Result<Settings, Error> {
    resolve_with(args, probe, None)
}

/// The host config `install` keeps, if one is there.
///
/// # Errors
/// [`Error::KeptConfig`] when it exists but cannot be read or does not
/// load as the provider would load it.
pub fn kept_host_config(probe: &dyn Probe) -> Result<Option<HostConfig>, Error> {
    let raw = match probe.read(Path::new(HOST_CONFIG)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::KeptConfig(e.to_string())),
    };
    HostConfig::parse(&String::from_utf8_lossy(&raw))
        .map(Some)
        .map_err(|e| Error::KeptConfig(e.to_string()))
}

/// A flag the kept config contradicts.
fn contradicts(name: &'static str, given: &str, kept: &str) -> Error {
    Error::Setting {
        name,
        why: format!(
            "{given}, but {HOST_CONFIG}, which install keeps, says {kept}: drop the flag, \
             edit the file, or pass --force to regenerate it (which also rotates the EK CA)"
        ),
    }
}

/// `given` when there is no kept value or it agrees; the kept value when
/// nothing is given; refused when they differ.
fn agree<T: PartialEq + std::fmt::Display + Clone>(
    name: &'static str,
    given: Option<T>,
    kept: Option<T>,
) -> Result<Option<T>, Error> {
    match (given, kept) {
        (Some(g), Some(k)) if g != k => Err(contradicts(name, &g.to_string(), &k.to_string())),
        (g, k) => Ok(g.or(k)),
    }
}

fn class_list<V: AsRef<str>>(classes: &BTreeMap<String, V>) -> String {
    classes
        .iter()
        .map(|(k, v)| format!("{k}={}", v.as_ref()))
        .collect::<Vec<_>>()
        .join(",")
}

/// The given classes, or the kept ones; refused when both exist and differ
/// as sets (the order given does not matter, the config has none).
fn agree_classes<V: Clone + PartialEq + AsRef<str>>(
    name: &'static str,
    given: Vec<(String, V)>,
    kept: Option<&BTreeMap<String, V>>,
) -> Result<Vec<(String, V)>, Error> {
    let Some(kept) = kept else {
        return Ok(given);
    };
    let as_map: BTreeMap<String, V> = given.iter().cloned().collect();
    if !given.is_empty() && &as_map != kept {
        return Err(contradicts(name, &class_list(&as_map), &class_list(kept)));
    }
    // The kept order, always: the same classes given in another order must
    // render the same units, or a re-run would restart the provider.
    Ok(kept.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// The given registry, or the kept one; refused when both exist and differ.
fn agree_registry(
    given: Option<Registry>,
    kept: Option<Registry>,
) -> Result<Option<Registry>, Error> {
    match (given, kept) {
        (Some(g), Some(k)) if g != k => Err(contradicts(
            "--registry-repository",
            &g.describe(),
            &k.describe(),
        )),
        (Some(g), None) => Ok(Some(g)),
        (_, k) => Ok(k),
    }
}

/// The directives the operator accepts this host not enforcing.
///
/// A flag is the operator's statement for this run and replaces whatever the
/// kept config records: `[hardening]` is a section the installer keeps
/// current (like `[vmm]`), and the preflight refuses a list that is not
/// exactly what the host lacks, so it needs no `--force`. With no flag, the
/// kept acceptance stands for the directives the host still does not
/// enforce, so a plain re-run does not demand the flag again; entries the
/// host now enforces (a systemd upgrade) are dropped.
fn accepted_hardening(
    args: &HostArgs,
    probe: &dyn Probe,
    kept: Option<&HostConfig>,
) -> Result<Vec<String>, Error> {
    let given = hardening::accepted(&args.accept_reduced_hardening)
        .map_err(|why| bad("--accept-reduced-hardening", why))?;
    let Some(kept) = kept.filter(|_| given.is_empty()) else {
        return Ok(given);
    };
    let mut recorded =
        hardening::accepted(kept.accepted_reduced_hardening()).map_err(Error::KeptConfig)?;
    if let Some(version) = probe.systemd_version() {
        let lacking = hardening::unenforced(version);
        recorded.retain(|name| lacking.contains(&name.as_str()));
    }
    Ok(recorded)
}

fn resolve_with(
    args: &HostArgs,
    probe: &dyn Probe,
    kept: Option<&HostConfig>,
) -> Result<Settings, Error> {
    let given_storage: Vec<(String, PathBuf)> = pairs("--storage-class", &args.storage_classes)?
        .into_iter()
        .map(|(k, v)| (k, PathBuf::from(v)))
        .collect();
    if let Some((k, p)) = given_storage.iter().find(|(_, p)| !p.is_absolute()) {
        return Err(Error::Setting {
            name: "--storage-class",
            why: format!("{k}={}: not an absolute path", p.display()),
        });
    }
    let mut storage = agree_classes(
        "--storage-class",
        given_storage
            .into_iter()
            .map(|(k, p)| (k, StoragePath(p)))
            .collect(),
        kept.map(|c| {
            c.storage_classes
                .iter()
                .map(|(k, p)| (k.clone(), StoragePath(p.clone())))
                .collect::<BTreeMap<_, _>>()
        })
        .as_ref(),
    )?
    .into_iter()
    .map(|(k, p)| (k, p.0))
    .collect::<Vec<_>>();
    if storage.is_empty() {
        storage.push((DEFAULT_CLASS.into(), default_storage(probe)));
    }
    let mut network = agree_classes(
        "--network-class",
        pairs("--network-class", &args.network_classes)?,
        kept.map(|c| &c.network_classes),
    )?;
    if network.is_empty()
        && kept.is_none()
        && probe.exists(&Path::new(SYS_CLASS_NET).join(DEFAULT_BRIDGE))
    {
        network.push((DEFAULT_CLASS.into(), DEFAULT_BRIDGE.into()));
    }
    let uid_base = agree(
        "--guest-uid-base",
        args.guest_uid_base,
        kept.map(|c| c.guests.uid_base),
    )?
    .unwrap_or(DEFAULT_GUEST_UID_BASE);
    let uid_count = agree(
        "--guest-uid-count",
        args.guest_uid_count,
        kept.map(|c| c.guests.uid_count),
    )?
    .unwrap_or(DEFAULT_GUEST_UID_COUNT);
    if uid_count == 0 || uid_base.checked_add(uid_count).is_none() {
        return Err(Error::Setting {
            name: "--guest-uid-count",
            why: format!("{uid_count} uids from {uid_base} is empty or overflows"),
        });
    }
    let provider_name = agree(
        "--provider-name",
        args.provider_name.clone(),
        kept.map(|c| c.provider.name.clone()),
    )?
    .unwrap_or_else(|| probe.hostname());
    if provider_name.is_empty() {
        return Err(Error::Setting {
            name: "--provider-name",
            why: "empty, and the host has no name".into(),
        });
    }
    let namespace = agree(
        "--provider-namespace",
        args.provider_namespace.clone(),
        kept.map(|c| c.provider.namespace.clone()),
    )?
    .unwrap_or_else(|| DEFAULT_NAMESPACE.into());
    let given_registry = args
        .registry_repository
        .as_ref()
        .filter(|r| !r.is_empty())
        .map(|r| Registry {
            repository: r.clone(),
            plain_http: args.registry_plain_http,
            keep_unreferenced: args.registry_keep_unreferenced,
        });
    let kept_registry = kept.and_then(|c| c.registry.as_ref()).map(|r| Registry {
        repository: r.repository.clone(),
        plain_http: r.plain_http,
        keep_unreferenced: r.keep_unreferenced,
    });
    let registry = if kept.is_some() {
        agree_registry(given_registry, kept_registry)?
    } else {
        given_registry
    };
    let (guest_accounts, accounts_reason) = accounts::choose(args.guest_accounts, probe)?;
    let accepted_reduced = accepted_hardening(args, probe, kept)?;
    let attestation = attestation_plan(args, probe)?;
    Ok(Settings {
        provider_name,
        namespace,
        storage,
        network,
        uid_base,
        uid_count,
        registry,
        allow_virtualized: args.allow_virtualized_host,
        guest_slice: guest_slice(args, probe)?,
        guest_accounts,
        accounts_reason,
        accepted_reduced,
        attestation,
    })
}

fn ca_files_present(probe: &dyn Probe) -> bool {
    let dir = Path::new(crate::paths::ATTESTATION_CA_DIR);
    [attestation::CA_KEY_FILE, attestation::CA_CERT_FILE]
        .iter()
        .any(|f| probe.stat(&dir.join(f)).is_some())
}

/// The attestation CA flags against what is on the host (ADR-0146
/// Decision 5).
///
/// The opt-in is `--attestation-ca-name` or `--attestation`, or an
/// `[attestation]` section already in the host config (kept by a plain
/// re-run, and by `--force`, which regenerates the file but never rotates
/// the CA). A name is validated and refused, never replaced; a CA that is
/// there is never replaced by a different name without
/// `--rotate-attestation-ca`, which needs a new name, different from the
/// current one.
fn attestation_plan(args: &HostArgs, probe: &dyn Probe) -> Result<Option<AttestationPlan>, Error> {
    const NAME_FLAG: &str = "--attestation-ca-name";
    const TTL_FLAG: &str = "--attestation-certificate-ttl";
    const ROTATE_FLAG: &str = "--rotate-attestation-ca";
    let given_name = match (&args.attestation_ca_name, args.attestation) {
        (Some(n), _) => {
            banlieue_attestation_ca::validate_name(n)
                .map_err(|e| bad(NAME_FLAG, format!("{n:?}: {e}")))?;
            Some(n.clone())
        }
        (None, true) => Some(
            attestation::default_name(&probe.hostname())
                .map_err(|why| bad("--attestation", why))?,
        ),
        (None, false) => None,
    };
    let given_ttl = args
        .attestation_certificate_ttl
        .as_deref()
        .map(attestation::parse_ttl)
        .transpose()
        .map_err(|why| bad(TTL_FLAG, why))?;
    let existing = attestation::existing_section(probe);
    let dir = Path::new(crate::paths::ATTESTATION_CA_DIR);
    let files = ca_files_present(probe);
    let current = probe.attestation_ca(dir);

    if args.rotate_attestation_ca {
        let Some(name) = args.attestation_ca_name.as_ref() else {
            return Err(bad(
                ROTATE_FLAG,
                "a new CA needs a new name: give --attestation-ca-name",
            ));
        };
        if !files {
            return Err(bad(ROTATE_FLAG, "there is no attestation CA to rotate"));
        }
        let info = current.as_ref().map_err(|why| {
            bad(
                ROTATE_FLAG,
                format!("the current CA cannot be read ({why}); fix or remove it by hand"),
            )
        })?;
        if &info.name == name {
            return Err(bad(
                NAME_FLAG,
                format!(
                    "{name:?} is the current CA's name: a name is never reused for a different                      key, give a new one"
                ),
            ));
        }
    }

    if given_name.is_none() && existing.is_none() {
        if given_ttl.is_some() {
            return Err(bad(
                TTL_FLAG,
                "this host has not opted in: give --attestation or --attestation-ca-name",
            ));
        }
        return Ok(None);
    }

    let ttl = given_ttl
        .or_else(|| {
            existing
                .as_ref()
                .map(|s| Duration::from_secs(s.certificate_ttl_secs))
        })
        .unwrap_or(attestation::DEFAULT_CERTIFICATE_TTL);
    if args.rotate_attestation_ca {
        return Ok(Some(AttestationPlan {
            new_ca_name: given_name,
            ttl,
            rotate: true,
        }));
    }
    if !files {
        let name = match given_name {
            Some(n) => n,
            None => attestation::default_name(&probe.hostname())
                .map_err(|why| bad("--attestation", why))?,
        };
        return Ok(Some(AttestationPlan {
            new_ca_name: Some(name),
            ttl,
            rotate: false,
        }));
    }
    if let (Some(name), Ok(info)) = (&given_name, &current)
        && &info.name != name
    {
        return Err(bad(
            NAME_FLAG,
            format!(
                "{name:?}, but the attestation CA here is named {:?}: a CA is never replaced                  under a new name unless --rotate-attestation-ca says so",
                info.name
            ),
        ));
    }
    Ok(Some(AttestationPlan {
        new_ca_name: None,
        ttl,
        rotate: false,
    }))
}

/// A storage path, compared and printed as the config spells it.
#[derive(Clone, PartialEq, Eq)]
struct StoragePath(PathBuf);

impl AsRef<str> for StoragePath {
    fn as_ref(&self) -> &str {
        self.0.to_str().unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod settings_tests;
