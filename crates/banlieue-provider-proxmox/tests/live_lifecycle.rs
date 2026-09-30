// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Live lifecycle tier (rules/testing.md): the provider's own reconciler
//! functions — `allocate_vmid`, `converge`, `finalize_backend` — against a
//! REAL Proxmox VE node. This is what the read-only `live_proxmox` tier in
//! `banlieue-proxmox` cannot answer: clone, config, resize, multipart ISO
//! upload, start/stop/delete, and task polling with real UPIDs.
//!
//! It **creates and destroys VMs**: a full clone of the template, plus one
//! foreign VM that shares the machine's name but not its ownership marker,
//! which must survive the machine's delete (ADR-0075 Decision 4). Everything
//! it creates is removed on the way out, including after a failed assertion.
//!
//! `#[ignore]`d, so running it is an explicit request to talk to a node: an
//! unset variable is a failure that names what is missing, never a skip.
//!
//! ```text
//! PROXMOX_ENDPOINT=https://bar.foo.io:8006 \
//! PROXMOX_TOKEN_ID='banlieue@pve!provider' PROXMOX_TOKEN_SECRET=<uuid> \
//! PROXMOX_CA_FILE=./proxmox-ca.pem \
//! PROXMOX_NODE=pve PROXMOX_TEMPLATE_VMID=9000 \
//!   make proxmox-lifecycle-test
//! ```
//!
//! Optional: `PROXMOX_STORAGE` (default `local-lvm`), `PROXMOX_ISO_STORAGE`
//! (default `banlieue-seed`, the dedicated seed storage the bootstrap `seed`
//! step creates), `PROXMOX_BRIDGE` (default `vmbr0`).

use std::panic::AssertUnwindSafe;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::FutureExt;

use banlieue_api::common::{Firmware, IpamSpec, LocalObjectReference, PowerState};
use banlieue_api::infrastructure::{ProxmoxMachineSpec, ProxmoxNicModel, ProxmoxNicSpec};
use banlieue_provider_proxmox::config::{is_ours, seed_volid};
use banlieue_provider_proxmox::reconciler::proxmoxmachine::{
    Allocation, MachineRef, allocate_vmid, converge, finalize_backend,
};
use banlieue_proxmox::{ApiToken, Client, ClientConfig, CloneParams, ProxmoxApi};

const WHOLE_TEST_TIMEOUT: Duration = Duration::from_secs(1800);
const FOREIGN_TASK_TIMEOUT: Duration = Duration::from_secs(900);
const OS_DISK_GIB: u32 = 4;
const MEMORY_MIB: u32 = 1024;

fn var(name: &str, example: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}, e.g. {name}={example}"))
}

fn var_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn client() -> Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let token = ApiToken::new(
        &var("PROXMOX_TOKEN_ID", "'banlieue@pve!provider'"),
        &var("PROXMOX_TOKEN_SECRET", "<uuid>"),
    )
    .expect("token");
    let mut cfg = ClientConfig::new(&var("PROXMOX_ENDPOINT", "https://bar.foo.io:8006"), token);
    if let Ok(path) = std::env::var("PROXMOX_CA_FILE") {
        cfg.ca_bundle_pem = Some(
            std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("PROXMOX_CA_FILE {path}: {e}")),
        );
    }
    Client::new(cfg).expect("client")
}

/// A uid unique to this run, shaped like a Kubernetes UID.
fn run_uid() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "e2e00000-0000-4000-8000-{:012x}",
        (nanos ^ u128::from(std::process::id())) & 0xffff_ffff_ffff
    )
}

fn spec(node: &str, template: u32) -> ProxmoxMachineSpec {
    ProxmoxMachineSpec {
        provider_id: None,
        failure_domain: None,
        provider_ref: LocalObjectReference {
            name: "pve-e2e".to_string(),
        },
        node: node.to_string(),
        template_vmid: template,
        storage: var_or("PROXMOX_STORAGE", "local-lvm"),
        pool: None,
        cores: 1,
        sockets: 1,
        memory_mi_b: MEMORY_MIB,
        cpu_type: None,
        // The bootstrap template is SeaBIOS; this tier is about the API, not firmware.
        firmware: Firmware::Bios,
        tpm_enabled: false,
        os_disk_size_gi_b: OS_DISK_GIB,
        data_disks: vec![],
        nics: vec![ProxmoxNicSpec {
            name: "eth0".to_string(),
            bridge: var_or("PROXMOX_BRIDGE", "vmbr0"),
            vlan: None,
            model: ProxmoxNicModel::Virtio,
            mac_address: None,
            ipam: IpamSpec::default(),
        }],
        iso_storage: Some(var_or("PROXMOX_ISO_STORAGE", "banlieue-seed")),
        user_data: Some("#cloud-config\nhostname: banlieue-e2e\n".to_string()),
        desired_power_state: PowerState::PoweredOn,
    }
}

/// Best-effort removal of a VM this test created by hand.
async fn destroy(api: &Client, node: &str, vmid: u32) {
    if let Ok(s) = api.vm_status(node, vmid).await
        && s.is_running()
        && let Ok(u) = api.stop_vm(node, vmid).await
    {
        let _ = api.wait_task(&u, FOREIGN_TASK_TIMEOUT).await;
    }
    if let Ok(u) = api.delete_vm(node, vmid).await {
        let _ = api.wait_task(&u, FOREIGN_TASK_TIMEOUT).await;
    }
}

#[tokio::test]
#[ignore = "creates and destroys VMs on a Proxmox VE node"]
async fn a_machine_is_cloned_seeded_started_and_removed_without_touching_a_foreign_namesake() {
    let api = client();
    let node = var("PROXMOX_NODE", "pve");
    let template: u32 = var("PROXMOX_TEMPLATE_VMID", "9000")
        .parse()
        .expect("PROXMOX_TEMPLATE_VMID is a number");
    let uid = run_uid();
    let name = format!("banlieue-e2e-{}", &uid[uid.len() - 8..]);
    let m = MachineRef {
        name: &name,
        uid: &uid,
    };
    let spec = spec(&node, template);

    // A foreign VM that shares the machine's name but carries no marker: an
    // admin's VM, or another namespace's machine. It must never be touched.
    let foreign_id = api.next_id().await.expect("nextid").0;
    let u = api
        .clone_vm(&node, template, &CloneParams::new(foreign_id).name(&name))
        .await
        .expect("clone foreign namesake");
    api.wait_task(&u, FOREIGN_TASK_TIMEOUT)
        .await
        .expect("foreign clone task");

    // `catch_unwind`, because a failed assertion is a panic, and a panic
    // unwinds straight past any cleanup written after it. The first live run
    // of this test left two VMs and a seed ISO behind for exactly that reason.
    let outcome = AssertUnwindSafe(tokio::time::timeout(WHOLE_TEST_TIMEOUT, async {
        // --- create ------------------------------------------------------
        let alloc = allocate_vmid(&api, &m, None).await.expect("allocate");
        let Allocation::Fresh { vmid } = alloc else {
            panic!("the foreign namesake was adopted: {alloc:?}");
        };
        assert_ne!(vmid, foreign_id);
        let observed = converge(&api, &m, &spec, &alloc, true)
            .await
            .expect("first converge");
        assert_eq!(observed.vmid, vmid);
        assert_eq!(observed.power, PowerState::PoweredOn, "{observed:?}");
        assert!(observed.configured);

        let cfg = api.vm_config(&node, vmid).await.expect("config");
        assert!(is_ours(&cfg, &uid), "clone lacks the ownership marker");
        assert_eq!(cfg.get("cores"), Some("1"));
        assert_eq!(cfg.get("memory"), Some(MEMORY_MIB.to_string().as_str()));
        let seed = seed_volid(spec.iso_storage.as_deref().unwrap(), &uid);
        let content = api
            .storage_content(&node, spec.iso_storage.as_deref().unwrap())
            .await
            .expect("iso storage content");
        assert!(
            content.iter().any(|v| v.volid == seed),
            "seed ISO {seed} not uploaded"
        );
        assert!(
            cfg.0.values().any(|v| v.contains(&seed)),
            "seed ISO not attached: {:?}",
            cfg.0
        );

        // --- idempotent second pass --------------------------------------
        let again = allocate_vmid(&api, &m, Some(vmid))
            .await
            .expect("re-allocate");
        assert!(
            matches!(again, Allocation::Existing { vmid: v, .. } if v == vmid),
            "{again:?}"
        );
        converge(&api, &m, &spec, &again, false)
            .await
            .expect("second converge");

        // --- delete ------------------------------------------------------
        finalize_backend(&api, &m, &spec, Some(vmid))
            .await
            .expect("finalize");
        let vms = api.cluster_vms().await.expect("inventory");
        assert!(
            !vms.iter().any(|v| v.vmid == vmid),
            "our VM survived its delete"
        );
        assert!(
            vms.iter().any(|v| v.vmid == foreign_id),
            "the foreign namesake was deleted"
        );
        let content = api
            .storage_content(&node, spec.iso_storage.as_deref().unwrap())
            .await
            .expect("iso storage content");
        assert!(
            !content.iter().any(|v| v.volid == seed),
            "seed ISO orphaned"
        );
    }))
    .catch_unwind()
    .await;

    // Clean up on every path: ours (idempotent) and the foreign VM.
    let _ = finalize_backend(&api, &m, &spec, None).await;
    destroy(&api, &node, foreign_id).await;

    match outcome {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(timed) => timed.expect("lifecycle exceeded its deadline"),
    }
}
