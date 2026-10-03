// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! What a host is to be (ADR-0067 Decision 6): flags, each with a
//! `BANLIEUE_HOST_*` environment variable, resolved against the host for
//! the defaults.

use crate::error::Error;
use crate::ops::Probe;
use clap::Args;
use std::path::{Path, PathBuf};

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

/// Settings shared by every `banlieue host cloud-hypervisor` verb.
#[derive(Clone, Debug, Default, Args)]
pub struct HostArgs {
    /// The `Provider` this host is. Default: the host's short name.
    #[arg(long, env = "BANLIEUE_HOST_PROVIDER_NAME")]
    pub provider_name: Option<String>,
    /// The `Provider`'s namespace.
    #[arg(long, env = "BANLIEUE_HOST_PROVIDER_NAMESPACE", default_value = DEFAULT_NAMESPACE)]
    pub provider_namespace: String,
    /// A storage class, `name=/path`. Repeat, or comma-separate. Default:
    /// `default` on the candidate mount with the most free space.
    #[arg(
        long = "storage-class",
        env = "BANLIEUE_HOST_STORAGE_CLASSES",
        value_delimiter = ',',
        value_name = "NAME=PATH"
    )]
    pub storage_classes: Vec<String>,
    /// A network class, `name=bridge`. The bridge must exist; banlieue never
    /// creates one. Default: `default=virbr0` when it exists.
    #[arg(
        long = "network-class",
        env = "BANLIEUE_HOST_NETWORK_CLASSES",
        value_delimiter = ',',
        value_name = "NAME=BRIDGE"
    )]
    pub network_classes: Vec<String>,
    /// First uid a guest runs as.
    #[arg(long, env = "BANLIEUE_HOST_GUEST_UID_BASE", default_value_t = DEFAULT_GUEST_UID_BASE)]
    pub guest_uid_base: u32,
    /// How many guest uids.
    #[arg(long, env = "BANLIEUE_HOST_GUEST_UID_COUNT", default_value_t = DEFAULT_GUEST_UID_COUNT)]
    pub guest_uid_count: u32,
    /// The one registry repository `Url` images are pulled from, by digest
    /// (ADR-0064). Unset: `BackingFile` images only.
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

/// Resolve `args` against `probe`.
///
/// # Errors
/// [`Error::Setting`] naming the first unusable value.
pub fn resolve(args: &HostArgs, probe: &dyn Probe) -> Result<Settings, Error> {
    let mut storage: Vec<(String, PathBuf)> = pairs("--storage-class", &args.storage_classes)?
        .into_iter()
        .map(|(k, v)| (k, PathBuf::from(v)))
        .collect();
    if let Some((k, p)) = storage.iter().find(|(_, p)| !p.is_absolute()) {
        return Err(Error::Setting {
            name: "--storage-class",
            why: format!("{k}={}: not an absolute path", p.display()),
        });
    }
    if storage.is_empty() {
        storage.push((DEFAULT_CLASS.into(), default_storage(probe)));
    }
    let mut network = pairs("--network-class", &args.network_classes)?;
    if network.is_empty() && probe.exists(&Path::new(SYS_CLASS_NET).join(DEFAULT_BRIDGE)) {
        network.push((DEFAULT_CLASS.into(), DEFAULT_BRIDGE.into()));
    }
    if args.guest_uid_count == 0
        || args
            .guest_uid_base
            .checked_add(args.guest_uid_count)
            .is_none()
    {
        return Err(Error::Setting {
            name: "--guest-uid-count",
            why: format!(
                "{} uids from {} is empty or overflows",
                args.guest_uid_count, args.guest_uid_base
            ),
        });
    }
    let provider_name = args
        .provider_name
        .clone()
        .unwrap_or_else(|| probe.hostname());
    if provider_name.is_empty() {
        return Err(Error::Setting {
            name: "--provider-name",
            why: "empty, and the host has no name".into(),
        });
    }
    let registry = args
        .registry_repository
        .as_ref()
        .filter(|r| !r.is_empty())
        .map(|r| Registry {
            repository: r.clone(),
            plain_http: args.registry_plain_http,
            keep_unreferenced: args.registry_keep_unreferenced,
        });
    Ok(Settings {
        provider_name,
        namespace: args.provider_namespace.clone(),
        storage,
        network,
        uid_base: args.guest_uid_base,
        uid_count: args.guest_uid_count,
        registry,
        allow_virtualized: args.allow_virtualized_host,
    })
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod settings_tests;
