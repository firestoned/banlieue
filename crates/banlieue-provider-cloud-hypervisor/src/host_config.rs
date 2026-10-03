// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The host-local configuration (ADR-0062 Decision 4).
//!
//! Written once by `banlieue host cloud-hypervisor install` (ADR-0067, which renders it from
//! this very type and parses it back), read by the provider at start. It is the **only** source of host paths and bridge
//! names: machines name a storage or network *class*, and this file alone
//! says what directory or bridge that class means on this host. A cluster
//! credential that is stolen can therefore choose among what the host owner
//! declared here, and nothing else on the host.
//!
//! Parsing is strict. Unknown keys, relative paths, a uid range that reaches
//! root or overflows, and impossible interface names are all refused, since a
//! mistake here would otherwise surface much later as a guest that fails to
//! start, or as one that starts somewhere it should not.

use banlieue_provider_sdk::ssa::{FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR, provider_field_manager};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Longest Linux interface name: `IFNAMSIZ` (16) minus the terminating NUL.
pub const MAX_INTERFACE_NAME: usize = 15;
/// Longest class name: a DNS label.
const MAX_CLASS_NAME: usize = 63;

/// Why the host config was refused.
#[derive(Debug, thiserror::Error)]
pub enum HostConfigError {
    /// The file could not be read.
    #[error("reading {path}: {source}")]
    Read {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// The file is not valid TOML of the expected shape.
    #[error("parsing host config: {0}")]
    Parse(#[from] toml::de::Error),
    /// The file parsed but a value is not acceptable.
    #[error("invalid host config: {0}")]
    Invalid(String),
}
/// The whole host config.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// Which `Provider` this host is.
    pub provider: ProviderSection,
    /// The pinned VMM and firmware.
    pub vmm: VmmSection,
    /// Runtime and state directories.
    pub paths: PathsSection,
    /// The uid range guests run as (ADR-0063 Decision 3).
    pub guests: GuestsSection,
    /// swtpm, when this host offers a vTPM (ADR-0065). Absent means no vTPM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpm: Option<TpmSection>,
    /// Where `Url` images are pulled from (ADR-0064). Absent means this host
    /// serves `BackingFile` images only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<RegistrySection>,
    /// Storage class name to directory.
    pub storage_classes: BTreeMap<String, PathBuf>,
    /// Network class name to bridge.
    pub network_classes: BTreeMap<String, String>,
}

impl HostConfig {
    /// This host's field manager: the Cloud Hypervisor class scoped to the
    /// `Provider` this host serves (ADR-0087).
    ///
    /// One systemd unit per host (ADR-0060) means one writer per host, and
    /// every host runs the same class. Applying under the bare class constant
    /// made each host's apply delete the other hosts' `VMImage.status`
    /// `perProvider` rows, which produced a permanent write loop rather than
    /// an error.
    #[must_use]
    pub fn field_manager(&self) -> String {
        provider_field_manager(
            FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR,
            &self.provider.namespace,
            &self.provider.name,
        )
    }
}

/// `[provider]`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSection {
    /// The `Provider` object's name. One Provider is one host (ADR-0060).
    pub name: String,
    /// Its namespace.
    pub namespace: String,
    /// Kubeconfig the provider authenticates with (ADR-0060 Decision 5).
    pub kubeconfig: PathBuf,
}

/// `[vmm]`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VmmSection {
    /// The `cloud-hypervisor` binary.
    pub binary: PathBuf,
    /// The version the bootstrap installed, for display. The provider trusts
    /// `vmm.ping`, not this string (ADR-0061 Decision 5).
    pub version: String,
    /// `CLOUDHV.fd`.
    pub firmware: PathBuf,
}

/// `[paths]`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathsSection {
    /// Per-guest sockets live under here (tmpfs).
    pub run_root: PathBuf,
    /// Provider state (the EK CA) lives under here.
    pub state_root: PathBuf,
}

/// `[guests]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GuestsSection {
    /// First uid a guest may run as.
    pub uid_base: u32,
    /// How many uids, from `uid_base`.
    pub uid_count: u32,
}

impl GuestsSection {
    /// Whether `uid` is inside the guest range.
    #[must_use]
    pub fn contains(&self, uid: u32) -> bool {
        uid >= self.uid_base && uid - self.uid_base < self.uid_count
    }
}

/// `[tpm]`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TpmSection {
    /// `swtpm`.
    pub swtpm: PathBuf,
    /// `swtpm_setup`.
    pub swtpm_setup: PathBuf,
    /// `swtpm_setup.conf` naming the host's EK CA.
    pub setup_config: PathBuf,
    /// The EK CA certificate the provider publishes (ADR-0065 Decision 6).
    pub ek_ca_certificate: PathBuf,
}

/// `[registry]`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrySection {
    /// `registry/repository`, no tag or digest. The only repository this
    /// host pulls from: a reference in the cluster naming any other is
    /// refused, so whoever can write `VMImage` status still cannot point
    /// this host at an image its owner did not choose.
    pub repository: String,
    /// Directory holding `username` and `password`. Absent pulls
    /// anonymously.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_dir: Option<PathBuf>,
    /// Speak `http://`. Only for a registry on a private network or a test.
    #[serde(default)]
    pub plain_http: bool,
    /// Pulled images no `VMImage` or machine on this host uses any more
    /// (superseded by a rebuild) that are kept anyway, newest first, so a
    /// rollback does not pull again. Older ones are deleted (ADR-0064
    /// Decision 4).
    #[serde(default = "default_keep_unreferenced")]
    pub keep_unreferenced: u32,
}

/// Default for [`RegistrySection::keep_unreferenced`]: the previous build.
pub const DEFAULT_KEEP_UNREFERENCED: u32 = 1;

fn default_keep_unreferenced() -> u32 {
    DEFAULT_KEEP_UNREFERENCED
}

impl HostConfig {
    /// Parse and validate `text`.
    ///
    /// # Errors
    /// [`HostConfigError::Parse`] for malformed TOML or unknown keys,
    /// [`HostConfigError::Invalid`] for a value that parses but is refused.
    pub fn parse(text: &str) -> Result<Self, HostConfigError> {
        let config: Self = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Read, parse and validate the file at `path`.
    ///
    /// # Errors
    /// [`HostConfigError::Read`] naming the path, or any [`Self::parse`]
    /// error.
    pub fn load(path: &Path) -> Result<Self, HostConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| HostConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text)
    }

    /// Directory behind a storage class, if this host declares it.
    #[must_use]
    pub fn storage_path(&self, class: &str) -> Option<&Path> {
        self.storage_classes.get(class).map(PathBuf::as_path)
    }

    /// Bridge behind a network class, if this host declares it.
    #[must_use]
    pub fn bridge(&self, class: &str) -> Option<&str> {
        self.network_classes.get(class).map(String::as_str)
    }

    /// Storage class names, for the failure domain. Names only.
    #[must_use]
    pub fn storage_class_names(&self) -> Vec<String> {
        self.storage_classes.keys().cloned().collect()
    }

    /// Network class names, for the failure domain. Names only.
    #[must_use]
    pub fn network_class_names(&self) -> Vec<String> {
        self.network_classes.keys().cloned().collect()
    }

    fn validate(&self) -> Result<(), HostConfigError> {
        let mut paths: Vec<(&str, &Path)> = vec![
            ("provider.kubeconfig", &self.provider.kubeconfig),
            ("vmm.binary", &self.vmm.binary),
            ("vmm.firmware", &self.vmm.firmware),
            ("paths.run_root", &self.paths.run_root),
            ("paths.state_root", &self.paths.state_root),
        ];
        if let Some(t) = &self.tpm {
            paths.extend([
                ("tpm.swtpm", t.swtpm.as_path()),
                ("tpm.swtpm_setup", t.swtpm_setup.as_path()),
                ("tpm.setup_config", t.setup_config.as_path()),
                ("tpm.ek_ca_certificate", t.ek_ca_certificate.as_path()),
            ]);
        }
        if let Some(r) = &self.registry {
            if let Some(dir) = &r.credentials_dir {
                paths.push(("registry.credentials_dir", dir.as_path()));
            }
            let repo = banlieue_oci::Reference::parse(&r.repository)
                .map_err(|e| HostConfigError::Invalid(format!("registry.repository: {e}")))?;
            if repo.tag.is_some() || repo.digest.is_some() {
                return invalid("registry.repository must not carry a tag or digest");
            }
        }
        for (what, p) in paths {
            require_absolute(what, p)?;
        }

        let g = self.guests;
        if g.uid_count == 0 {
            return invalid("guests.uid_count must be at least 1");
        }
        if g.uid_base == 0 {
            return invalid("guests.uid_base must not include uid 0 (root)");
        }
        if g.uid_base.checked_add(g.uid_count - 1).is_none() {
            return invalid("guests.uid_base + uid_count overflows the uid space");
        }

        if self.storage_classes.is_empty() {
            return invalid("at least one [storage_classes] entry is required");
        }
        if self.network_classes.is_empty() {
            return invalid("at least one [network_classes] entry is required");
        }
        for (name, path) in &self.storage_classes {
            require_class_name(name)?;
            require_absolute(&format!("storage_classes.{name}"), path)?;
        }
        for (name, bridge) in &self.network_classes {
            require_class_name(name)?;
            require_interface_name(&format!("network_classes.{name}"), bridge)?;
        }
        Ok(())
    }
}

fn invalid<T>(msg: impl Into<String>) -> Result<T, HostConfigError> {
    Err(HostConfigError::Invalid(msg.into()))
}

fn require_absolute(what: &str, p: &Path) -> Result<(), HostConfigError> {
    if p.is_absolute() {
        return Ok(());
    }
    invalid(format!(
        "{what} must be an absolute path, got {}",
        p.display()
    ))
}

/// A lowercase DNS label: what a class name has to be to live in a CR.
fn require_class_name(name: &str) -> Result<(), HostConfigError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_CLASS_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if ok {
        return Ok(());
    }
    invalid(format!(
        "class name {name:?} must be a lowercase DNS label (a-z, 0-9, '-')"
    ))
}

/// A Linux interface name: 1 to 15 bytes, no '/', no whitespace, not "." or "..".
pub(crate) fn require_interface_name(what: &str, name: &str) -> Result<(), HostConfigError> {
    // One rule, the kernel's, shared with the syscalls in `sys`.
    if crate::sys::valid_ifname(name) {
        return Ok(());
    }
    invalid(format!(
        "{what} = {name:?} is not a valid interface name (1-{MAX_INTERFACE_NAME} chars, no '/' ':' or spaces)"
    ))
}

#[cfg(test)]
#[path = "host_config_tests.rs"]
mod host_config_tests;
