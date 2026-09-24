// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The vTPM endorsement-certificate read path, against a **real guest with a
//! real swtpm** (ADR-0045).
//!
//! Everything offline proves something weaker. `guest_tests.rs` parses a real
//! `swtpm_localca` certificate, but one this repository pasted in, against
//! replies it wrote itself — so it proves the interpretation is
//! self-consistent, not that a live vTPM produces something matching it.
//! `libvirtmachine_tests.rs` drives the gating through `FakeMachineClient`,
//! which proves the ordering and nothing about swtpm.
//!
//! What only this can show is the claim ADR-0045 actually rests on: that
//! libvirt, given the domain XML banlieue emits, starts an swtpm whose
//! certificate carries subject CN `<domain-name>:<domain-uuid>` — both
//! values banlieue itself assigned. If libvirt ever changes what it passes
//! as `--vmid`, every offline test here still passes and this one fails,
//! which is the entire point of it existing.
//!
//! ```sh
//! LIBVIRT_HOST=bar.foo.io \
//! LIBVIRT_TLS_DIR="$HOME/.config/banlieue/<host>/libvirt" \
//! LIBVIRT_POOL=k0s-bootstrap \
//! LIBVIRT_SOURCE_VOLUME=debian-13-genericcloud-amd64.qcow2 \
//!   cargo test -p banlieue-provider-libvirt --test live_ek -- --ignored --nocapture
//! ```
//!
//! **The host must have swtpm configured** (`swtpm`, `swtpm-tools`; see
//! `scripts/bootstrap-libvirt-host.sh`). Without it the domain will not
//! start at all, and the failure says so rather than blaming the read path.
//!
//! **The image does not have to ship anything** — the NoCloud seed installs
//! `tpm2-tools` and exports the certificate, which is exactly the guest-side
//! contract ADR-0045 Decision 2 defines. A production image carries the same
//! two lines in its cloud-config layer rather than in a test seed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use banlieue_api::common::{Firmware, IpamSpec, LocalObjectReference, PowerState};
use banlieue_api::infrastructure::{
    LibvirtBootSource, LibvirtBootSourceKind, LibvirtDiskBus, LibvirtDiskSpec, LibvirtMachineSpec,
    LibvirtNicSource, LibvirtNicSourceKind, LibvirtNicSpec,
};
use banlieue_libvirt::{
    AGENT_TIMEOUT_DEFAULT, DEFAULT_TLS_PORT, Session, TlsIdentity, connect_open, connect_tls,
    domain_qemu_agent_command,
};
use banlieue_provider_libvirt::guest::{EK_PATH, EkProbe, expected_ek_cn, read_ek_certificate};
use banlieue_provider_libvirt::machine_client::SessionMachineClient;
use banlieue_provider_libvirt::reconciler::libvirtmachine::{converge, finalize_backend};
use tokio::io::{AsyncRead, AsyncWrite};

/// The agent starts late in boot; the package install and the NV read land
/// later still.
const READY_TIMEOUT: Duration = Duration::from_secs(420);
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Overlay size. Must be at least the backing image's virtual size.
const DISK_GIB: u32 = 40;
/// NV index holding the RSA EK certificate, per the TCG EK Credential
/// Profile. This is the index swtpm's `--create-ek-cert` populates.
const EK_NV_INDEX: &str = "0x01c00002";

/// The guest-side half of ADR-0045 Decision 2.
///
/// Exports the certificate from the vTPM's NVRAM to [`EK_PATH`] so banlieue
/// can read it with the ADR-0043 read-only path. A production image carries
/// these same steps in its cloud-config layer.
fn seed() -> String {
    format!(
        "#cloud-config\n\
package_update: true\n\
packages:\n\
  - qemu-guest-agent\n\
  - tpm2-tools\n\
runcmd:\n\
  - [ systemctl, enable, --now, qemu-guest-agent ]\n\
  - [ mkdir, -p, /run/banlieue ]\n\
  - [ sh, -c, \"tpm2_nvread {EK_NV_INDEX} -o /run/banlieue/ek.der\" ]\n\
  - [ sh, -c, \"openssl x509 -inform DER -in /run/banlieue/ek.der -out {EK_PATH}\" ]\n"
    )
}

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
            name: "live-ek-test".to_string(),
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
        // The point of this test.
        tpm_enabled: true,
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
        user_data: Some(seed()),
        desired_power_state: PowerState::PoweredOn,
    }
}

#[tokio::test]
#[ignore = "boots a real guest with a vTPM; needs LIBVIRT_HOST, LIBVIRT_TLS_DIR, LIBVIRT_SOURCE_VOLUME and swtpm on the host"]
async fn a_real_swtpm_certificate_is_read_and_bound_to_its_domain() {
    let Some((host, dir, pool, source_volume)) = settings() else {
        panic!("set LIBVIRT_HOST, LIBVIRT_TLS_DIR and LIBVIRT_SOURCE_VOLUME");
    };
    let identity = load_identity(&dir);

    // Two sessions: a session carries one in-flight call, so sharing would
    // interleave the agent poll with whatever converge is doing.
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
        "ekcheck-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs()
    );
    let spec = spec(&domain_name, &pool, &source_volume);
    eprintln!("booting {domain_name} (vTPM enabled) from {source_volume}");

    let outcome = converge(&mut client, &spec, false, false).await;
    let result = match &outcome {
        Ok(observed) => exercise(&mut agent_session, &observed.domain, &spec.domain_name).await,
        // A host without swtpm fails here, at domain start — say so, rather
        // than letting it read as a broken read path.
        Err(e) => Err(format!(
            "converge failed: {e}\n\
             If this is a domain-start failure, check that the host has swtpm \
             and swtpm-tools installed (scripts/bootstrap-libvirt-host.sh) — a \
             vTPM-enabled domain cannot start without them."
        )),
    };

    eprintln!("tearing down {domain_name}");
    let torn = finalize_backend(&mut client, &spec).await;

    if let Err(why) = result {
        panic!("{why}");
    }
    torn.expect("teardown");
    eprintln!("  ✓ EK certificate read from a real vTPM and bound to its domain");
}

/// Wait for the guest to export its certificate, then read it the way the
/// reconciler does.
async fn exercise<S>(
    session: &mut Session<S>,
    domain: &banlieue_libvirt::Domain,
    domain_name: &str,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let uuid = format_uuid(&domain.uuid);
    let expected = expected_ek_cn(domain_name, &uuid);
    eprintln!("  expecting subject CN {expected}");

    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last = EkProbe::NotPublished;
    while Instant::now() < deadline {
        last = read_ek_certificate(session, domain, domain_name, &uuid).await;
        match &last {
            EkProbe::Published(pem) => {
                // The assertion that matters: a certificate a real swtpm
                // issued, whose CN is what banlieue named the domain.
                assert!(
                    pem.contains("BEGIN CERTIFICATE"),
                    "published value must be PEM (missing header, len={})",
                    pem.len()
                );
                eprintln!("  ✓ certificate published, CN matches {expected}");
                return Ok(());
            }
            // Fail fast: a mismatch will not fix itself by waiting, and it
            // is the one outcome that means libvirt's --vmid changed shape.
            EkProbe::Mismatch => {
                return Err(format!(
                    "the guest exported a certificate whose subject CN is NOT {expected}.\n\
                     libvirt passes `--vmid <domain-name>:<domain-uuid>` to swtpm_setup and \
                     swtpm_localca puts it in the CN (ADR-0045 Decision 3). If libvirt changed \
                     what it passes, ADR-0045's binding check needs updating — this test is \
                     what exists to catch that."
                ));
            }
            EkProbe::NotPublished => {
                eprintln!("  … waiting for the guest to export {EK_PATH}");
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }

    // Distinguish "no agent" from "agent up, no certificate": they send
    // whoever reads this to completely different places.
    let agent_up = domain_qemu_agent_command(
        session,
        domain,
        r#"{"execute":"guest-ping"}"#,
        AGENT_TIMEOUT_DEFAULT,
    )
    .await
    .is_ok_and(|r| r.is_some());

    Err(if agent_up {
        format!(
            "qemu-guest-agent is answering but {EK_PATH} never appeared within {}s \
             (last probe: {last:?}).\n\
             This is a GUEST gap, not a bug in the read path: the image must export \
             its EK certificate from NV {EK_NV_INDEX} (ADR-0045 Decision 2). Check \
             that tpm2-tools installed and that the domain really has a vTPM.",
            READY_TIMEOUT.as_secs()
        )
    } else {
        format!(
            "qemu-guest-agent never answered within {}s, so this says NOTHING about \
             the EK read path.\n\
             Check firmware against the image (LIBVIRT_FIRMWARE=bios|efi), that the \
             overlay is at least the backing image's virtual size, and that the guest \
             can reach a package mirror.",
            READY_TIMEOUT.as_secs()
        )
    })
}

/// libvirt's canonical hyphenated UUID form — the same rendering
/// `LibvirtMachineStatus.domainUuid` carries, and the half of the expected
/// subject CN that comes from libvirt rather than from the spec.
fn format_uuid(raw: &[u8; 16]) -> String {
    let h = |r: &[u8]| r.iter().map(|b| format!("{b:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        h(&raw[0..4]),
        h(&raw[4..6]),
        h(&raw[6..8]),
        h(&raw[8..10]),
        h(&raw[10..16])
    )
}
