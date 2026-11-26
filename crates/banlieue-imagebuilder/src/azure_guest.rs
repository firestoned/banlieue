// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The banlieue-generated Azure guest layer (ADR-0171).
//!
//! A `VMImage` with any `azure` source gets two things in its build, so any
//! Kairos source image boots correctly on Azure (ADR-0163 Decision 3)
//! without the site writing a line of it:
//!
//! 1. **The helper binary**, `banlieue-azure-guest`, unpacked onto the build
//!    root filesystem from a digest-pinned OCI image through kairos-operator's
//!    `spec.artifacts.bundles` (Decision 2). Nothing is fetched at boot.
//! 2. **A cloud-config layer**, merged **last**, after the operator layers
//!    and the first-boot sealing layer (ADR-0037, ADR-0088), so no operator
//!    layer can drop it. Every step is gated on Azure's DMI chassis asset tag,
//!    so the same image is unchanged on any other backend:
//!    - `rootfs.before` (the initramfs's own network config) and `initramfs`
//!      (the real root's): a networkd file leaving the accelerated-networking
//!      VF (`mana`, `mlx4_core`, `mlx5_core`) unmanaged;
//!    - `initramfs`: mask `qemu-guest-agent.service` for this boot (Kairos's
//!      `26_vm.yaml` matches Hyper-V's DMI name and waits 90 s for a port that
//!      never appears), and install `banlieue-azure-ready.service`;
//!    - `boot`: `banlieue-azure-guest customdata`, in every mode, so a raw
//!      image's first (recovery) boot writes `/oem/91_azure_customdata.yaml`
//!      before AuroraBoot's reset, and the first active boot applies it from
//!      its first stage;
//!    - `banlieue-azure-ready.service`: `banlieue-azure-guest ready`, ordered
//!      after `cos-setup-network.service` (Kairos's last boot stage, so after
//!      every stage of customData) and only on an active or passive boot.
//!
//! Pure: no I/O, so every decision is unit tested.

use banlieue_api::banlieue::ImageSource;
use serde_json::{Value, json};

/// The provider class whose sources trigger this layer.
pub const PROVIDER_CLASS_AZURE: &str = "azure";

/// Where the helper is installed in the image, by the bundle.
pub const HELPER_PATH: &str = "/usr/bin/banlieue-azure-guest";

/// The DMI chassis asset tag every Azure VM carries.
pub const AZURE_CHASSIS_ASSET_TAG: &str = "7783-7084-3265-9085-8269-3286-77";

/// The guard on every step: the machine is an Azure VM.
pub const AZURE_GUARD: &str =
    "grep -qF 7783-7084-3265-9085-8269-3286-77 /sys/class/dmi/id/chassis_asset_tag 2>/dev/null";

/// The networkd file leaving the VF unmanaged. Sorts before Kairos's
/// catch-all DHCP file (`20-...`), so it wins for the VF.
pub const VF_NETWORK_FILE: &str = "/etc/systemd/network/10-banlieue-azure-vf-unmanaged.network";

/// Its content.
pub const VF_NETWORK_CONTENT: &str = "# banlieue (ADR-0163, ADR-0171): on Azure, the accelerated-networking VF is\n\
# enslaved to the synthetic hv_netvsc interface by the kernel. Configuring it\n\
# as well gives two interfaces the same address and can put the default route\n\
# on the VF, which cannot reach WireServer.\n\
[Match]\n\
Driver=mana mlx4_core mlx5_core\n\
\n\
[Link]\n\
Unmanaged=yes\n";

/// The ready unit's name.
pub const READY_UNIT_NAME: &str = "banlieue-azure-ready.service";

/// Where the ready unit is written (each boot: `/etc` is ephemeral on
/// Kairos).
pub const READY_UNIT_FILE: &str = "/etc/systemd/system/banlieue-azure-ready.service";

/// The `multi-user.target.wants` directory the unit is linked into.
const WANTS_DIR: &str = "/etc/systemd/system/multi-user.target.wants";

/// The unit. Ordered after `cos-setup-network.service`, the unit that runs
/// Kairos's `network.before`, `network` and `network.after` stages, so it
/// starts after every stage of the image's and customData's configuration.
/// The conditions repeat the helper's own check: never from recovery.
/// `TimeoutStartSec=infinity` because the helper has its own deadline.
pub const READY_UNIT_CONTENT: &str = "[Unit]\n\
Description=banlieue: report ready to Azure WireServer (ADR-0163, ADR-0171)\n\
After=cos-setup-network.service\n\
ConditionPathExists=|/run/cos/active_mode\n\
ConditionPathExists=|/run/cos/passive_mode\n\
ConditionPathExists=!/run/cos/recovery_mode\n\
\n\
[Service]\n\
Type=oneshot\n\
RemainAfterExit=yes\n\
ExecStart=/usr/bin/banlieue-azure-guest ready\n\
TimeoutStartSec=infinity\n\
\n\
[Install]\n\
WantedBy=multi-user.target\n";

/// `files[].permissions` of the networkd file and the unit (0644).
const PUBLIC_FILE_MODE: u32 = 0o644;

/// Step names, greppable in the boot log.
pub const STEP_VF_INITRD: &str = "banlieue-azure-vf-unmanaged-initrd";
/// The VF step for the real root.
pub const STEP_VF_ROOT: &str = "banlieue-azure-vf-unmanaged";
/// The QEMU guest agent mask.
pub const STEP_NO_QEMU_GA: &str = "banlieue-azure-no-qemu-guest-agent";
/// The ready unit install.
pub const STEP_READY_UNIT: &str = "banlieue-azure-ready-unit";
/// The customData read.
pub const STEP_CUSTOM_DATA: &str = "banlieue-azure-customdata";

/// True when any of `sources` is for the `azure` provider class, whatever
/// its kind: the build is shared, so an image also built for another class
/// carries the layer too, inert off Azure (ADR-0171 Decision 1).
#[must_use]
pub fn has_azure_source(sources: &[ImageSource]) -> bool {
    sources
        .iter()
        .any(|s| s.provider_class == PROVIDER_CLASS_AZURE)
}

/// Length of a SHA-256 digest in hex.
const SHA256_HEX_LEN: usize = 64;

/// The helper image must be pinned by digest: `<repository>@sha256:<64
/// lowercase hex>` (a tag before the `@` is allowed and ignored by the
/// pull). Anything else is refused, so what lands in every Azure image is
/// exactly what was reviewed (ADR-0171 Decision 2).
///
/// # Errors
/// A message naming what is wrong.
pub fn validate_helper_image(reference: &str) -> Result<(), String> {
    let Some((repo, digest)) = reference.split_once('@') else {
        return Err(format!(
            "{reference:?} is not pinned by digest (<repository>@sha256:<hex>)"
        ));
    };
    if repo.is_empty() || repo.contains(char::is_whitespace) {
        return Err(format!("{reference:?} has no repository"));
    }
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(format!("{reference:?}: the digest is not sha256"));
    };
    if hex.len() != SHA256_HEX_LEN || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(format!(
            "{reference:?}: the digest is not 64 lowercase hex characters"
        ));
    }
    Ok(())
}

fn vf_step(name: &str) -> Value {
    json!({
        "name": name,
        "if": AZURE_GUARD,
        "files": [{
            "path": VF_NETWORK_FILE,
            "permissions": PUBLIC_FILE_MODE,
            "owner": 0,
            "group": 0,
            "content": VF_NETWORK_CONTENT,
        }],
        "commands": ["networkctl reload 2>/dev/null || true"],
    })
}

/// The cloud-config layer, to be merged after every other layer.
#[must_use]
pub fn azure_guest_layer() -> String {
    let layer = json!({
        "stages": {
            "rootfs.before": [vf_step(STEP_VF_INITRD)],
            "initramfs": [
                vf_step(STEP_VF_ROOT),
                {
                    "name": STEP_NO_QEMU_GA,
                    "if": AZURE_GUARD,
                    "commands": ["systemctl mask --runtime qemu-guest-agent.service 2>/dev/null || true"],
                },
                {
                    "name": STEP_READY_UNIT,
                    "if": AZURE_GUARD,
                    "files": [{
                        "path": READY_UNIT_FILE,
                        "permissions": PUBLIC_FILE_MODE,
                        "owner": 0,
                        "group": 0,
                        "content": READY_UNIT_CONTENT,
                    }],
                    "commands": [format!(
                        "mkdir -p {WANTS_DIR} && ln -sf {READY_UNIT_FILE} {WANTS_DIR}/{READY_UNIT_NAME}"
                    )],
                },
            ],
            "boot": [{
                "name": STEP_CUSTOM_DATA,
                "if": AZURE_GUARD,
                "commands": [format!("{HELPER_PATH} customdata")],
            }],
        },
    });
    let yaml = banlieue_api::yaml::to_string(&layer).unwrap_or_default();
    yaml.strip_prefix("---\n").unwrap_or(&yaml).to_string()
}

#[cfg(test)]
#[path = "azure_guest_tests.rs"]
mod azure_guest_tests;
