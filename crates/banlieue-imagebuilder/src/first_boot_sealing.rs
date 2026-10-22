// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The banlieue-generated cloud-config layer for first-boot sealing
//! (ADR-0088, as amended 2026-10-22).
//!
//! A `VMImage` with `spec.firstBootSealing` gets one more layer, merged
//! **after** every operator layer (ADR-0037 makes later layers win), that:
//!
//! - puts the Kairos sealing policy where `encrypt-pending` reads it:
//!   `kcrypt.encrypt_on_boot: true` plus `install.encrypted_partitions`.
//!   - **ISO builds** (vSphere, an installer runs): as a file written into the
//!     installed disk's `COS_OEM` from an `after-install-chroot` stage, never
//!     in the install configuration. The install hook does not read
//!     `encrypt_on_boot` (upstream #4924 is the boot step only), so
//!     `encrypted_partitions` in the install configuration would make an
//!     `Immediate` template's install, which has no vTPM, try to encrypt.
//!   - **CloudImage builds** (no installer): as top-level keys, because the
//!     image's cloud-config is itself what lands in `COS_OEM`.
//! - adds a `boot` stage that reports which listed partitions are LUKS
//!   containers, to `guestinfo.banlieue.sealed` (vSphere) and
//!   `/run/banlieue/sealed` (libvirt), for `GuestReady` (Decision 5).
//!
//! Pure: no I/O, so every decision is unit tested.

use std::fmt;

use banlieue_api::banlieue::{BuildArtifactKind, InstallMode};
use banlieue_api::common::FirstBootSealing;
use serde_json::{Value, json};

use crate::cloud_config_merge::CLOUD_CONFIG_HEADER;

/// Where the ISO layer writes the policy: the installed disk's `COS_OEM`.
/// Sorts after the image's own `90_custom.yaml`.
pub const POLICY_FILE: &str = "/oem/91_banlieue_first_boot_sealing.yaml";

/// `name` of the `after-install-chroot` step that writes [`POLICY_FILE`].
pub const POLICY_STEP_NAME: &str = "banlieue-first-boot-sealing-policy";

/// `name` of the `boot` step that publishes the sealing report.
pub const REPORT_STEP_NAME: &str = "banlieue-first-boot-sealing-report";

/// vSphere channel for the report: read back from `config.extraConfig`,
/// the same transport as ADR-0043's `guestinfo.banlieue.phase`.
pub const GUESTINFO_SEALED_KEY: &str = "guestinfo.banlieue.sealed";

/// libvirt channel for the report: read back with the guest agent, the same
/// transport as ADR-0043's `/run/banlieue/phase`.
pub const RUN_SEALED_PATH: &str = "/run/banlieue/sealed";

/// `buildArtifact.reason` when an operator layer asks the template install to
/// encrypt on an image that seals on first boot instead.
pub const REASON_FIRST_BOOT_SEALING_CONFLICT: &str = "FirstBootSealingConflict";

/// `buildArtifact.reason` when the declared partitions cannot be honoured.
pub const REASON_FIRST_BOOT_SEALING_INVALID: &str = "FirstBootSealingInvalid";

/// Partitions the running system depends on, which Kairos refuses to seal
/// on first boot and admission refuses to declare.
const SYSTEM_PARTITIONS: [&str; 4] = ["COS_OEM", "COS_STATE", "COS_RECOVERY", "COS_GRUB"];

/// The guard ADR-0043 uses: only a system booted from the installed disk,
/// never the live installer or recovery.
const INSTALLED_SYSTEM_GUARD: &str = "[ -f /run/cos/active_mode ] || [ -f /run/cos/passive_mode ]";

/// `files[].permissions` for the policy file (0644): policy, not a secret.
const POLICY_FILE_MODE: u32 = 0o644;

/// Why a sealing layer could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealingLayerError {
    /// An operator layer asks the template install itself to encrypt.
    Conflict(String),
    /// The declaration cannot be honoured (empty, system or non-label names).
    Invalid(String),
    /// The merged operator cloud-config is not valid YAML.
    Parse(String),
}

impl SealingLayerError {
    /// Stable `buildArtifact.reason` for this error.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Conflict(_) => REASON_FIRST_BOOT_SEALING_CONFLICT,
            Self::Invalid(_) | Self::Parse(_) => REASON_FIRST_BOOT_SEALING_INVALID,
        }
    }
}

impl fmt::Display for SealingLayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict(m) | Self::Invalid(m) | Self::Parse(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for SealingLayerError {}

/// A plain Kairos partition label: upper-case letters, digits, underscore.
/// The names are interpolated into a root shell script on every clone, so
/// nothing else is accepted.
fn is_plain_label(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn validate(sealing: &FirstBootSealing) -> Result<(), SealingLayerError> {
    if sealing.partitions.is_empty() {
        return Err(SealingLayerError::Invalid(
            "firstBootSealing.partitions is empty: nothing would be sealed (ADR-0088)".to_string(),
        ));
    }
    for p in &sealing.partitions {
        if !is_plain_label(p) {
            return Err(SealingLayerError::Invalid(format!(
                "firstBootSealing.partitions entry {p:?} is not a partition label ([A-Z0-9_]+)"
            )));
        }
        if SYSTEM_PARTITIONS.contains(&p.as_str()) {
            return Err(SealingLayerError::Invalid(format!(
                "firstBootSealing cannot seal {p}: the running system depends on it (ADR-0088)"
            )));
        }
    }
    Ok(())
}

/// `install.encrypted_partitions` from the merged operator cloud-config.
fn operator_encrypted_partitions(operator_merged: &str) -> Result<Vec<String>, SealingLayerError> {
    if operator_merged.trim().is_empty() {
        return Ok(Vec::new());
    }
    let doc: Value = serde_saphyr::from_str(operator_merged)
        .map_err(|e| SealingLayerError::Parse(format!("operator cloud-config is not YAML: {e}")))?;
    Ok(doc
        .get("install")
        .and_then(|i| i.get("encrypted_partitions"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

/// The POSIX shell the report step runs: which listed partitions are LUKS
/// containers now, written to both channels. An empty report is still
/// written, so the provider can tell "nothing sealed" from "not booted".
fn report_script(partitions: &[String]) -> String {
    let list = partitions.join(" ");
    format!(
        r#"sealed=""
luks="$(blkid -t TYPE=crypto_LUKS -o value -s LABEL 2>/dev/null)"
for p in {list}; do
  if printf '%s\n' "$luks" | grep -qx -e "$p" -e "${{p}}_LUKS"; then
    sealed="${{sealed:+$sealed,}}$p"
  fi
done
if command -v vmware-rpctool >/dev/null 2>&1; then
  vmware-rpctool "info-set {GUESTINFO_SEALED_KEY} $sealed" || true
fi
mkdir -p "$(dirname {RUN_SEALED_PATH})"
printf '%s\n' "$sealed" > {RUN_SEALED_PATH}
"#
    )
}

fn report_step(partitions: &[String]) -> Value {
    json!({
        "name": REPORT_STEP_NAME,
        "if": INSTALLED_SYSTEM_GUARD,
        "commands": [report_script(partitions)],
    })
}

/// The policy file's content. It carries the `#cloud-config` header because
/// Kairos's collector silently skips a headerless file, which would leave
/// `encrypt-pending` a no-op.
fn policy_yaml(partitions: &[String]) -> String {
    let policy = json!({
        "kcrypt": { "encrypt_on_boot": true },
        "install": { "encrypted_partitions": partitions },
    });
    let yaml = banlieue_api::yaml::to_string(&policy).unwrap_or_default();
    let body = yaml.strip_prefix("---\n").unwrap_or(&yaml);
    format!("{CLOUD_CONFIG_HEADER}\n{body}")
}

/// The cloud-config layer for `sealing`, to be merged after every operator
/// layer (ADR-0088 Decision 2, as amended).
///
/// # Arguments
/// * `sealing` - the image's `spec.firstBootSealing`
/// * `kind` - the artifact this build produces
/// * `install_mode` - the image's install mode (`Immediate` without a template)
/// * `operator_merged` - the merged operator cloud-config, or `""` for none
///
/// # Errors
/// [`SealingLayerError::Invalid`] for an empty list, a system partition or a
/// name that is not a plain label; [`SealingLayerError::Conflict`] when an
/// `Immediate` ISO's operator layers set `install.encrypted_partitions` (the
/// template install has no vTPM); [`SealingLayerError::Parse`] when the
/// operator cloud-config is not YAML.
pub fn sealing_layer(
    sealing: &FirstBootSealing,
    kind: &BuildArtifactKind,
    install_mode: InstallMode,
    operator_merged: &str,
) -> Result<String, SealingLayerError> {
    validate(sealing)?;
    let operator_listed = operator_encrypted_partitions(operator_merged)?;

    let layer = match kind {
        BuildArtifactKind::Iso => {
            if install_mode == InstallMode::Immediate && !operator_listed.is_empty() {
                return Err(SealingLayerError::Conflict(format!(
                    "an operator cloud-config sets install.encrypted_partitions ({}) on an \
                     Immediate image with firstBootSealing: the template installs once with no \
                     vTPM, so install-time encryption fails. Remove it; banlieue writes the \
                     policy into COS_OEM for first boot instead (ADR-0088)",
                    operator_listed.join(", ")
                )));
            }
            json!({
                "stages": {
                    "after-install-chroot": [{
                        "name": POLICY_STEP_NAME,
                        "files": [{
                            "path": POLICY_FILE,
                            "permissions": POLICY_FILE_MODE,
                            "content": policy_yaml(&sealing.partitions),
                        }],
                    }],
                    "boot": [report_step(&sealing.partitions)],
                },
            })
        }
        BuildArtifactKind::CloudImage => {
            // banlieue's merge concatenates lists, so only add what the
            // operator did not already list.
            let missing: Vec<&String> = sealing
                .partitions
                .iter()
                .filter(|p| !operator_listed.contains(p))
                .collect();
            json!({
                "kcrypt": { "encrypt_on_boot": true },
                "install": { "encrypted_partitions": missing },
                "stages": { "boot": [report_step(&sealing.partitions)] },
            })
        }
    };

    let yaml = banlieue_api::yaml::to_string(&layer).unwrap_or_default();
    Ok(yaml.strip_prefix("---\n").unwrap_or(&yaml).to_string())
}

#[cfg(test)]
#[path = "first_boot_sealing_tests.rs"]
mod first_boot_sealing_tests;
