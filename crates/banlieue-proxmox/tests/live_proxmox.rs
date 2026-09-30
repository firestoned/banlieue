// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Live protocol tier (rules/testing.md): does a REAL Proxmox VE accept and
//! answer what this client sends? Read-only: it creates nothing.
//!
//! `#[ignore]`d, so running it is an explicit request to talk to a node: an
//! unset variable is a failure that names what is missing, never a skip.
//!
//! ```text
//! PROXMOX_ENDPOINT=https://bar.foo.io:8006 \
//! PROXMOX_TOKEN_ID='banlieue@pve!provider' \
//! PROXMOX_TOKEN_SECRET=<uuid> \
//! PROXMOX_CA_FILE=./pve-root-ca.pem \
//!   make proxmox-live-test
//! ```
//!
//! `scripts/bootstrap-proxmox-host.sh` creates the role and token.

use banlieue_proxmox::{ApiToken, Client, ClientConfig, ProxmoxApi};

fn var(name: &str, example: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}, e.g. {name}={example}"))
}

fn client(secret: Option<&str>) -> Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let secret = secret.map_or_else(|| var("PROXMOX_TOKEN_SECRET", "<uuid>"), str::to_string);
    let token =
        ApiToken::new(&var("PROXMOX_TOKEN_ID", "'banlieue@pve!provider'"), &secret).unwrap();
    let mut cfg = ClientConfig::new(&var("PROXMOX_ENDPOINT", "https://bar.foo.io:8006"), token);
    if let Ok(path) = std::env::var("PROXMOX_CA_FILE") {
        cfg.ca_bundle_pem = Some(
            std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("PROXMOX_CA_FILE {path}: {e}")),
        );
    }
    Client::new(cfg).unwrap()
}

#[tokio::test]
#[ignore = "needs a Proxmox VE node"]
async fn inventory_endpoints_decode_against_a_real_node() {
    let c = client(None);
    let version = c.version().await.expect("GET /version");
    assert!(version.major().is_some(), "unparseable version {version:?}");

    let nodes = c.list_nodes().await.expect("GET /nodes");
    assert!(!nodes.is_empty(), "a node lists itself");
    let node = std::env::var("PROXMOX_NODE").unwrap_or_else(|_| nodes[0].node.clone());

    let storage = c.node_storage(&node).await.expect("GET storage");
    assert!(!storage.is_empty(), "node {node} has no storage");
    let nets = c.node_networks(&node).await.expect("GET network");
    assert!(
        nets.iter().any(|n| n.is_bridge()),
        "node {node} has no bridge"
    );
    c.cluster_vms().await.expect("GET /cluster/resources");
    c.next_id().await.expect("GET /cluster/nextid");
}

#[tokio::test]
#[ignore = "needs a Proxmox VE node"]
async fn a_wrong_secret_is_reported_as_unauthorized_with_proxmoxs_message() {
    let e = client(Some("00000000-0000-0000-0000-000000000000"))
        .version()
        .await
        .unwrap_err();
    assert!(e.is_unauthorized(), "{e}");
    assert!(
        !e.to_string().ends_with(": no message from Proxmox"),
        "reason phrase was lost: {e}"
    );
}
