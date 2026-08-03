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
    DEFAULT_TLS_PORT, TlsIdentity, connect_open, connect_tls, list_all_networks,
    list_all_storage_pools, raw_volume_xml, storage_pool_list_all_volumes, storage_vol_create_xml,
    storage_vol_upload,
};

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
