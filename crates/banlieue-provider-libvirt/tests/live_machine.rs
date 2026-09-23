// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The `LibvirtMachine` reconciler's convergence and teardown, against a
//! **real libvirtd** (ADR-0050).
//!
//! `libvirtmachine_tests.rs` drives the same two functions through
//! `FakeMachineClient`, which proves the ordering and the branching but not
//! that libvirtd accepts what we produce. Only this test can do that: the
//! domain XML in particular is a document a real daemon either parses or
//! rejects, and a self-consistent misunderstanding of it passes every offline
//! test.
//!
//! Deliberately *not* a Kubernetes test. `converge` and `finalize_backend`
//! take a client and a spec, with no API server anywhere, so the backend half
//! of the reconciler is reachable on its own.
//!
//! ```sh
//! LIBVIRT_HOST=bar.foo.io \
//! LIBVIRT_TLS_DIR="$HOME/.config/banlieue/<host>/libvirt" \
//! LIBVIRT_POOL=images LIBVIRT_SOURCE_VOLUME=base.raw \
//!   cargo test -p banlieue-provider-libvirt --test live_machine -- --ignored --nocapture
//! ```
//!
//! It creates a real domain and a real volume, and removes both before it
//! returns — including when the body fails.

use std::path::{Path, PathBuf};

use banlieue_api::common::{Firmware, IpamSpec, LocalObjectReference, PowerState};
use banlieue_api::infrastructure::{
    LibvirtBootSource, LibvirtBootSourceKind, LibvirtDiskBus, LibvirtDiskSpec, LibvirtMachineSpec,
    LibvirtNicSource, LibvirtNicSourceKind, LibvirtNicSpec,
};
use banlieue_libvirt::{DEFAULT_TLS_PORT, TlsIdentity, connect_open, connect_tls};
use banlieue_provider_libvirt::machine_client::SessionMachineClient;
use banlieue_provider_libvirt::reconciler::libvirtmachine::{converge, finalize_backend};

/// Smallest disk the test asks for. A qcow2 volume is sparse, so this costs
/// kilobytes on the host regardless of the number.
const DISK_GIB: u32 = 1;

fn settings() -> Option<(String, PathBuf, String, String)> {
    let host = std::env::var("LIBVIRT_HOST").ok()?;
    let dir = std::env::var("LIBVIRT_TLS_DIR").ok()?;
    let pool = std::env::var("LIBVIRT_POOL").unwrap_or_else(|_| "default".to_string());
    let source = std::env::var("LIBVIRT_SOURCE_VOLUME").ok()?;
    Some((host, PathBuf::from(expand_home(&dir)), pool, source))
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
            // An overlay over the imported image: the `Immediate` shape, and
            // the one that matches a `.raw` artifact from banlieue's own
            // import pipeline (ADR-0011).
            kind: LibvirtBootSourceKind::BackingVolume,
            volume: source_volume.to_string(),
        },
        vcpus: 1,
        memory_mi_b: 512,
        // EFI with no explicit loader, so this also proves libvirt's firmware
        // autoselection works on a real host rather than only in a unit test.
        firmware: Firmware::Efi,
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
        // Exercises the cloud-init seed path (ADR-0054). The payload is
        // deliberately inert — the point is that libvirtd accepts the
        // generated ISO as a CD-ROM and the volume lands in the pool, not
        // that a guest runs it.
        user_data: Some("#cloud-config\nhostname: banlieue-livetest\n".to_string()),
        desired_power_state: PowerState::PoweredOn,
    }
}

#[tokio::test]
#[ignore = "creates a real domain and volume; set LIBVIRT_HOST, LIBVIRT_TLS_DIR, LIBVIRT_SOURCE_VOLUME"]
async fn converge_and_finalize_against_real_libvirtd() {
    let Some((host, dir, pool, source_volume)) = settings() else {
        panic!(
            "set LIBVIRT_HOST, LIBVIRT_TLS_DIR and LIBVIRT_SOURCE_VOLUME\n  \
             LIBVIRT_POOL defaults to \"default\", LIBVIRT_NETWORK to \"default\""
        );
    };
    let identity = load_identity(&dir);

    let mut session = connect_tls(&host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS connection failed");
    connect_open(&mut session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN failed");
    let mut client = SessionMachineClient::new(session);

    let domain_name = format!(
        "banlieue-livetest-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_secs()
    );
    let spec = spec(&domain_name, &pool, &source_volume);
    eprintln!("converging {domain_name} (pool={pool}, source={source_volume})");

    // Run the body catching the result, so teardown happens either way.
    let outcome = converge(&mut client, &spec, false).await;

    match &outcome {
        Ok(observed) => {
            eprintln!("  state         {:?}", observed.state);
            eprintln!("  addresses     {:?}", observed.addresses);
            eprintln!("  address src   {:?}", observed.address_source);
        }
        Err(e) => eprintln!("  converge FAILED: {e}"),
    }

    // Check the seed here, while it still exists — `finalize_backend`
    // below deletes it. ADR-0054 Decision 3 names a real host accepting
    // this image as the acceptance test, because the offline tests can
    // only prove it self-consistent.
    let seed_present = outcome.is_ok() && seed_exists(&mut client, &pool, &domain_name).await;
    eprintln!("  seed volume present: {seed_present}");

    // A second pass must be a no-op on the backend: this is the property the
    // reconciler relies on every time it is re-entered, and the one that
    // would silently discard a running VM's disk if it were wrong.
    let second = if outcome.is_ok() {
        eprintln!("  second converge (idempotence)");
        Some(converge(&mut client, &spec, false).await)
    } else {
        None
    };

    eprintln!("  tearing down");
    let teardown = finalize_backend(&mut client, &spec).await;

    // Assert last, so nothing above can leave a domain behind.
    let observed = outcome.expect("converge failed");
    assert!(
        observed.state.is_running(),
        "domain should be running, was {:?}",
        observed.state
    );
    assert!(
        observed.domain.uuid.iter().any(|&b| b != 0),
        "libvirt returned a zero UUID"
    );
    if let Some(second) = second {
        let second: banlieue_provider_libvirt::reconciler::libvirtmachine::Observed =
            second.expect("second converge failed");
        assert_eq!(
            second.domain.uuid, observed.domain.uuid,
            "a second converge produced a different domain"
        );
    }
    assert!(
        seed_present,
        "cloud-init seed volume was not created for {domain_name}"
    );
    teardown.expect("teardown failed");

    // And prove the teardown actually happened, rather than trusting it.
    let still_there = client_lookup(&mut client, &domain_name).await;
    assert!(
        !still_there,
        "domain {domain_name} is still defined after finalize_backend"
    );
    eprintln!("  torn down and confirmed gone");
}

/// Whether this machine's cloud-init seed volume is in the pool.
async fn seed_exists<S>(
    client: &mut SessionMachineClient<S>,
    pool_name: &str,
    domain_name: &str,
) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    use banlieue_provider_libvirt::machine_client::LibvirtMachineClient;
    use banlieue_provider_libvirt::reconciler::libvirtmachine::seed_volume_name;
    let Ok(Some(pool)) = client.lookup_pool(pool_name).await else {
        return false;
    };
    client
        .lookup_volume(&pool, &seed_volume_name(domain_name))
        .await
        .ok()
        .flatten()
        .is_some()
}

async fn client_lookup<S>(client: &mut SessionMachineClient<S>, name: &str) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    use banlieue_provider_libvirt::machine_client::LibvirtMachineClient;
    client
        .lookup_domain(name)
        .await
        .expect("lookup after teardown failed")
        .is_some()
}
