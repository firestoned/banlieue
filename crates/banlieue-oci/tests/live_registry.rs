// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Push and pull against a real OCI registry.
//!
//! Any distribution-spec registry will do; the reference one runs locally:
//!
//! ```sh
//! podman run -d --name banlieue-test-registry -p 127.0.0.1:5000:5000 docker.io/library/registry:2
//! OCI_TEST_REGISTRY=localhost:5000 OCI_TEST_PLAIN_HTTP=1 \
//!   cargo test -p banlieue-oci --test live_registry -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d; running it is a request to talk to a registry, and a
//! missing one fails rather than skipping.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;

use banlieue_oci::manifest::ARTIFACT_TYPE_RAW;
use banlieue_oci::{Client, Credentials, Reference};

const MIB: usize = 1024 * 1024;

fn client() -> (Client, String) {
    banlieue_oci::install_crypto_provider();
    let registry = std::env::var("OCI_TEST_REGISTRY")
        .expect("set OCI_TEST_REGISTRY, e.g. localhost:5000 (see the module docs)");
    let plain = std::env::var("OCI_TEST_PLAIN_HTTP").is_ok();
    let credentials = Credentials {
        username: std::env::var("OCI_TEST_USERNAME").ok(),
        password: std::env::var("OCI_TEST_PASSWORD").ok(),
    };
    (
        Client::new(reqwest::Client::new(), credentials, plain),
        registry,
    )
}

#[tokio::test]
#[ignore = "needs an OCI registry: see the module docs"]
async fn a_sparse_disk_round_trips_by_digest_and_stays_sparse() {
    let (client, registry) = client();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("disk.raw");
    let mut data = vec![0u8; 16 * MIB];
    data[..3].copy_from_slice(b"MBR");
    data[9 * MIB] = 0x5A;
    std::fs::write(&src, &data).unwrap();

    let target = Reference::parse(&format!(
        "{registry}/banlieue-test/disk:e2e-{}",
        std::process::id()
    ))
    .unwrap();
    let pushed = client
        .push_file(
            &target,
            &src,
            dir.path(),
            ARTIFACT_TYPE_RAW,
            BTreeMap::from([("io.banlieue.vmimage".to_string(), "e2e".to_string())]),
        )
        .await
        .expect("push");
    println!(
        "pushed {} (layer {} bytes)",
        pushed.reference, pushed.layer_size
    );
    assert!(pushed.reference.digest.is_some());
    assert!(
        pushed.layer_size < (MIB as u64),
        "16 MiB of zeros compresses"
    );

    // Pushing again is idempotent: blobs are skipped, same digest.
    let again = client
        .push_file(
            &target,
            &src,
            dir.path(),
            ARTIFACT_TYPE_RAW,
            BTreeMap::from([("io.banlieue.vmimage".to_string(), "e2e".to_string())]),
        )
        .await
        .expect("second push");
    assert_eq!(again.reference, pushed.reference);

    let dest = dir.path().join("pulled.raw");
    let pulled = client
        .pull_file(&pushed.reference, &dest)
        .await
        .expect("pull");
    assert_eq!(pulled.len, data.len() as u64);
    assert_eq!(
        pulled
            .annotations
            .get("io.banlieue.vmimage")
            .map(String::as_str),
        Some("e2e")
    );
    assert_eq!(std::fs::read(&dest).unwrap(), data, "byte-identical");
    let allocated = std::fs::metadata(&dest).unwrap().blocks() * 512;
    assert!(
        allocated < (2 * MIB) as u64,
        "pulled file is sparse: {allocated} bytes allocated"
    );
    assert!(!dir.path().join("pulled.raw.partial").exists());
}

/// A digest the registry does not have is an error, and no file appears.
#[tokio::test]
#[ignore = "needs an OCI registry: see the module docs"]
async fn an_unknown_digest_is_refused_and_leaves_nothing() {
    let (client, registry) = client();
    let dir = tempfile::tempdir().unwrap();
    let r = Reference::parse(&format!(
        "{registry}/banlieue-test/disk@sha256:{}",
        "0".repeat(64)
    ))
    .unwrap();
    let dest = dir.path().join("x.raw");
    assert!(client.pull_file(&r, &dest).await.is_err());
    assert!(!dest.exists());
    assert!(!dir.path().join("x.raw.partial").exists());
}
