// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Live protocol test: the client against a real `cloud-hypervisor` process.
//!
//! Proves the VMM accepts what this crate sends and returns what it decodes,
//! which no offline test can: the fixtures are captured from one release, and
//! the schema check proves field names, not behaviour. It never boots a
//! guest, so it needs `/dev/kvm` access but no image, bridge or root.
//!
//! ```sh
//! CH_BINARY=/usr/local/bin/cloud-hypervisor \
//! CH_FIRMWARE=/opt/banlieue/firmware/ch-97eeb7b09/CLOUDHV.fd \
//!   cargo test -p banlieue-cloud-hypervisor --test live_vmm -- --ignored
//! ```
//!
//! `#[ignore]`d, so running it is already an explicit request to talk to a
//! real VMM: missing configuration is a failure, never a silent pass.

use banlieue_cloud_hypervisor::{Client, Error, GuestPlan, PlannedDisk, VmState, check_version};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
/// Size of the throwaway disk. Never booted, so any size will do.
const DISK_BYTES: u64 = 64 * 1024 * 1024;
const MEMORY_MIB: u64 = 512;
/// Polls of the socket path while the VMM starts.
const SOCKET_WAIT_POLLS: u32 = 100;
const SOCKET_WAIT_STEP: Duration = Duration::from_millis(50);

fn required_env(name: &str) -> PathBuf {
    let v = std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} is unset. This test talks to a real VMM; set CH_BINARY and CH_FIRMWARE \
             (see the module docs)."
        )
    });
    let p = PathBuf::from(v);
    assert!(p.exists(), "{name}={} does not exist", p.display());
    p
}

/// The VMM process, killed on drop so a failed assertion leaks nothing.
struct Vmm(Child);

impl Drop for Vmm {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn start_vmm(binary: &Path, socket: &Path) -> Vmm {
    let child = Command::new(binary)
        .arg("--api-socket")
        .arg(format!("path={}", socket.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cloud-hypervisor");
    let vmm = Vmm(child);
    for _ in 0..SOCKET_WAIT_POLLS {
        if socket.exists() {
            return vmm;
        }
        tokio::time::sleep(SOCKET_WAIT_STEP).await;
    }
    panic!("VMM did not create {}", socket.display());
}

#[tokio::test]
#[ignore = "needs a real cloud-hypervisor binary and firmware (CH_BINARY, CH_FIRMWARE)"]
async fn create_read_back_delete_against_a_real_vmm() {
    let binary = required_env("CH_BINARY");
    let firmware = required_env("CH_FIRMWARE");
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("api.sock");
    let disk = dir.path().join("os.raw");
    std::fs::File::create(&disk)
        .unwrap()
        .set_len(DISK_BYTES)
        .unwrap();

    let _vmm = start_vmm(&binary, &socket).await;
    let client = Client::new(&socket, TIMEOUT);

    // Version gate against the real thing.
    let ping = client.ping().await.expect("vmm.ping");
    check_version(&ping).expect("the installed VMM must pass the pin");

    // Nothing created yet: info is None, not an error.
    assert!(client.info().await.expect("vm.info").is_none());

    let plan = GuestPlan {
        firmware,
        boot_vcpus: 1,
        max_vcpus: 2,
        memory_mib: MEMORY_MIB,
        hugepages: false,
        disks: vec![PlannedDisk {
            id: "os".into(),
            path: disk,
            readonly: false,
        }],
        nics: vec![],
        tpm_socket: None,
        serial_file: dir.path().join("serial.log"),
        landlock: false,
    };
    client.create(&plan).await.expect("vm.create");

    // The invariants reached the VMM, not just the request body.
    let info = client.info().await.expect("vm.info").expect("created");
    assert_eq!(info.state, VmState::Created);
    let cpus = info.config.cpus.expect("cpus");
    assert!(!cpus.nested, "nested must be off in the running config");
    assert_eq!(cpus.max_vcpus, 2);
    assert_eq!(info.config.disks.len(), 1);
    assert_eq!(
        info.config.disks[0].image_type,
        Some(banlieue_cloud_hypervisor::types::ImageType::Raw)
    );

    // A second create is refused with the VMM's own messages.
    let err = client.create(&plan).await.expect_err("second vm.create");
    assert!(matches!(err, Error::Api { .. }), "{err:?}");

    client.delete().await.expect("vm.delete");
    assert!(client.info().await.expect("vm.info").is_none());
    client.shutdown_vmm().await.expect("vmm.shutdown");
}
