// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Where everything goes on a host.
//!
//! These are not settings (ADR-0067 Decision 6): the template units, the
//! polkit rule, the provider and the guides all name them, so moving one
//! means moving all of them.

/// Versioned VMM and firmware trees.
pub const OPT_ROOT: &str = "/opt/banlieue";
/// Where the `cloud-hypervisor`, `ch-remote` and `banlieue` commands live.
pub const BIN_DIR: &str = "/usr/local/bin";
/// banlieue's configuration.
pub const CONF_DIR: &str = "/etc/banlieue";
/// The provider's state: EK CA, EK certificates, TPM state, unit environments.
pub const STATE_ROOT: &str = "/var/lib/banlieue";
/// Per-guest sockets (tmpfs, recreated at boot by tmpfiles.d).
pub const RUN_ROOT: &str = "/run/banlieue/ch";
/// The host config the provider loads (ADR-0062 Decision 4).
pub const HOST_CONFIG: &str = "/etc/banlieue/cloud-hypervisor.toml";
/// The provider's credential directory; it renews its token here.
pub const CREDENTIALS_DIR: &str = "/etc/banlieue/credentials";
/// The provider's kubeconfig, issued by `banlieue bootstrap cloud-hypervisor-host`.
pub const KUBECONFIG_PATH: &str = "/etc/banlieue/credentials/kubeconfig";
/// Registry credentials (`username`, `password`), written by an admin.
pub const REGISTRY_CREDENTIALS_DIR: &str = "/etc/banlieue/registry";
/// The provider binary the provider unit runs.
pub const PROVIDER_BINARY: &str = "/usr/local/bin/banlieue";
/// The `cloud-hypervisor` symlink the host config and the VMM template name.
pub const VMM_BINARY: &str = "/usr/local/bin/cloud-hypervisor";
/// The `ch-remote` symlink.
pub const CH_REMOTE_BINARY: &str = "/usr/local/bin/ch-remote";
/// swtpm and swtpm_localca configuration.
pub const SWTPM_CONF_DIR: &str = "/etc/banlieue/swtpm";
/// `swtpm_setup`'s configuration, naming the host's EK CA.
pub const SWTPM_SETUP_CONF: &str = "/etc/banlieue/swtpm/swtpm_setup.conf";
/// The per-host EK CA's directory (ADR-0065 Decision 6).
pub const EK_CA_DIR: &str = "/var/lib/banlieue/swtpm-localca";
/// The EK CA certificate the provider publishes.
pub const EK_CA_CERT: &str = "/var/lib/banlieue/swtpm-localca/issuercert.pem";
/// Guest uid records for NSS (ADR-0063 Decision 3).
pub const USERDB_DIR: &str = "/etc/userdb";
/// The polkit rule (ADR-0063 Decision 6).
pub const POLKIT_RULE: &str = "/etc/polkit-1/rules.d/60-banlieue-cloud-hypervisor.rules";
/// The run-root tmpfiles.d entry.
pub const TMPFILES_CONF: &str = "/etc/tmpfiles.d/banlieue-cloud-hypervisor.conf";
/// systemd unit files.
pub const SYSTEMD_UNIT_DIR: &str = "/etc/systemd/system";

/// The unprivileged account the provider runs as (ADR-0063 Decision 6).
pub const BANLIEUE_USER: &str = "banlieue";
/// Guest records are named `<prefix><uid>`.
pub const GUEST_NAME_PREFIX: &str = "banlieue-g";
/// The slice guests and vTPMs run in (ADR-0105).
pub const GUEST_SLICE_UNIT: &str = "banlieue-guests.slice";
/// The parent of every VM slice: `banlieue-guests.slice` and the k0s
/// bootstrap's `banlieue-nodes.slice` (ADR-0106). systemd creates it from its
/// children; `overhead` reads its totals.
pub const VMS_SLICE_UNIT: &str = "banlieue.slice";
/// The provider's own unit.
pub const PROVIDER_UNIT: &str = "banlieue-provider-cloud-hypervisor.service";
/// The templates the provider starts instances of (ADR-0063, amended).
pub const UNIT_TEMPLATES: [&str; 4] = [
    "banlieue-ch@.service",
    "banlieue-swtpm@.service",
    "banlieue-swtpm-setup@.service",
    "banlieue-ch-import@.service",
];
/// What `status` lists as guests and their helpers.
pub const UNIT_GLOBS: [&str; 4] = [
    "banlieue-ch@*",
    "banlieue-swtpm@*",
    "banlieue-swtpm-setup@*",
    "banlieue-ch-import@*",
];

/// Subdirectories of the state root, and their modes: `ek` and `units` are
/// the provider's alone; `tpm` is traversable so each guest's swtpm reaches
/// its own `tpm/<uid>/` and cannot list the others.
pub const STATE_SUBDIRS: [(&str, u32); 3] = [("ek", 0o700), ("tpm", 0o711), ("units", 0o700)];
