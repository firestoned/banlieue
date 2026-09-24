// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The guest-readiness read path, against a **real running guest** (ADR-0043).
//!
//! Everything else that touches this proves something weaker:
//!
//! - `guest_tests.rs` parses replies we wrote ourselves, so it proves the
//!   interpretation is self-consistent and nothing more;
//! - `live_libvirtd.rs` proves **libvirtd** understands the qemu RPC program,
//!   but it asks a *stopped* domain, so no agent is ever involved.
//!
//! What neither shows is a real `qemu-guest-agent` opening, reading and
//! closing a file for us — the path `GuestReady` actually depends on. This
//! boots a guest and does exactly that.
//!
//! It reads `/etc/hostname` rather than banlieue's own marker, on purpose:
//! every Linux guest has one, so the test isolates *the read path* from *the
//! image having been built with the phase layer*. Those are two different
//! failures and conflating them is how "GuestReady never fires" becomes
//! unexplainable.
//!
//! The other assertion is the tri-state: with the marker genuinely absent,
//! a reachable agent must report `NotAnnounced`, not `AgentUnreachable`.
//! That distinction is what keeps the poll loop from either lagging by five
//! minutes or spinning forever, so it is worth proving against reality
//! rather than against a fake.
//!
//! ```sh
//! LIBVIRT_HOST=bar.foo.io \
//! LIBVIRT_TLS_DIR="$HOME/.config/banlieue/<host>/libvirt" \
//! LIBVIRT_POOL=k0s-bootstrap \
//! LIBVIRT_SOURCE_VOLUME=debian-13-genericcloud-amd64.qcow2 \
//! LIBVIRT_FIRMWARE=efi \
//!   cargo test -p banlieue-provider-libvirt --test live_guest -- --ignored --nocapture
//! ```
//!
//! **The image does not have to ship `qemu-guest-agent`** — this test
//! installs it at boot through the NoCloud seed (ADR-0054), which is why it
//! can run against a stock cloud image. That was the thing blocking it:
//! neither the Kairos build nor Debian's `genericcloud` ships the agent, and
//! waiting for an image that did left the read path unverified indefinitely.
//!
//! The image must therefore **run cloud-init and reach a package mirror**.
//! If the agent never answers, the failure message says whether the guest
//! booted at all, so "no network" and "never booted" stay distinguishable.
//!
//! What this still does not prove: that a real Kairos/immucore `Deferred`
//! install writes the marker *at the right moment* — that its
//! `/run/cos/active_mode` guard keeps it out of the live installer. That
//! needs an image built with the phase layer
//! (`examples/16-cloud-config-guest-phase.yaml`) and is a separate gap.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;

use banlieue_api::common::{Firmware, IpamSpec, LocalObjectReference, PowerState};
use banlieue_api::infrastructure::{
    LibvirtBootSource, LibvirtBootSourceKind, LibvirtDiskBus, LibvirtDiskSpec, LibvirtMachineSpec,
    LibvirtNicSource, LibvirtNicSourceKind, LibvirtNicSpec,
};
use banlieue_libvirt::{
    AGENT_TIMEOUT_DEFAULT, DEFAULT_TLS_PORT, InterfaceAddressSource, Session, TlsIdentity,
    connect_open, connect_tls, domain_interface_addresses, domain_qemu_agent_command,
};
use banlieue_provider_libvirt::guest::{
    GuestProbe, MARKER_PATH, MARKER_READ_MAX, PHASE_INSTALLED, guest_file_close_cmd,
    guest_file_open_cmd, guest_file_read_cmd, guest_phase_from_read, parse_file_handle,
    probe_guest,
};
use banlieue_provider_libvirt::machine_client::SessionMachineClient;
use banlieue_provider_libvirt::reconciler::libvirtmachine::{converge, finalize_backend};
use tokio::io::{AsyncRead, AsyncWrite};

/// A guest agent starts late in boot, after the network and most services.
const AGENT_TIMEOUT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Present in every Linux guest, so a failure to read it is a failure of the
/// read path rather than of the image's configuration.
const KNOWN_FILE: &str = "/etc/hostname";
/// Overlay size. Must be at least the backing image's virtual size or the
/// guest gets a truncated disk and never boots.
const DISK_GIB: u32 = 40;

/// Installs `qemu-guest-agent` on first boot.
///
/// The agent is what this whole test needs and what no available image
/// ships, so the seed supplies it rather than the image. `enable --now` is
/// belt-and-braces: the Debian package's unit is socket-activated off the
/// virtio-serial channel the domain XML always emits
/// (`render_agent_channel`), but an explicit start removes one variable from
/// a test whose failure mode is "the agent never answered".
const AGENT_SEED: &str = "#cloud-config\n\
package_update: true\n\
packages:\n\
  - qemu-guest-agent\n\
runcmd:\n\
  - [ systemctl, enable, --now, qemu-guest-agent ]\n\
  - [ mkdir, -p, /run/banlieue ]\n";

fn settings() -> Option<(String, PathBuf, String, String)> {
    let host = std::env::var("LIBVIRT_HOST").ok()?;
    let dir = std::env::var("LIBVIRT_TLS_DIR").ok()?;
    let pool = std::env::var("LIBVIRT_POOL").unwrap_or_else(|_| "default".to_string());
    let source = std::env::var("LIBVIRT_SOURCE_VOLUME").ok()?;
    let dir = match (dir.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => dir,
    };
    Some((host, PathBuf::from(dir), pool, source))
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
    let firmware = match std::env::var("LIBVIRT_FIRMWARE").as_deref() {
        Ok("bios") => Firmware::Bios,
        _ => Firmware::Efi,
    };
    LibvirtMachineSpec {
        provider_id: None,
        failure_domain: None,
        provider_ref: LocalObjectReference {
            name: "live-guest-test".to_string(),
        },
        pool: pool.to_string(),
        domain_name: domain_name.to_string(),
        boot_source: LibvirtBootSource {
            kind: LibvirtBootSourceKind::BackingVolume,
            volume: source_volume.to_string(),
        },
        vcpus: 2,
        memory_mi_b: 2048,
        firmware,
        machine_type: None,
        tpm_enabled: false,
        disks: vec![LibvirtDiskSpec {
            name: "os".to_string(),
            size_gi_b: DISK_GIB,
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
        user_data: Some(AGENT_SEED.to_string()),
        desired_power_state: PowerState::PoweredOn,
    }
}

#[tokio::test]
#[ignore = "boots a real guest; set LIBVIRT_HOST, LIBVIRT_TLS_DIR, LIBVIRT_SOURCE_VOLUME"]
async fn a_real_guest_agent_serves_the_read_path() {
    let Some((host, dir, pool, source_volume)) = settings() else {
        panic!("set LIBVIRT_HOST, LIBVIRT_TLS_DIR and LIBVIRT_SOURCE_VOLUME");
    };
    let identity = load_identity(&dir);

    // Two sessions: one drives the machine, one talks to the guest agent. A
    // session carries a single in-flight call, so sharing would interleave
    // the agent poll with whatever converge is doing.
    let mut machine_session = connect_tls(&host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS (machine)");
    connect_open(&mut machine_session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN (machine)");
    let mut client = SessionMachineClient::new(machine_session);

    let mut agent_session = connect_tls(&host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS (agent)");
    connect_open(&mut agent_session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN (agent)");

    let domain_name = format!(
        "guestcheck-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs()
    );
    let spec = spec(&domain_name, &pool, &source_volume);
    eprintln!("booting {domain_name} from {source_volume}");

    let outcome = converge(&mut client, &spec, false, false).await;
    let result = match &outcome {
        Ok(observed) => exercise(&mut agent_session, &observed.domain).await,
        Err(e) => Err(format!("converge failed: {e}")),
    };

    // Always tear down, whatever happened above.
    eprintln!("tearing down {domain_name}");
    let torn = finalize_backend(&mut client, &spec).await;

    if let Err(why) = result {
        panic!("{why}");
    }
    torn.expect("teardown");
    eprintln!("  ✓ read path verified against a real guest agent");
}

/// Whether the guest reached the network — cheap proof it booted, which is
/// what separates "no agent in the image" from "never booted".
///
/// Deliberately **not** matched on the lease hostname. Kairos announces its
/// own `kairos-<hash>` rather than the domain name (see the image table in
/// `live_cloudinit.rs`), so a hostname match reports a booted Kairos guest
/// as never-booted — which is precisely the conflation this function exists
/// to prevent. An address from any source is hostname-independent.
async fn guest_reached_the_network<S>(
    session: &mut Session<S>,
    domain: &banlieue_libvirt::Domain,
) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    for source in [InterfaceAddressSource::Lease, InterfaceAddressSource::Arp] {
        if let Ok(ifaces) = domain_interface_addresses(session, domain, source).await
            && ifaces.iter().any(|i| !i.addrs.is_empty())
        {
            return true;
        }
    }
    false
}

/// Wait for the agent, then drive the real open/read/close sequence.
async fn exercise<S>(
    session: &mut Session<S>,
    domain: &banlieue_libvirt::Domain,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // 1. Wait for qemu-guest-agent to come up.
    let deadline = Instant::now() + AGENT_TIMEOUT;
    let mut agent_up = false;
    while Instant::now() < deadline {
        match domain_qemu_agent_command(
            session,
            domain,
            r#"{"execute":"guest-ping"}"#,
            AGENT_TIMEOUT_DEFAULT,
        )
        .await
        {
            Ok(Some(_)) => {
                agent_up = true;
                break;
            }
            _ => {
                eprintln!("  … waiting for qemu-guest-agent");
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }
    if !agent_up {
        // Two very different failures look identical from here, and saying
        // the wrong one sends whoever reads this to the wrong place. A DHCP
        // lease tells them apart: it proves the guest reached userspace.
        let booted = guest_reached_the_network(session, domain).await;
        return Err(if booted {
            format!(
                "the guest booted (it has a network address) but qemu-guest-agent \
                 never answered within {}s.\n\
                 This is an IMAGE gap, not a bug in the read path: on libvirt \
                 an image without qemu-guest-agent cannot satisfy GuestReady \
                 at all (ADR-0043). Install and enable it, or use an image \
                 that ships it.",
                AGENT_TIMEOUT.as_secs()
            )
        } else {
            format!(
                "no qemu-guest-agent AND no network address within {}s — the guest \
                 probably never booted, so this says NOTHING about the read \
                 path or about the image's agent.\n\
                 Check firmware against the image (LIBVIRT_FIRMWARE=bios|efi) \
                 and that the overlay is at least the backing image's virtual \
                 size.",
                AGENT_TIMEOUT.as_secs()
            )
        });
    }
    eprintln!("  ✓ qemu-guest-agent answered guest-ping");

    // 2. The real sequence, on a file every Linux guest has.
    let open = guest_file_open_cmd(KNOWN_FILE);
    let reply = domain_qemu_agent_command(session, domain, &open, AGENT_TIMEOUT_DEFAULT)
        .await
        .map_err(|e| format!("guest-file-open failed at the libvirt level: {e}"))?
        .ok_or_else(|| "guest-file-open returned no payload".to_string())?;
    let handle = parse_file_handle(&reply)
        .ok_or_else(|| format!("could not parse a handle from {reply}"))?;
    eprintln!("  ✓ opened {KNOWN_FILE} (handle {handle})");

    let read = guest_file_read_cmd(handle, MARKER_READ_MAX);
    let read_reply = domain_qemu_agent_command(session, domain, &read, AGENT_TIMEOUT_DEFAULT)
        .await
        .map_err(|e| format!("guest-file-read failed: {e}"))?
        .ok_or_else(|| "guest-file-read returned no payload".to_string())?;

    let contents = guest_phase_from_read(&read_reply)
        .ok_or_else(|| format!("could not decode the guest's base64 payload from {read_reply}"))?;
    if contents.is_empty() {
        return Err(format!("{KNOWN_FILE} decoded to nothing: {read_reply}"));
    }
    eprintln!("  ✓ read and decoded {KNOWN_FILE} = {contents:?}");

    let close = guest_file_close_cmd(handle);
    domain_qemu_agent_command(session, domain, &close, AGENT_TIMEOUT_DEFAULT)
        .await
        .map_err(|e| format!("guest-file-close failed: {e}"))?;
    eprintln!("  ✓ closed the handle");

    // 3. The tri-state, against reality: the marker is genuinely absent on
    //    this image, and a reachable agent must say NotAnnounced rather than
    //    AgentUnreachable. Getting this wrong is what makes the poll loop
    //    either lag by five minutes or spin forever (ADR-0043 Decision 8).
    let probe = probe_guest(session, domain).await;
    if probe != GuestProbe::NotAnnounced {
        return Err(format!(
            "expected NotAnnounced for a reachable agent with no marker, got {probe:?}"
        ));
    }
    eprintln!("  ✓ probe_guest reported NotAnnounced, not AgentUnreachable");

    // 4. The other half of the tri-state, which only reality can settle:
    //    with the marker genuinely PRESENT, probe_guest must say Installed.
    //    Until now nothing had ever produced that verdict from a real agent —
    //    `guest_tests.rs` decides it from replies we wrote ourselves, so it
    //    proves our parser agrees with our own fixtures and nothing more.
    //
    //    The marker is written here rather than by the seed on purpose: if
    //    cloud-init wrote it at boot, the absent-marker assertion above could
    //    never run, and that one is the harder of the two to get right.
    write_marker(session, domain).await?;

    let probe = probe_guest(session, domain).await;
    if probe != GuestProbe::Installed {
        return Err(format!(
            "wrote {PHASE_INSTALLED:?} to {MARKER_PATH} but probe_guest returned {probe:?}"
        ));
    }
    eprintln!("  ✓ probe_guest reported Installed once the marker existed");
    Ok(())
}

/// Write the installed-marker into the guest, through the agent.
///
/// Deliberately does not reuse [`guest_file_open_cmd`], which is read-only by
/// design so a reconcile loop can never modify a guest it is inspecting
/// (ADR-0043). A test may write; the production path may not, and keeping
/// that asymmetry visible is the point.
async fn write_marker<S>(
    session: &mut Session<S>,
    domain: &banlieue_libvirt::Domain,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let open = serde_json::json!({
        "execute": "guest-file-open",
        "arguments": { "path": MARKER_PATH, "mode": "w" }
    })
    .to_string();
    let reply = domain_qemu_agent_command(session, domain, &open, AGENT_TIMEOUT_DEFAULT)
        .await
        .map_err(|e| format!("opening {MARKER_PATH} for write failed: {e}"))?
        .ok_or_else(|| format!("opening {MARKER_PATH} for write returned no payload"))?;
    let handle = parse_file_handle(&reply).ok_or_else(|| {
        format!("could not parse a handle from {reply} — does {MARKER_PATH}'s directory exist?")
    })?;

    let write = serde_json::json!({
        "execute": "guest-file-write",
        "arguments": {
            "handle": handle,
            "buf-b64": base64::engine::general_purpose::STANDARD.encode(PHASE_INSTALLED),
        }
    })
    .to_string();
    let wrote = domain_qemu_agent_command(session, domain, &write, AGENT_TIMEOUT_DEFAULT).await;

    // Close before reporting the write: a leaked handle outlives this test
    // and the next probe would find the agent's table one entry smaller.
    let close = guest_file_close_cmd(handle);
    domain_qemu_agent_command(session, domain, &close, AGENT_TIMEOUT_DEFAULT)
        .await
        .map_err(|e| format!("closing the written {MARKER_PATH} failed: {e}"))?;

    wrote.map_err(|e| format!("writing {MARKER_PATH} failed: {e}"))?;
    eprintln!("  ✓ wrote {PHASE_INSTALLED:?} to {MARKER_PATH} through the agent");
    Ok(())
}
