// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Proof that a **guest consumes** the cloud-init seed (ADR-0054).
//!
//! Everything else that tests the seed proves something weaker:
//!
//! - the unit tests read our own bytes back, so they prove the image is
//!   self-consistent and nothing more;
//! - mounting it proves an independent ISO9660 implementation agrees on the
//!   format;
//! - `live_machine.rs` proves libvirtd accepts the volume as a CD-ROM.
//!
//! None of those show a guest *reading* it. This does, and it needs no shell
//! access and no guest agent: the seed's `meta-data` sets `local-hostname`,
//! the guest announces that name in its DHCP request, and libvirt records it
//! in the lease. A lease bearing the domain's name can only have come from
//! the guest having parsed the seed.
//!
//! ```sh
//! LIBVIRT_HOST=bar.foo.io \
//! LIBVIRT_TLS_DIR="$HOME/.config/banlieue/libvirt" \
//! LIBVIRT_POOL=images LIBVIRT_SOURCE_VOLUME=cirros-0.6.2.qcow2 \
//!   cargo test -p banlieue-provider-libvirt --test live_cloudinit -- --ignored --nocapture
//! ```
//!
//! # Choosing a guest, which is most of the difficulty
//!
//! The image must run **cloud-init** *and* its DHCP client must send the
//! hostname. Verified against a real host:
//!
//! | Image | Boots | Announces hostname | Verdict |
//! |---|---|---|---|
//! | Alpine `nocloud_*-bios-cloudinit` (raw) | yes | **yes** | use this |
//! | CirrOS 0.6.2 (raw, BIOS) | yes | no — busybox udhcpc sends none | useless as a signal |
//! | Kairos (raw, EFI) | yes | its own `kairos-<hash>` | consumes yip config, not cloud-init |
//!
//! Two traps, both of which look exactly like "the guest ignored the seed":
//!
//! - **Wrong firmware.** Kairos builds are EFI-only; CirrOS and the Alpine
//!   `-bios-` images are MBR. The domain runs and never reaches a
//!   bootloader. Set `LIBVIRT_FIRMWARE=bios|efi` to match the image.
//! - **Overlay smaller than the backing image's virtual size.** The guest
//!   gets a truncated disk and never boots.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use banlieue_api::common::{Firmware, IpamSpec, LocalObjectReference, PowerState};
use banlieue_api::infrastructure::{
    LibvirtBootSource, LibvirtBootSourceKind, LibvirtDiskBus, LibvirtDiskSpec, LibvirtMachineSpec,
    LibvirtNicSource, LibvirtNicSourceKind, LibvirtNicSpec,
};
use banlieue_libvirt::{
    DEFAULT_TLS_PORT, TlsIdentity, connect_open, connect_tls, list_all_networks,
    network_get_dhcp_leases,
};
use banlieue_provider_libvirt::machine_client::SessionMachineClient;
use banlieue_provider_libvirt::reconciler::libvirtmachine::{converge, finalize_backend};

/// How long to wait for the guest to boot and DHCP. CirrOS is usually up in
/// well under a minute; the rest is headroom for a loaded host.
const BOOT_TIMEOUT: Duration = Duration::from_secs(240);
/// Gap between lease polls.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

fn settings() -> Option<(String, PathBuf, String, String)> {
    Some((
        std::env::var("LIBVIRT_HOST").ok()?,
        PathBuf::from(expand_home(&std::env::var("LIBVIRT_TLS_DIR").ok()?)),
        std::env::var("LIBVIRT_POOL").unwrap_or_else(|_| "default".to_string()),
        std::env::var("LIBVIRT_SOURCE_VOLUME").ok()?,
    ))
}

fn expand_home(p: &str) -> String {
    match (p.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => p.to_string(),
    }
}

fn load_identity(dir: &Path) -> TlsIdentity {
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

fn spec(domain_name: &str, pool: &str, source_volume: &str) -> LibvirtMachineSpec {
    LibvirtMachineSpec {
        provider_id: None,
        failure_domain: None,
        provider_ref: LocalObjectReference {
            name: "live-test".to_string(),
        },
        pool: pool.to_string(),
        domain_name: domain_name.to_string(),
        boot_source: LibvirtBootSource {
            kind: LibvirtBootSourceKind::BackingVolume,
            volume: source_volume.to_string(),
        },
        vcpus: 2,
        memory_mi_b: 2048,
        // `LIBVIRT_FIRMWARE=bios|efi`, default EFI. The image decides:
        // Kairos builds are EFI-only, while CirrOS's plain disk image is
        // MBR/BIOS. Booting one with the other's firmware yields a domain
        // that runs and never reaches a bootloader — which looks exactly
        // like "the guest ignored the seed".
        firmware: match std::env::var("LIBVIRT_FIRMWARE").as_deref() {
            Ok("bios") => Firmware::Bios,
            _ => Firmware::Efi,
        },
        machine_type: None,
        tpm_enabled: false,
        disks: vec![LibvirtDiskSpec {
            name: "os".to_string(),
            // `LIBVIRT_DISK_GIB`, default 20. Must be at least the backing
            // image's *virtual* size: a qcow2 overlay smaller than what it
            // overlays gives the guest a truncated disk, so it never
            // reaches a bootloader.
            size_gi_b: std::env::var("LIBVIRT_DISK_GIB")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(20),
            bus: LibvirtDiskBus::Virtio,
        }],
        network: vec![LibvirtNicSpec {
            name: "eth0".to_string(),
            source: LibvirtNicSource {
                kind: LibvirtNicSourceKind::Network,
                name: std::env::var("LIBVIRT_NETWORK").unwrap_or_else(|_| "default".to_string()),
            },
            model: None,
            mac_address: None,
            ipam: IpamSpec::default(),
        }],
        // A marker the guest can only have got from the seed.
        user_data: Some("#cloud-config\n".to_string()),
        desired_power_state: PowerState::PoweredOn,
    }
}

#[tokio::test]
#[ignore = "boots a real guest; set LIBVIRT_HOST, LIBVIRT_TLS_DIR, LIBVIRT_SOURCE_VOLUME"]
async fn a_guest_consumes_the_cloud_init_seed() {
    let Some((host, dir, pool, source_volume)) = settings() else {
        panic!("set LIBVIRT_HOST, LIBVIRT_TLS_DIR and LIBVIRT_SOURCE_VOLUME");
    };
    let identity = load_identity(&dir);

    // Two sessions: one drives the machine, one polls leases. A session
    // carries a single in-flight call, so sharing would mean interleaving
    // the poll with whatever converge is doing.
    let mut machine_session = connect_tls(&host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS (machine)");
    connect_open(&mut machine_session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN (machine)");
    let mut client = SessionMachineClient::new(machine_session);

    let mut lease_session = connect_tls(&host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS (leases)");
    connect_open(&mut lease_session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN (leases)");
    let net_name = std::env::var("LIBVIRT_NETWORK").unwrap_or_else(|_| "default".to_string());
    let network = list_all_networks(&mut lease_session)
        .await
        .expect("list networks")
        .into_iter()
        .find(|n| n.name == net_name)
        .unwrap_or_else(|| panic!("network {net_name:?} not found on the host"));

    // The domain name is what `meta-data` puts in `local-hostname`, so it is
    // also what the guest should announce over DHCP.
    let domain_name = format!(
        "seedcheck-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs()
    );
    let spec = spec(&domain_name, &pool, &source_volume);
    eprintln!("booting {domain_name} from {source_volume}");

    let outcome = converge(&mut client, &spec).await;

    // Poll for a lease announcing our hostname. Everything is inside this
    // block so the teardown below always runs.
    let mut seen: Option<String> = None;
    if outcome.is_ok() {
        let deadline = Instant::now() + BOOT_TIMEOUT;
        while Instant::now() < deadline && seen.is_none() {
            tokio::time::sleep(POLL_INTERVAL).await;
            let leases = network_get_dhcp_leases(&mut lease_session, &network, None)
                .await
                .unwrap_or_default();
            for l in &leases {
                if l.hostname.as_deref() == Some(domain_name.as_str()) {
                    eprintln!(
                        "  guest announced {:?} at {} — it read the seed",
                        l.hostname.as_deref().unwrap_or(""),
                        l.ipaddr
                    );
                    seen = Some(l.ipaddr.clone());
                }
            }
            if seen.is_none() {
                // Domain state and address discovery, so a failure says
                // *where* it failed: a domain that shut off never reached a
                // bootloader, one that is Running with no address booted but
                // did not network, and one with an address but no matching
                // lease hostname read no seed.
                use banlieue_provider_libvirt::machine_client::LibvirtMachineClient;
                let state = match client.lookup_domain(&domain_name).await {
                    Ok(Some(d)) => match client.domain_state(&d).await {
                        Ok(st) => format!("{st:?}"),
                        Err(e) => format!("state-error: {e}"),
                    },
                    Ok(None) => "gone".to_string(),
                    Err(e) => format!("lookup-error: {e}"),
                };
                let addrs = match client.domain_addresses_any(&domain_name).await {
                    Some(a) => a,
                    None => "none".to_string(),
                };
                let names: Vec<&str> = leases
                    .iter()
                    .filter_map(|l| l.hostname.as_deref())
                    .collect();
                eprintln!("  waiting… state={state} addrs={addrs} leases={names:?}");
            }
        }
    }

    eprintln!("  tearing down");
    let teardown = finalize_backend(&mut client, &spec).await;

    // Assert last, so nothing above can leave a domain behind.
    outcome.expect("converge failed");
    teardown.expect("teardown failed");
    let addr = seen.unwrap_or_else(|| {
        panic!(
            "no DHCP lease announced the hostname {domain_name:?} within {BOOT_TIMEOUT:?}.\n\
             That name reaches the guest only through the seed's meta-data, so either the \
             guest did not read it, or the guest is not cloud-init-capable (a Kairos image \
             will fail here for that reason, not because the seed is wrong)."
        )
    });
    eprintln!("cloud-init consumed the seed; guest reachable at {addr}");
}
