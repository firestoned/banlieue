// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! End to end on a real Cloud Hypervisor host: a `tpmEnabled`
//! `CloudHypervisorMachine` installs itself from an installer ISO
//! (`installMode: Deferred`), seals its persistent partition to its own
//! vTPM, and reports `phase=installed` over vsock; the provider ejects the
//! installer, publishes `GuestReady`, and deleting the machine leaves no
//! unit, disk or TPM state behind (ADR-0065 Decisions 1–6).
//!
//! The installer is a Kairos core ISO (verified with Hadron v0.4.0,
//! systemd 260). The user-data installs to the empty
//! first disk, encrypts `COS_PERSISTENT` with its key in the TPM, reboots,
//! and gives the installed system a `boot` stage that reports with
//! `systemd-notify` over `vsock-stream:2:<port>` (systemd 256 or later; no
//! extra package in the image).
//!
//! Runs **on the host**, and **as root**: it reads the guest's disk and the
//! provider's state directories, which neither the guest's uid nor an admin
//! login can. `make ch-deferred-e2e` builds as you and runs only the test
//! binary through `sudo`. `#[ignore]`d; a missing setting fails loudly.
//!
//! ```sh
//! export KUBECONFIG=~/.kube/<cluster>.yaml
//! BANLIEUE_E2E_PROVIDER=<provider>          # this host's Provider, declaring `vtpm`
//! BANLIEUE_E2E_INSTALLER=<file>             # a Kairos ISO in the class's images/
//! BANLIEUE_E2E_STORAGE_CLASS=<class>        # a host storage class name
//! BANLIEUE_E2E_NETWORK_CLASS=<class>        # a host network class name
//! BANLIEUE_E2E_STORAGE_DIR=/srv/banlieue/ch # that class's directory
//! BANLIEUE_E2E_STATE_ROOT=/var/lib/banlieue # the host's [paths] state_root
//! BANLIEUE_E2E_SSH_AUTHORIZED_KEY=<key>     # optional: a `kairos` login, for debugging
//!   make ch-deferred-e2e
//! ```

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use banlieue_api::banlieue::Provider;
use banlieue_api::infrastructure::CloudHypervisorMachine;
use banlieue_provider_cloud_hypervisor::report::REPORT_PORT;
use banlieue_provider_cloud_hypervisor::systemd::{Bus, Systemd, UnitState};
use banlieue_provider_sdk::ek::ek_cn_matches;
use kube::api::{Api, DeleteParams, PostParams};
use kube::{Client, ResourceExt};
use serde_json::json;

/// Manufacture, the install, a reboot, encryption on first boot.
const INSTALLED_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// The persistent partition is encrypted on the installed system's first
/// boot, which may finish after its report.
const SEALED_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DELETE_TIMEOUT: Duration = Duration::from_secs(3 * 60);
const POLL: Duration = Duration::from_secs(5);
const OS_DISK_GIB: u32 = 20;
const MEMORY_MIB: u32 = 4096;
const FEATURE_VTPM: &str = "vtpm";
const GUEST_READY: &str = "GuestReady";
/// Host CID, seen from a guest.
const HOST_CID: u32 = 2;

/// GPT: the header is LBA 1; these are its fields' offsets.
const SECTOR: u64 = 512;
const GPT_SIGNATURE: &[u8] = b"EFI PART";
const GPT_ENTRIES_LBA: usize = 72;
const GPT_ENTRY_COUNT: usize = 80;
const GPT_ENTRY_SIZE: usize = 84;
const GPT_ENTRY_FIRST_LBA: usize = 32;
/// A LUKS (1 or 2) header starts with this.
const LUKS_MAGIC: &[u8] = b"LUKS\xba\xbe";

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is not set; see the module docs of tests/e2e_deferred.rs")
    })
}

async fn wait_for<T, F, Fut>(what: &str, timeout: Duration, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = check().await {
            return v;
        }
        assert!(
            start.elapsed() < timeout,
            "timed out after {timeout:?} waiting for {what}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Whether anything is at `p`. Permission errors fail: this test runs as
/// root, and one it cannot see into proves nothing.
fn present(p: &Path) -> bool {
    match std::fs::symlink_metadata(p) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!(
            "stat {} (run as root: see the module docs): {e}",
            p.display()
        ),
    }
}

/// The byte offsets of the GPT partitions on `disk` that start with a LUKS
/// header. Empty when the disk has no GPT yet.
fn luks_partitions(disk: &Path) -> std::io::Result<Vec<u64>> {
    let mut f = File::open(disk)?;
    let mut header = [0u8; SECTOR as usize];
    f.seek(SeekFrom::Start(SECTOR))?;
    f.read_exact(&mut header)?;
    if &header[..GPT_SIGNATURE.len()] != GPT_SIGNATURE {
        return Ok(vec![]);
    }
    let u64_at = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let u32_at = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let entries_lba = u64_at(&header, GPT_ENTRIES_LBA);
    let count = u32_at(&header, GPT_ENTRY_COUNT) as usize;
    let size = u32_at(&header, GPT_ENTRY_SIZE) as usize;
    let mut table = vec![0u8; count * size];
    f.seek(SeekFrom::Start(entries_lba * SECTOR))?;
    f.read_exact(&mut table)?;
    let mut found = vec![];
    for entry in table.chunks(size) {
        let first = u64_at(entry, GPT_ENTRY_FIRST_LBA);
        if first == 0 {
            continue;
        }
        let mut magic = [0u8; LUKS_MAGIC.len()];
        f.seek(SeekFrom::Start(first * SECTOR))?;
        f.read_exact(&mut magic)?;
        if magic == LUKS_MAGIC {
            found.push(first * SECTOR);
        }
    }
    Ok(found)
}

/// Install to the empty first disk with `COS_PERSISTENT` sealed to the TPM,
/// reboot into it, and report from the installed system only (a live
/// installer boot must not claim an install). With `ssh_key`, a `kairos`
/// admin user to look inside a stuck install.
fn user_data(ssh_key: Option<&str>) -> String {
    let users = ssh_key.map_or_else(String::new, |k| {
        format!(
            "users:\n  - name: kairos\n    groups: [admin]\n    ssh_authorized_keys:\n      - {k}\n"
        )
    });
    format!(
        r#"#cloud-config
{users}install:
  device: /dev/vda
  auto: true
  reboot: true
  encrypted_partitions:
    - COS_PERSISTENT
stages:
  boot:
    - name: banlieue installed report
      if: '[ -e /run/cos/active_mode ]'
      commands:
        - NOTIFY_SOCKET=vsock-stream:{HOST_CID}:{REPORT_PORT} systemd-notify phase=installed
"#
    )
}

#[tokio::test]
#[ignore = "needs a Cloud Hypervisor host with [tpm] running banlieue, and root: see the module docs"]
async fn a_tpm_machine_installs_itself_seals_reports_and_leaves_nothing() {
    let provider_name = required("BANLIEUE_E2E_PROVIDER");
    let installer = required("BANLIEUE_E2E_INSTALLER");
    let storage_class = required("BANLIEUE_E2E_STORAGE_CLASS");
    let network_class = required("BANLIEUE_E2E_NETWORK_CLASS");
    let storage_dir = PathBuf::from(required("BANLIEUE_E2E_STORAGE_DIR"));
    let state_root = PathBuf::from(required("BANLIEUE_E2E_STATE_ROOT"));
    assert!(
        present(&storage_dir.join("images").join(&installer)),
        "{installer} is not in {}/images",
        storage_dir.display()
    );

    let client = Client::try_default().await.expect("a reachable cluster");
    let provider = Api::<Provider>::all(client.clone())
        .list(&Default::default())
        .await
        .expect("listing Providers")
        .into_iter()
        .find(|p| p.name_any() == provider_name)
        .unwrap_or_else(|| panic!("no Provider {provider_name}"));
    let ns = provider.namespace().expect("Providers are namespaced");
    let advertised = provider
        .status
        .as_ref()
        .and_then(|s| s.failure_domains.first())
        .is_some_and(|fd| fd.attributes.features.iter().any(|f| f == FEATURE_VTPM));
    assert!(
        advertised,
        "{provider_name} does not advertise vtpm: declare it and check the host's [tpm]"
    );

    let machines: Api<CloudHypervisorMachine> = Api::namespaced(client.clone(), &ns);
    let name = format!("e2e-deferred-{}", std::process::id());
    let machine: CloudHypervisorMachine = serde_json::from_value(json!({
        "apiVersion": "infrastructure.banlieue.io/v1alpha1",
        "kind": "CloudHypervisorMachine",
        "metadata": { "name": name, "namespace": ns },
        "spec": {
            "providerRef": { "name": provider_name },
            "cpus": { "boot": 2 },
            "memory": { "sizeMiB": MEMORY_MIB },
            "storageClass": storage_class,
            "bootSource": { "kind": "installMedia", "image": installer },
            "osDiskSizeGiB": OS_DISK_GIB,
            "nics": [{
                "name": "eth0",
                "networkClass": network_class,
                "ipam": banlieue_api::common::IpamSpec::default(),
            }],
            "tpmEnabled": true,
            "userData": user_data(std::env::var("BANLIEUE_E2E_SSH_AUTHORIZED_KEY").ok().as_deref()),
        }
    }))
    .expect("a valid CloudHypervisorMachine");
    let created = machines
        .create(&PostParams::default(), &machine)
        .await
        .expect("creating the machine");
    let uid = created.uid().expect("a UID");
    let machine_dir = storage_dir.join(&uid);
    println!("created {name} ({uid})");
    let systemd = Systemd::connect(Bus::System).await.expect("system bus");
    let host_uid = std::sync::Mutex::new(None::<u32>);
    let started = Instant::now();

    let outcome = async {
        // While installing: the guest's own copy of the installer is
        // attached, and GuestReady says it is waiting.
        let installing = wait_for(
            "the installer to be attached",
            INSTALLED_TIMEOUT,
            || async {
                let st = machines.get(&name).await.ok()?.status?;
                (st.install_media_detached == Some(false)).then_some(st)
            },
        )
        .await;
        let n = installing.host_uid.expect("a guest uid");
        *host_uid.lock().unwrap() = Some(n);
        assert!(
            present(&machine_dir.join("install.iso")),
            "the installer is staged in the machine directory"
        );
        println!("installing as guest uid {n}");

        let ready = wait_for(
            "GuestReady after the install",
            INSTALLED_TIMEOUT,
            || async {
                let st = machines.get(&name).await.ok()?.status?;
                st.conditions
                    .iter()
                    .any(|c| c.type_ == GUEST_READY && c.status == "True")
                    .then_some(st)
            },
        )
        .await;
        println!(
            "installed, reported and ejected after {:?}",
            started.elapsed()
        );
        assert_eq!(ready.guest_installed, Some(true));
        assert_eq!(ready.install_media_detached, Some(true));
        assert_eq!(ready.tpm_attached, Some(true));
        assert!(!ready.tpm_endorsement_certificates.is_empty());
        for pem in &ready.tpm_endorsement_certificates {
            assert!(
                ek_cn_matches(pem, &name, &uid),
                "EK certificate CN is not {name}:{uid}"
            );
        }
        // The copy goes on the pass after the eject.
        wait_for(
            "the installer copy to be deleted",
            DELETE_TIMEOUT,
            || async { (!present(&machine_dir.join("install.iso"))).then_some(()) },
        )
        .await;

        let os_disk = machine_dir.join("os.raw");
        let sealed = wait_for(
            "a LUKS partition on the OS disk",
            SEALED_TIMEOUT,
            || async {
                let found = luks_partitions(&os_disk).expect("reading the OS disk");
                (!found.is_empty()).then_some(found)
            },
        )
        .await;
        println!("OS disk has LUKS at byte offset(s) {sealed:?}");
        assert_eq!(
            systemd
                .state(&format!("banlieue-swtpm@{n}.service"))
                .await
                .expect("unit state"),
            Some(UnitState::Active),
            "swtpm runs while the guest does"
        );
    };
    let checked = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(outcome)).await;

    machines
        .delete(&name, &DeleteParams::default())
        .await
        .expect("deleting the machine");
    wait_for("the machine to be gone", DELETE_TIMEOUT, || async {
        machines.get_opt(&name).await.ok()?.is_none().then_some(())
    })
    .await;
    let recorded = *host_uid.lock().unwrap();
    if let Some(n) = recorded {
        for template in ["banlieue-ch", "banlieue-swtpm", "banlieue-swtpm-setup"] {
            let unit = format!("{template}@{n}.service");
            let state = systemd.state(&unit).await.expect("unit state");
            assert!(
                matches!(state, None | Some(UnitState::Inactive)),
                "{unit} left behind: {state:?}"
            );
        }
        assert!(
            !present(&state_root.join("tpm").join(n.to_string())),
            "TPM state left behind"
        );
        assert!(
            !present(
                &state_root
                    .join("units")
                    .join(format!("swtpm-setup-{n}.env"))
            ),
            "manufacture environment file left behind"
        );
    }
    assert!(!present(&machine_dir), "machine directory left behind");
    assert!(
        !present(&state_root.join("ek").join(&uid)),
        "EK certificates left behind"
    );
    println!("deleted; no unit, disk or TPM state left");
    if let Err(panic) = checked {
        std::panic::resume_unwind(panic);
    }
}
