// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shared helpers for the live and e2e suites that talk to a real libvirt
//! host: the environment they read, the TLS identity, an opened session and
//! the `LibvirtMachineSpec` every live test starts from.

// Each test binary compiles this module in full but uses only the parts it
// needs; without this, every binary warns about the rest.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use banlieue_api::common::{Firmware, IpamSpec, PowerState, ProviderReference};
use banlieue_api::infrastructure::{
    LibvirtBootSource, LibvirtBootSourceKind, LibvirtDiskBus, LibvirtDiskSpec, LibvirtMachineSpec,
    LibvirtNicSource, LibvirtNicSourceKind, LibvirtNicSpec,
};
use banlieue_libvirt::{DEFAULT_TLS_PORT, Session, TlsIdentity, connect_open, connect_tls};
use tokio::io::{AsyncRead, AsyncWrite};

/// The libvirt driver URI every suite opens.
pub const LIBVIRT_URI: &str = "qemu:///system";
/// Pool and network used when `LIBVIRT_POOL` / `LIBVIRT_NETWORK` are unset.
const DEFAULT_NAME: &str = "default";
/// Size of [`base_spec`]'s OS disk. A qcow2 volume is sparse, so this costs
/// kilobytes on the host; suites that boot a guest override `disks`.
const BASE_DISK_GIB: u32 = 1;
const BASE_VCPUS: u32 = 2;
const BASE_MEMORY_MIB: u32 = 2048;

/// `p` with a leading `~/` replaced by `$HOME/`.
pub fn expand_home(p: &str) -> String {
    match (p.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => p.to_string(),
    }
}

/// `(LIBVIRT_HOST, LIBVIRT_TLS_DIR)`, or `None` when either is unset.
pub fn libvirt_settings() -> Option<(String, PathBuf)> {
    let host = std::env::var("LIBVIRT_HOST").ok()?;
    let dir = std::env::var("LIBVIRT_TLS_DIR").ok()?;
    Some((host, PathBuf::from(expand_home(&dir))))
}

/// `(host, TLS dir, LIBVIRT_POOL, LIBVIRT_SOURCE_VOLUME)`; the pool defaults
/// to `default`. `None` when a required variable is unset.
pub fn settings() -> Option<(String, PathBuf, String, String)> {
    let (host, dir) = libvirt_settings()?;
    let pool = std::env::var("LIBVIRT_POOL").unwrap_or_else(|_| DEFAULT_NAME.to_string());
    let source = std::env::var("LIBVIRT_SOURCE_VOLUME").ok()?;
    Some((host, dir, pool, source))
}

/// `LIBVIRT_NETWORK`, defaulting to `default`.
pub fn network_name() -> String {
    std::env::var("LIBVIRT_NETWORK").unwrap_or_else(|_| DEFAULT_NAME.to_string())
}

/// `LIBVIRT_FIRMWARE=bios|efi`, default EFI. The image decides: Kairos
/// builds are EFI-only, while CirrOS's plain disk image is MBR/BIOS. Booting
/// one with the other's firmware yields a domain that runs and never reaches
/// a bootloader.
pub fn firmware_from_env() -> Firmware {
    match std::env::var("LIBVIRT_FIRMWARE").as_deref() {
        Ok("bios") => Firmware::Bios,
        _ => Firmware::Efi,
    }
}

/// The client identity in `dir`: `ca.pem`, `client-cert.pem`, `client-key.pem`.
pub fn load_identity(dir: &Path) -> TlsIdentity {
    let read = |name: &str| {
        std::fs::read(dir.join(name))
            .unwrap_or_else(|e| panic!("reading {}/{name}: {e}", dir.display()))
    };
    TlsIdentity {
        ca_pem: read("ca.pem"),
        client_cert_pem: read("client-cert.pem"),
        client_key_pem: read("client-key.pem"),
    }
}

/// Connect to `host` over mutual TLS and open [`LIBVIRT_URI`]. Panics, naming
/// the host, when either step fails: an unreachable host is a failure, never
/// a skip.
pub async fn open_session(
    host: &str,
    identity: &TlsIdentity,
) -> Session<impl AsyncRead + AsyncWrite + Unpin + Send + Sync + use<>> {
    let mut session = connect_tls(host, DEFAULT_TLS_PORT, identity)
        .await
        .unwrap_or_else(|e| panic!("TLS to libvirt on {host}: {e}"));
    connect_open(&mut session, Some(LIBVIRT_URI), false)
        .await
        .unwrap_or_else(|e| panic!("CONNECT_OPEN on {host}: {e}"));
    session
}

/// One virtio OS disk of `size_gi_b`.
pub fn os_disk(size_gi_b: u32) -> LibvirtDiskSpec {
    LibvirtDiskSpec {
        name: "os".to_string(),
        size_gi_b,
        bus: LibvirtDiskBus::Virtio,
    }
}

/// The machine every live suite starts from: powered on, one NIC on
/// [`network_name`], firmware from [`firmware_from_env`], no seed. Callers
/// set what their case is about with struct update syntax.
pub fn base_spec(domain_name: &str, pool: &str, source_volume: &str) -> LibvirtMachineSpec {
    LibvirtMachineSpec {
        provider_id: None,
        failure_domain: None,
        provider_ref: ProviderReference {
            name: "live-test".to_string(),
            namespace: None,
        },
        pool: pool.to_string(),
        domain_name: domain_name.to_string(),
        boot_source: LibvirtBootSource {
            // An overlay over the imported image: the `Immediate` shape, and
            // the one that matches a `.raw` artifact from banlieue's own
            // import pipeline (ADR-0011).
            kind: LibvirtBootSourceKind::BackingVolume,
            volume: source_volume.to_string(),
        },
        vcpus: BASE_VCPUS,
        memory_mi_b: BASE_MEMORY_MIB,
        firmware: firmware_from_env(),
        machine_type: None,
        tpm_enabled: false,
        disks: vec![os_disk(BASE_DISK_GIB)],
        network: vec![LibvirtNicSpec {
            name: "eth0".to_string(),
            source: LibvirtNicSource {
                kind: LibvirtNicSourceKind::Network,
                name: network_name(),
            },
            model: None,
            mac_address: None,
            ipam: IpamSpec::default(),
        }],
        user_data: None,
        desired_power_state: PowerState::PoweredOn,
        first_boot_sealing: None,
    }
}
