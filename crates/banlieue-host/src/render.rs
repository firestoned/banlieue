// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The files `install` writes (ADR-0067 Decision 5).
//!
//! The systemd, tmpfiles and polkit files are the ones in
//! `deploy/provider-cloud-hypervisor/host/`, compiled in: they stay in the
//! repository as files, reviewed and documented as files, and a placeholder
//! no value fills is an error, never a unit shipped with `@NAME@` in it. The
//! host config is serialized from the provider's own `HostConfig` and parsed
//! back before it is written, so the installer cannot write a file the
//! provider would refuse.

use crate::error::Error;
use crate::ops::Probe;
use crate::paths::{
    BANLIEUE_USER, CREDENTIALS_DIR, EK_CA_CERT, EK_CA_DIR, HOST_CONFIG, KUBECONFIG_PATH,
    PROVIDER_BINARY, REGISTRY_CREDENTIALS_DIR, RUN_ROOT, STATE_ROOT, SWTPM_CONF_DIR,
    SWTPM_SETUP_CONF, VMM_BINARY,
};
use crate::pins::Release;
use crate::settings::Settings;
use banlieue_provider_cloud_hypervisor::host_config::{
    GuestsSection, HostConfig, PathsSection, ProviderSection, RegistrySection, TpmSection,
    VmmSection,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A template compiled into the binary: its file name and text.
pub struct Template {
    /// File name, as installed.
    pub name: &'static str,
    /// Text, with `@NAME@` placeholders.
    pub text: &'static str,
}

macro_rules! template {
    ($name:literal) => {
        Template {
            name: $name,
            text: include_str!(concat!(
                "../../../deploy/provider-cloud-hypervisor/host/",
                $name
            )),
        }
    };
}

/// The four unit templates the provider starts instances of.
pub const UNIT_TEMPLATES: [Template; 4] = [
    template!("banlieue-ch@.service"),
    template!("banlieue-swtpm@.service"),
    template!("banlieue-swtpm-setup@.service"),
    template!("banlieue-ch-import@.service"),
];
/// The provider's own unit.
pub const PROVIDER_UNIT: Template = template!("banlieue-provider-cloud-hypervisor.service");
/// The run root, recreated at every boot.
pub const TMPFILES: Template = template!("banlieue-cloud-hypervisor.tmpfiles.conf");
/// The polkit rule.
pub const POLKIT_RULE: Template = template!("60-banlieue-cloud-hypervisor.rules");

/// Where a command is on this host, or where Debian puts it.
fn command_path(probe: &dyn Probe, command: &str) -> PathBuf {
    probe
        .which(command)
        .unwrap_or_else(|| Path::new("/usr/bin").join(command))
}

fn space_joined<'a>(items: impl Iterator<Item = &'a Path>) -> String {
    items
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every placeholder's value.
#[must_use]
pub fn values(s: &Settings, probe: &dyn Probe) -> BTreeMap<&'static str, String> {
    let dirs: Vec<&Path> = s.storage.iter().map(|(_, p)| p.as_path()).collect();
    let image_dirs: Vec<PathBuf> = dirs
        .iter()
        .map(|d| d.join(banlieue_provider_cloud_hypervisor::plan::IMAGES_DIR))
        .collect();
    let state = Path::new(STATE_ROOT);
    let mut rw = vec![PathBuf::from(RUN_ROOT)];
    rw.extend(dirs.iter().map(|d| d.to_path_buf()));
    rw.extend(["ek", "tpm", "units"].iter().map(|d| state.join(d)));
    let run_parent = Path::new(RUN_ROOT)
        .parent()
        .map_or_else(String::new, |p| p.display().to_string());
    BTreeMap::from([
        ("BANLIEUE_USER", BANLIEUE_USER.to_string()),
        ("PROVIDER_BINARY", PROVIDER_BINARY.to_string()),
        ("KUBECONFIG_PATH", KUBECONFIG_PATH.to_string()),
        ("CREDENTIALS_DIR", CREDENTIALS_DIR.to_string()),
        ("HOST_CONFIG", HOST_CONFIG.to_string()),
        ("RUN_ROOT", RUN_ROOT.to_string()),
        ("RUN_PARENT", run_parent),
        (
            "PROVIDER_RW_PATHS",
            space_joined(rw.iter().map(PathBuf::as_path)),
        ),
        ("STATE_ROOT", STATE_ROOT.to_string()),
        ("VMM_BINARY", VMM_BINARY.to_string()),
        ("STORAGE_DIRS", space_joined(dirs.iter().copied())),
        (
            "STORAGE_IMAGE_DIRS",
            space_joined(image_dirs.iter().map(PathBuf::as_path)),
        ),
        ("SWTPM", command_path(probe, "swtpm").display().to_string()),
        (
            "SWTPM_SETUP",
            command_path(probe, "swtpm_setup").display().to_string(),
        ),
        ("SWTPM_SETUP_CONF", SWTPM_SETUP_CONF.to_string()),
        ("EK_CA_DIR", EK_CA_DIR.to_string()),
        ("GUEST_UID_BASE", s.uid_base.to_string()),
        ("GUEST_UID_COUNT", s.uid_count.to_string()),
    ])
}

/// `template` with every placeholder filled.
///
/// # Errors
/// [`Error::Template`] naming a placeholder no value fills.
pub fn render(
    template: &Template,
    values: &BTreeMap<&'static str, String>,
) -> Result<String, Error> {
    let mut out = template.text.to_string();
    for (k, v) in values {
        out = out.replace(&format!("@{k}@"), v);
    }
    if let Some(placeholder) = leftover_placeholder(&out) {
        return Err(Error::Template {
            template: template.name,
            placeholder,
        });
    }
    Ok(out)
}

/// The first `@NAME@` left in `text` (uppercase, digits, `_`). Unit names
/// such as `banlieue-ch@.service` also contain `@`, so every one is
/// checked, not only the first.
fn leftover_placeholder(text: &str) -> Option<String> {
    text.match_indices('@').find_map(|(start, _)| {
        let rest = &text[start + 1..];
        let end = rest.find('@')?;
        let name = &rest[..end];
        let is_placeholder = name.starts_with(|c: char| c.is_ascii_uppercase())
            && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        is_placeholder.then(|| format!("@{name}@"))
    })
}

const HOST_CONFIG_HEADER: &str = "\
# banlieue Cloud Hypervisor host configuration.
# Written by `banlieue host cloud-hypervisor install` (ADR-0067).
#
# Host-local BY DESIGN (ADR-0062 Decision 4): paths and bridges are resolved
# here and never taken from the cluster, so a stolen cluster credential can
# choose among what this file declares and nothing else. Machines name a
# class; the provider publishes the class names on its failure domain.
#
# Schema: crates/banlieue-provider-cloud-hypervisor/src/host_config.rs
# (unknown keys are refused, so a typo fails the provider at startup).

";

/// The `[vmm]` section naming `release`.
fn vmm_section(release: &Release) -> VmmSection {
    VmmSection {
        binary: VMM_BINARY.into(),
        version: release.version.clone(),
        firmware: release.firmware.clone(),
    }
}

/// `config` as the file the provider loads: the header, then the TOML,
/// parsed back before it is returned.
fn serialize(config: &HostConfig) -> Result<String, Error> {
    let body = toml::to_string(config).map_err(|e| Error::Config(e.to_string()))?;
    let text = format!("{HOST_CONFIG_HEADER}{body}");
    let parsed = HostConfig::parse(&text).map_err(|e| Error::Config(e.to_string()))?;
    if &parsed != config {
        return Err(Error::Config("it does not read back as written".into()));
    }
    Ok(text)
}

/// An existing host config with its `[vmm]` section naming `release`, or
/// `None` when it already does (ADR-0084 Decision 5). Every other key
/// keeps its value; comments an admin added are not kept.
///
/// # Errors
/// [`Error::Config`] if `existing` does not parse as the provider would.
pub fn with_vmm(existing: &str, release: &Release) -> Result<Option<String>, Error> {
    let mut config = HostConfig::parse(existing).map_err(|e| Error::Config(e.to_string()))?;
    let vmm = vmm_section(release);
    if config.vmm == vmm {
        return Ok(None);
    }
    config.vmm = vmm;
    serialize(&config).map(Some)
}

/// The host config for `s` and `release`, as the provider will load it.
///
/// # Errors
/// [`Error::Config`] if it does not survive the provider's own parser.
pub fn host_config(s: &Settings, probe: &dyn Probe, release: &Release) -> Result<String, Error> {
    let config = HostConfig {
        provider: ProviderSection {
            name: s.provider_name.clone(),
            namespace: s.namespace.clone(),
            kubeconfig: KUBECONFIG_PATH.into(),
        },
        vmm: vmm_section(release),
        paths: PathsSection {
            run_root: RUN_ROOT.into(),
            state_root: STATE_ROOT.into(),
        },
        guests: GuestsSection {
            uid_base: s.uid_base,
            uid_count: s.uid_count,
        },
        tpm: Some(TpmSection {
            swtpm: command_path(probe, "swtpm"),
            swtpm_setup: command_path(probe, "swtpm_setup"),
            setup_config: SWTPM_SETUP_CONF.into(),
            ek_ca_certificate: EK_CA_CERT.into(),
        }),
        registry: s.registry.as_ref().map(|r| RegistrySection {
            repository: r.repository.clone(),
            credentials_dir: Some(REGISTRY_CREDENTIALS_DIR.into()),
            plain_http: r.plain_http,
            keep_unreferenced: r.keep_unreferenced,
        }),
        storage_classes: s.storage.iter().cloned().collect(),
        network_classes: s.network.iter().cloned().collect(),
    };
    serialize(&config)
}

/// The swtpm and swtpm_localca configuration: `(path, text)`.
#[must_use]
pub fn swtpm_config(probe: &dyn Probe) -> Vec<(PathBuf, String)> {
    let dir = Path::new(SWTPM_CONF_DIR);
    let ca = Path::new(EK_CA_DIR);
    let localca_conf = dir.join("swtpm-localca.conf");
    let localca_options = dir.join("swtpm-localca.options");
    vec![
        (
            localca_conf.clone(),
            format!(
                "statedir = {ca}\nsigningkey = {key}\nissuercert = {cert}\ncertserial = {serial}\n",
                ca = ca.display(),
                key = ca.join("signkey.pem").display(),
                cert = ca.join("issuercert.pem").display(),
                serial = ca.join("certserial").display(),
            ),
        ),
        (
            localca_options.clone(),
            "--platform-manufacturer banlieue\n--platform-version 2.1\n--platform-model cloud-hypervisor\n"
                .to_string(),
        ),
        (
            PathBuf::from(SWTPM_SETUP_CONF),
            format!(
                "create_certs_tool = {tool}\ncreate_certs_tool_config = {conf}\ncreate_certs_tool_options = {opts}\nactive_pcr_banks = sha256\n",
                tool = command_path(probe, "swtpm_localca").display(),
                conf = localca_conf.display(),
                opts = localca_options.display(),
            ),
        ),
    ]
}

/// A guest's userdb records: `(user JSON, group JSON)`. One private group
/// per guest, gid equal to uid (ADR-0063 Decision 3).
#[must_use]
pub fn guest_records(name: &str, uid: u32) -> (String, String) {
    let user = serde_json::json!({
        "userName": name,
        "uid": uid,
        "gid": uid,
        "realName": "banlieue guest",
        "homeDirectory": "/",
        "shell": "/usr/sbin/nologin",
        "locked": true,
    });
    let group = serde_json::json!({ "groupName": name, "gid": uid });
    (format!("{user}\n"), format!("{group}\n"))
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod render_tests;
