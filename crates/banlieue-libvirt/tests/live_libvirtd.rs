// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Integration test against a **real libvirtd** over mutual TLS.
//!
//! Every other test in this crate validates our *reading* of libvirt's
//! `remote_protocol.x`. None of them can validate that the reading is
//! correct: a self-consistent misunderstanding of a struct layout round-trips
//! against itself perfectly and only fails on contact with the real daemon.
//! ADR-0011 therefore records this test as non-optional.
//!
//! `#[ignore]`d by default so `cargo test` stays hermetic. Run it explicitly:
//!
//! ```sh
//! LIBVIRT_HOST=bar.foo.io \
//! LIBVIRT_TLS_DIR="$HOME/.config/banlieue/libvirt" \
//!   cargo test -p banlieue-libvirt --test live_libvirtd -- --ignored --nocapture
//! ```
//!
//! `LIBVIRT_TLS_DIR` must contain `ca.pem`, `client-cert.pem` and
//! `client-key.pem` — what `scripts/bootstrap-libvirt-tls.sh` produces. Keep
//! that directory outside the repository; it holds a private key.
//!
//! `LIBVIRT_HOST` must match a SAN in libvirtd's server certificate, because
//! libvirt validates against the address the client actually dialled.

use std::path::{Path, PathBuf};

use banlieue_libvirt::{
    DEFAULT_TLS_PORT, Domain, InterfaceAddressSource, Session, TlsIdentity, TransportError,
    connect_open, connect_tls, domain_create, domain_define_xml, domain_destroy, domain_get_state,
    domain_interface_addresses, domain_lookup_by_name, domain_undefine, list_all_networks,
    list_all_storage_pools, raw_volume_xml, storage_pool_list_all_volumes, storage_vol_create_xml,
    storage_vol_upload,
};
use tokio::io::{AsyncRead, AsyncWrite};

/// Read the connection settings, or explain precisely what is missing.
fn settings() -> Option<(String, PathBuf)> {
    let host = std::env::var("LIBVIRT_HOST").ok()?;
    let dir = std::env::var("LIBVIRT_TLS_DIR").ok()?;
    Some((host, PathBuf::from(shellexpand_home(&dir))))
}

/// Expand a leading `~/`, which a shell would normally have done.
fn shellexpand_home(p: &str) -> String {
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

#[tokio::test]
#[ignore = "requires a live libvirtd over TLS; set LIBVIRT_HOST and LIBVIRT_TLS_DIR"]
async fn connect_open_and_list_against_real_libvirtd() {
    let Some((host, dir)) = settings() else {
        panic!(
            "set LIBVIRT_HOST and LIBVIRT_TLS_DIR to run this test\n  \
             e.g. LIBVIRT_HOST=bar.foo.io LIBVIRT_TLS_DIR=~/.config/banlieue/libvirt"
        );
    };
    let identity = load_identity(&dir);

    // 1. TLS handshake. Exercises PEM parsing, the client certificate as the
    //    credential (auth_tls="none"), and SAN validation for `host`.
    eprintln!("connecting to {host}:{DEFAULT_TLS_PORT} over mutual TLS...");
    let mut session = connect_tls(&host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS connection failed");
    eprintln!("  TLS established");

    // 2. CONNECT_OPEN. The first real RPC: if the header framing, the length
    //    prefix, or the optional-string encoding of the URI is wrong, this is
    //    where it shows.
    connect_open(&mut session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN failed");
    eprintln!("  CONNECT_OPEN ok");

    // 3. A reply carrying a variable-length array of structs — the layout
    //    that offline tests can only assume.
    let pools = list_all_storage_pools(&mut session)
        .await
        .expect("LIST_ALL_STORAGE_POOLS failed");
    eprintln!("  storage pools ({}):", pools.len());
    for p in &pools {
        eprintln!("    {:<20} {}", p.name, hex(&p.uuid));
    }

    let nets = list_all_networks(&mut session)
        .await
        .expect("LIST_ALL_NETWORKS failed");
    eprintln!("  networks ({}):", nets.len());
    for n in &nets {
        eprintln!("    {:<20} {}", n.name, hex(&n.uuid));
    }

    // Assertions kept environment-independent: any libvirt host has at least
    // one pool and one network, and a decode that silently went wrong shows up
    // as empty names or an all-zero UUID rather than as an error.
    assert!(!pools.is_empty(), "expected at least one storage pool");
    assert!(!nets.is_empty(), "expected at least one network");
    for p in &pools {
        assert!(!p.name.is_empty(), "pool name decoded empty");
        assert!(
            p.uuid.iter().any(|&b| b != 0),
            "pool {} has a zero UUID",
            p.name
        );
    }
    for n in &nets {
        assert!(!n.name.is_empty(), "network name decoded empty");
        assert!(
            n.uuid.iter().any(|&b| b != 0),
            "network {} has a zero UUID",
            n.name
        );
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
/// Upload a real file into a real storage pool and leave it in place for the
/// caller to verify by hash.
///
/// Separate from the read-only test above because it *writes* to the host.
/// Requires `LIBVIRT_UPLOAD_SRC` (path to a local file) in addition to the
/// usual settings; `LIBVIRT_POOL` selects the target pool (default `default`)
/// and `LIBVIRT_VOL` the volume name.
///
/// Deliberately does NOT delete the volume: the point is to compare its
/// contents against the source afterwards. `virsh vol-delete` cleans up.
#[tokio::test]
#[ignore = "writes a volume to a real libvirt host; set LIBVIRT_UPLOAD_SRC"]
async fn upload_a_real_file_into_a_real_pool() {
    let Some((host, dir)) = settings() else {
        panic!("set LIBVIRT_HOST and LIBVIRT_TLS_DIR");
    };
    let src = std::env::var("LIBVIRT_UPLOAD_SRC").expect("set LIBVIRT_UPLOAD_SRC");
    let pool_name = std::env::var("LIBVIRT_POOL").unwrap_or_else(|_| "default".to_string());
    let vol_name = std::env::var("LIBVIRT_VOL")
        .unwrap_or_else(|_| "banlieue-live-upload-test.raw".to_string());

    let identity = load_identity(&dir);
    let bytes = std::fs::metadata(&src).expect("stat source").len();
    eprintln!("uploading {src} ({bytes} bytes) -> pool={pool_name} vol={vol_name}");

    let mut session = connect_tls(&host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS connection failed");
    connect_open(&mut session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN failed");

    let pool = list_all_storage_pools(&mut session)
        .await
        .expect("listing pools")
        .into_iter()
        .find(|p| p.name == pool_name)
        .unwrap_or_else(|| panic!("pool {pool_name} not found"));

    let xml = raw_volume_xml(&vol_name, bytes);
    let vol = storage_vol_create_xml(&mut session, &pool, &xml)
        .await
        .expect("STORAGE_VOL_CREATE_XML failed");
    eprintln!("  created volume key={}", vol.key);

    let mut file = tokio::fs::File::open(&src).await.expect("open source");
    storage_vol_upload(&mut session, &vol, &mut file, bytes)
        .await
        .expect("STORAGE_VOL_UPLOAD failed");
    eprintln!("  uploaded {bytes} bytes");
    eprintln!(
        "VERIFY: compare a hash of {src} against {} on the host",
        vol.key
    );
}

/// List the volumes in a pool against a real host.
///
/// This is what makes the import idempotent — `banlieue provider libvirt
/// import` skips the transfer when the volume is already there — so getting the
/// decode wrong would mean re-uploading a multi-gigabyte disk on every retry,
/// or worse, failing because the volume it created last time still exists.
///
/// Run after `upload_a_real_file_into_a_real_pool` to see its volume listed:
///
/// ```sh
/// LIBVIRT_HOST=<host> LIBVIRT_TLS_DIR=~/.config/banlieue/libvirt \
///   cargo test -p banlieue-libvirt --test live_libvirtd list_volumes -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "requires a reachable libvirtd; set LIBVIRT_HOST and LIBVIRT_TLS_DIR"]
async fn list_volumes_in_a_real_pool() {
    let Some((host, dir)) = settings() else {
        panic!("set LIBVIRT_HOST and LIBVIRT_TLS_DIR");
    };
    let pool_name = std::env::var("LIBVIRT_POOL").unwrap_or_else(|_| "default".to_string());

    let identity = load_identity(&dir);
    let mut session = connect_tls(&host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS connection failed");
    connect_open(&mut session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN failed");

    let pool = list_all_storage_pools(&mut session)
        .await
        .expect("listing pools")
        .into_iter()
        .find(|p| p.name == pool_name)
        .unwrap_or_else(|| panic!("pool {pool_name} not found"));

    let vols = storage_pool_list_all_volumes(&mut session, &pool)
        .await
        .expect("STORAGE_POOL_LIST_ALL_VOLUMES failed");

    eprintln!("  volumes in pool {pool_name} ({}):", vols.len());
    for v in &vols {
        eprintln!("    {:<40} {}", v.name, v.key);
        // Every field must decode as text; a framing error shows up as an
        // empty or garbled name long before it shows up as an error.
        assert!(!v.name.is_empty(), "decoded an empty volume name");
        assert!(
            !v.key.is_empty(),
            "decoded an empty volume key for {}",
            v.name
        );
        assert_eq!(v.pool, pool.name, "volume reports the wrong pool");
    }
}

/// Full domain lifecycle against a real libvirtd: define → start → read state
/// → read addresses → destroy → undefine → confirm gone (ADR-0050).
///
/// This is the test that validates the domain half of `procs.rs`. The offline
/// unit tests pin our *reading* of `remote_protocol.x`; a self-consistent
/// misreading passes all of them and fails only here.
///
/// **Deliberately diskless.** The domain has no disk, no CD-ROM and no
/// storage volume of any kind, so it touches nothing on the host but the
/// domain table. It will not boot anything — that is fine and expected: the
/// point is the lifecycle transitions, not the guest. A diskless domain is
/// also the only shape that is safe to run against a host with real VMs on
/// it, because there is no path by which it can name, open or delete an
/// existing volume.
///
/// Cleans up after itself even on failure, and then *verifies* the cleanup by
/// looking the domain up again — a teardown that reports success while
/// leaving the domain defined is the exact failure this project has already
/// been bitten by once (`.wolf/cerebrum.md`, 2026-07-29).
///
/// ```sh
/// LIBVIRT_HOST=bar.foo.io LIBVIRT_TLS_DIR=~/.config/banlieue/libvirt \
///   cargo test -p banlieue-libvirt --test live_libvirtd domain_lifecycle \
///   -- --ignored --nocapture
/// ```
///
/// `LIBVIRT_DOMAIN_TYPE` (default `kvm`) and `LIBVIRT_EMULATOR` (default: let
/// libvirt choose) override the two host-specific bits.
#[tokio::test]
#[ignore = "defines and destroys a real domain; set LIBVIRT_HOST and LIBVIRT_TLS_DIR"]
async fn domain_lifecycle_against_real_libvirtd() {
    let Some((host, dir)) = settings() else {
        panic!("set LIBVIRT_HOST and LIBVIRT_TLS_DIR");
    };
    let identity = load_identity(&dir);

    let mut session = connect_tls(&host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS connection failed");
    // read_only = false: defining a domain is a write.
    connect_open(&mut session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN failed");

    let name = format!(
        "banlieue-livetest-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_secs()
    );
    let xml = diskless_domain_xml(&name);
    eprintln!("defining {name}");

    // 1. DEFINE. Returns a handle carrying the UUID libvirt assigned.
    let dom = domain_define_xml(&mut session, &xml)
        .await
        .expect("DOMAIN_DEFINE_XML_FLAGS failed");
    assert_eq!(dom.name, name, "define returned the wrong domain name");
    assert!(
        dom.uuid.iter().any(|&b| b != 0),
        "define returned a zero UUID — the handle decode is wrong"
    );
    eprintln!("  defined, uuid {}", hex(&dom.uuid));

    // Everything from here runs inside a closure so the teardown below
    // executes whether the body succeeded or panicked.
    let outcome = run_lifecycle_body(&mut session, &dom).await;

    // 2. TEARDOWN — always, and never swallowed.
    eprintln!("  tearing down");
    // destroy() fails if the domain is already off; that is not an error here.
    if let Err(e) = domain_destroy(&mut session, &dom).await {
        eprintln!("    destroy: {e} (ignored — domain may already be off)");
    }
    domain_undefine(&mut session, &dom)
        .await
        .expect("DOMAIN_UNDEFINE_FLAGS failed — the domain is still defined on the host");

    // 3. VERIFY the teardown, rather than trusting it.
    let after = domain_lookup_by_name(&mut session, &name).await;
    assert!(
        after.is_err(),
        "domain {name} is still defined after undefine — teardown reported success but did nothing"
    );
    eprintln!("  undefined and confirmed gone");

    if let Err(msg) = outcome {
        panic!("{msg}");
    }
}

/// The part of the lifecycle test that can fail without leaking a domain.
/// Returns `Err(message)` instead of panicking so the caller can always run
/// its teardown first.
async fn run_lifecycle_body<S>(session: &mut Session<S>, dom: &Domain) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // A freshly defined domain is not running.
    let state = domain_get_state(session, dom)
        .await
        .map_err(|e| format!("DOMAIN_GET_STATE (before start) failed: {e}"))?;
    eprintln!("  state before start: {state:?}");
    if state.is_running() {
        return Err(format!("a freshly defined domain reported {state:?}"));
    }

    // START.
    let started = domain_create(session, dom)
        .await
        .map_err(|e| format!("DOMAIN_CREATE_WITH_FLAGS failed: {e}"))?;
    if started.id < 0 {
        return Err(format!(
            "started domain reported id {} — expected a live domain id",
            started.id
        ));
    }
    eprintln!("  started, live id {}", started.id);

    let state = domain_get_state(session, dom)
        .await
        .map_err(|e| format!("DOMAIN_GET_STATE (after start) failed: {e}"))?;
    eprintln!("  state after start: {state:?}");
    if !state.is_running() {
        return Err(format!("started domain reported {state:?}"));
    }

    // ADDRESSES. A diskless domain has no guest, so every source is expected
    // to return either an error or an empty list. What is being checked is
    // that the *reply decodes* — a wrong layout shows up as a Protocol error,
    // not as an empty list.
    for source in [
        InterfaceAddressSource::Lease,
        InterfaceAddressSource::Agent,
        InterfaceAddressSource::Arp,
    ] {
        match domain_interface_addresses(session, dom, source).await {
            Ok(ifaces) => eprintln!("  addresses via {source:?}: {} interface(s)", ifaces.len()),
            Err(TransportError::Protocol { detail }) => {
                return Err(format!(
                    "DOMAIN_INTERFACE_ADDRESSES via {source:?} decoded wrongly: {detail}"
                ));
            }
            Err(e) => eprintln!("  addresses via {source:?}: unavailable ({e}) — expected"),
        }
    }

    Ok(())
}

/// Minimal domain XML: no disks, no CD-ROM, no volumes. See the test's own
/// doc comment for why diskless is the point rather than a shortcut.
fn diskless_domain_xml(name: &str) -> String {
    /// Enough RAM for the firmware to start and nothing more.
    const MEMORY_MIB: u32 = 128;

    let domain_type = std::env::var("LIBVIRT_DOMAIN_TYPE").unwrap_or_else(|_| "kvm".to_string());
    let emulator = match std::env::var("LIBVIRT_EMULATOR") {
        Ok(path) => format!("<emulator>{path}</emulator>"),
        Err(_) => String::new(),
    };
    format!(
        "<domain type='{domain_type}'>\
<name>{name}</name>\
<memory unit='MiB'>{MEMORY_MIB}</memory>\
<vcpu placement='static'>1</vcpu>\
<os><type arch='x86_64'>hvm</type></os>\
<features><acpi/><apic/></features>\
<devices>{emulator}<console type='pty'/></devices>\
</domain>"
    )
}

/// Delete a volume from a pool. A cleanup tool for images an operator or a
/// test uploaded by hand; banlieue's own reconcilers delete only what they
/// created.
///
/// ```sh
/// LIBVIRT_HOST=bar.foo.io LIBVIRT_TLS_DIR=~/.config/banlieue/libvirt \
/// LIBVIRT_POOL=images LIBVIRT_VOL=scratch.raw \
///   cargo test -p banlieue-libvirt --test live_libvirtd delete_a_volume -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "deletes a volume from a real pool; set LIBVIRT_POOL and LIBVIRT_VOL"]
async fn delete_a_volume_from_a_real_pool() {
    use banlieue_libvirt::{
        storage_pool_lookup_by_name, storage_vol_delete, storage_vol_lookup_by_name,
    };

    let Some((host, dir)) = settings() else {
        panic!("set LIBVIRT_HOST and LIBVIRT_TLS_DIR");
    };
    let pool_name = std::env::var("LIBVIRT_POOL").unwrap_or_else(|_| "default".to_string());
    let vol_name = std::env::var("LIBVIRT_VOL").expect("set LIBVIRT_VOL");
    let identity = load_identity(&dir);

    let mut session = connect_tls(&host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS connection failed");
    connect_open(&mut session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN failed");

    let pool = storage_pool_lookup_by_name(&mut session, &pool_name)
        .await
        .expect("pool lookup");
    match storage_vol_lookup_by_name(&mut session, &pool, &vol_name).await {
        Ok(vol) => {
            storage_vol_delete(&mut session, &vol)
                .await
                .expect("delete");
            eprintln!("deleted {pool_name}/{vol_name}");
        }
        Err(e) => eprintln!("{pool_name}/{vol_name}: {e} (nothing to delete)"),
    }
}

/// The qemu-specific RPC program, against a real libvirtd (ADR-0043).
///
/// This is the only way to find out whether our reading of libvirt's *second*
/// program is right. Every offline test asserts our own encoding against
/// itself; only a real daemon can say whether it recognises program
/// `0x2000_8087`, procedure 3 at all.
///
/// The assertion is deliberately not "the agent replied". Most domains have
/// no `qemu-guest-agent`, and that is fine — it surfaces as a libvirt *error*
/// reply, which still proves the daemon parsed the call. What must NOT happen
/// is a transport-level failure: a desynchronised stream or an undecodable
/// reply would mean the program number, procedure number or argument encoding
/// is wrong.
#[tokio::test]
#[ignore = "requires a real libvirtd; set LIBVIRT_HOST and LIBVIRT_TLS_DIR"]
async fn qemu_agent_program_is_understood_by_real_libvirtd() {
    let Some((host, dir)) = settings() else {
        panic!("set LIBVIRT_HOST and LIBVIRT_TLS_DIR");
    };
    let identity = load_identity(&dir);

    let mut session = connect_tls(&host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS connection failed");
    connect_open(&mut session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN failed");

    // Define a throwaway domain so the call has a real target. It is never
    // started, so the agent is certainly absent — which is the interesting
    // case: we want libvirtd's error, not a desync.
    let name = format!(
        "banlieue-agenttest-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_secs()
    );
    let domain = banlieue_libvirt::domain_define_xml(&mut session, &diskless_domain_xml(&name))
        .await
        .expect("defining the test domain");
    eprintln!("defined {name}");

    let result = banlieue_libvirt::domain_qemu_agent_command(
        &mut session,
        &domain,
        r#"{"execute":"guest-ping"}"#,
        banlieue_libvirt::AGENT_TIMEOUT_DEFAULT,
    )
    .await;

    // Clean up before asserting, so a failure cannot leave a domain behind.
    let undefined = banlieue_libvirt::domain_undefine(&mut session, &domain).await;

    match &result {
        Ok(reply) => eprintln!("  agent replied: {reply:?}"),
        Err(banlieue_libvirt::TransportError::Remote { message, .. }) => {
            eprintln!("  libvirtd returned an error, as expected: {message}");
        }
        Err(e) => panic!(
            "libvirtd did not understand the qemu program: {e}\n\
             A Protocol or Desynchronised error here means the program number, \
             procedure number or argument encoding is wrong."
        ),
    }
    undefined.expect("undefining the test domain");
    eprintln!("  ✓ qemu program {:#x} accepted", banlieue_libvirt::QEMU_PROGRAM);
}

