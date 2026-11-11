// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Live test of what a Cloud Hypervisor host publishes as capacity
//! (ADR-0107, ADR-0128), on a real host. Read-only: it reads `/proc`,
//! `statvfs` and the guests' slices over the **system** bus, and changes
//! nothing, so it runs beside a live provider without disturbing it.
//!
//! Every unit test feeds these functions text or numbers; only a real host
//! shows that `MemAvailable` parses, that `MemoryCurrent` is accounted for
//! the slices, and that the published figures are plausible together.
//!
//! ```sh
//! CH_STORAGE_DIR=/srv/banlieue/ch \
//!   cargo test -p banlieue-provider-cloud-hypervisor --test live_capacity -- --ignored --nocapture
//! ```
//!
//! `CH_STORAGE_DIR` is the directory one host storage class maps to (the
//! `[storage_classes]` table of the host config). No root is needed.

use banlieue_api::banlieue::{
    Provider, ProviderCapabilities, ProviderConnection, ProviderSpec, StorageClassMapping,
};
use banlieue_api::common::LocalObjectReference;
use banlieue_provider_cloud_hypervisor::host_config::HostConfig;
use banlieue_provider_cloud_hypervisor::provider::{
    TARGET_KEY_HOST_CLASS, compute_status, gather_facts, guest_capacity,
};
use banlieue_provider_cloud_hypervisor::systemd::{Bus, GUEST_SLICE, Systemd, VMS_SLICE};
use std::collections::BTreeMap;

/// The host storage class the test maps the directory to.
const HOST_CLASS: &str = "live";
/// The Provider storage class that targets it.
const PROVIDER_CLASS: &str = "gold";
/// MiB in one GiB: published free memory is a whole number of GiB.
const MIB_PER_GIB: u64 = 1024;

fn config(storage_dir: &str) -> HostConfig {
    HostConfig::parse(&format!(
        r#"
[provider]
name = "live"
namespace = "banlieue-system"
kubeconfig = "/nonexistent"
[vmm]
binary = "/nonexistent"
version = "v0"
firmware = "/nonexistent"
[paths]
run_root = "/nonexistent"
state_root = "/nonexistent"
[guests]
uid_base = 2000000
uid_count = 1
[storage_classes]
{HOST_CLASS} = "{storage_dir}"
[network_classes]
unused = "bl-live-none"
"#
    ))
    .expect("test host config parses")
}

fn provider() -> Provider {
    let mut p = Provider::new(
        "live",
        ProviderSpec {
            provider_class_ref: LocalObjectReference {
                name: "cloud-hypervisor".into(),
            },
            connection: ProviderConnection {
                endpoint: "bar.foo.io".into(),
                credentials_ref: None,
                insecure_skip_tls_verify: false,
                ca_bundle: None,
            },
            capabilities: ProviderCapabilities {
                storage_classes: vec![StorageClassMapping {
                    name: PROVIDER_CLASS.into(),
                    target: Some(BTreeMap::from([(
                        TARGET_KEY_HOST_CLASS.to_string(),
                        HOST_CLASS.to_string(),
                    )])),
                    ..Default::default()
                }],
                network_classes: Vec::new(),
                features: Vec::new(),
            },
            paused: false,
            failure_domain_name_overrides: Default::default(),
            use_content_library: Default::default(),
            attestation: None,
            scheduling: None,
        },
    );
    p.metadata.namespace = Some("banlieue-system".into());
    p
}

#[tokio::test]
#[ignore = "reads a real Cloud Hypervisor host; set CH_STORAGE_DIR"]
async fn a_real_host_publishes_plausible_measured_capacity() {
    let dir = std::env::var("CH_STORAGE_DIR")
        .expect("set CH_STORAGE_DIR to a host storage-class directory, e.g. /srv/banlieue/ch");
    let config = config(&dir);

    // The provider's own path: gather the host, then read the slices over
    // the system bus, exactly as `reconcile` does.
    let mut facts = gather_facts(&config);
    let sd = Systemd::connect(Bus::System)
        .await
        .expect("system bus (systemd)");
    facts.guest_budget = sd.guest_slice_budget().await;
    for slice in [GUEST_SLICE, VMS_SLICE] {
        let headroom = sd
            .slice_memory_headroom(slice)
            .await
            .expect("slice properties readable");
        eprintln!("  {slice}: headroom {headroom:?} MiB");
    }
    eprintln!("  host: {:?}", facts.capacity);
    eprintln!("  budget: {:?}", facts.guest_budget);

    let host = &facts.capacity;
    let total = host.memory_mib.expect("MemTotal parsed");
    let available = host.memory_available_mib.expect("MemAvailable parsed");
    assert!(
        available <= total,
        "MemAvailable {available} > MemTotal {total}"
    );
    let space = host
        .storage
        .get(HOST_CLASS)
        .expect("statvfs of the directory");
    assert!(space.total_gib > 0, "{space:?}");
    assert!(space.free_gib <= space.total_gib, "{space:?}");

    // What reaches the scheduler.
    let cap = guest_capacity(&facts.capacity, &facts.guest_budget);
    eprintln!("  guest capacity: {cap:?}");
    let published = cap
        .memory_available_mi_b
        .expect("a measured free-memory figure");
    let limit = cap.memory_mi_b.expect("a memory figure");
    assert!(
        published <= limit,
        "available {published} > memoryMiB {limit}"
    );
    assert_eq!(published % MIB_PER_GIB, 0, "not a whole GiB: {published}");
    if let Some(headroom) = facts.guest_budget.memory_headroom_mib {
        assert!(
            published <= headroom,
            "available {published} ignores the slice headroom {headroom}"
        );
    }

    // The fixture names no real VMM or firmware, and a host missing either
    // publishes no failure domain at all; this test is about capacity, so
    // take readiness as given.
    facts.vmm_binary = true;
    facts.firmware = true;
    let status = compute_status(&provider(), &config, &facts, 1);
    let fd_cap = status.failure_domains[0]
        .attributes
        .capacity
        .clone()
        .expect("typed capacity on the failure domain");
    eprintln!("  failure domain storage: {:?}", fd_cap.storage);
    let store = fd_cap
        .storage_for(PROVIDER_CLASS)
        .expect("storage[] carries the Provider's class");
    assert_eq!(store.free_gi_b, Some(space.free_gib));
    assert_eq!(store.total_gi_b, Some(space.total_gib));
}
