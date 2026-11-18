// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! End to end on a real Cloud Hypervisor host: restarting the provider —
//! which is what an upgrade does — never disturbs a guest (roadmap 09's
//! stop condition and restart test).
//!
//! The provider's service is restarted three times around one machine:
//!
//! 1. **mid-provision**, as soon as the machine has a guest uid: the next
//!    process finishes the bring-up and the machine becomes `Ready`;
//! 2. **with the guest running**: the guest's VMM is the same process
//!    afterwards (same systemd `InvocationID` and PID), and the new provider
//!    has adopted it (it re-binds the guest's report socket, a new inode);
//! 3. **mid-delete**, right after the delete is requested: the next process
//!    finishes the teardown, and nothing is left behind.
//!
//! The machine is created **directly**, as `e2e_vtpm.rs` does, from an
//! installed image in the cache. Runs **on the host**, **as root**: it
//! restarts a system service and looks inside the run directory. `make
//! ch-restart-e2e` builds as you and runs only the test binary through
//! `sudo`. `#[ignore]`d; a missing setting fails loudly.
//!
//! ```sh
//! export KUBECONFIG=~/.kube/<cluster>.yaml
//! BANLIEUE_E2E_PROVIDER=<provider>          # this host's Provider
//! BANLIEUE_E2E_PROVIDER_UNIT=banlieue-provider-cloud-hypervisor.service
//! BANLIEUE_E2E_BOOT_IMAGE=<file>            # an installed raw image in the class's images/
//! BANLIEUE_E2E_STORAGE_CLASS=<class>        # a host storage class name
//! BANLIEUE_E2E_NETWORK_CLASS=<class>        # a host network class name
//! BANLIEUE_E2E_STORAGE_DIR=/srv/banlieue/ch # that class's directory
//! BANLIEUE_E2E_RUN_ROOT=/run/banlieue/ch    # the host's [paths] run_root
//!   make ch-restart-e2e
//! ```

mod common;

use common::{present_as_root as present, required, taps_for, wait_for};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use banlieue_api::infrastructure::CloudHypervisorMachine;
use banlieue_provider_cloud_hypervisor::report::REPORT_PORT;
use kube::api::{Api, DeleteParams, PostParams};
use kube::{Client, ResourceExt};
use serde_json::json;

const READY_TIMEOUT: Duration = Duration::from_secs(4 * 60);
const ADOPT_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const DELETE_TIMEOUT: Duration = Duration::from_secs(3 * 60);
const POLL: Duration = Duration::from_secs(2);
const OS_DISK_GIB: u32 = 20;
const MEMORY_MIB: u32 = 2048;
const READY: &str = "Ready";

fn systemctl(args: &[&str]) -> String {
    let out = Command::new("systemctl")
        .args(args)
        .output()
        .expect("running systemctl");
    assert!(
        out.status.success(),
        "systemctl {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A unit's `InvocationID` and main PID: both change when systemd starts
/// the unit again, and neither changes otherwise.
fn invocation(unit: &str) -> (String, String) {
    let id = systemctl(&["show", "--value", "-p", "InvocationID", unit]);
    let pid = systemctl(&["show", "--value", "-p", "MainPID", unit]);
    (id, pid)
}

/// Restart the provider and wait until systemd says it is running again.
fn restart_provider(unit: &str) {
    systemctl(&["restart", unit]);
    assert_eq!(
        systemctl(&["is-active", unit]),
        "active",
        "{unit} came back"
    );
}

fn inode(p: &Path) -> Option<u64> {
    std::fs::symlink_metadata(p).ok().map(|m| m.ino())
}

#[tokio::test]
#[ignore = "needs a Cloud Hypervisor host running banlieue, and root: see the module docs"]
async fn restarting_the_provider_never_disturbs_a_guest() {
    let provider_name = required("BANLIEUE_E2E_PROVIDER");
    let provider_unit = required("BANLIEUE_E2E_PROVIDER_UNIT");
    let image = required("BANLIEUE_E2E_BOOT_IMAGE");
    let storage_class = required("BANLIEUE_E2E_STORAGE_CLASS");
    let network_class = required("BANLIEUE_E2E_NETWORK_CLASS");
    let storage_dir = PathBuf::from(required("BANLIEUE_E2E_STORAGE_DIR"));
    let run_root = PathBuf::from(required("BANLIEUE_E2E_RUN_ROOT"));
    assert_eq!(
        systemctl(&["is-active", &provider_unit]),
        "active",
        "the provider runs before the test starts"
    );

    let client = Client::try_default().await.expect("a reachable cluster");
    let ns = std::env::var("BANLIEUE_E2E_NAMESPACE").unwrap_or_else(|_| "banlieue-system".into());
    let machines: Api<CloudHypervisorMachine> = Api::namespaced(client, &ns);
    let name = format!("e2e-restart-{}", std::process::id());
    let machine: CloudHypervisorMachine = serde_json::from_value(json!({
        "apiVersion": "infrastructure.banlieue.io/v1alpha1",
        "kind": "CloudHypervisorMachine",
        "metadata": { "name": name, "namespace": ns },
        "spec": {
            "providerRef": { "name": provider_name },
            "cpus": { "boot": 2 },
            "memory": { "sizeMiB": MEMORY_MIB },
            "storageClass": storage_class,
            "bootSource": { "kind": "image", "image": image },
            "osDiskSizeGiB": OS_DISK_GIB,
            "nics": [{
                "name": "eth0",
                "networkClass": network_class,
                "ipam": banlieue_api::common::IpamSpec::default(),
            }],
        }
    }))
    .expect("a valid CloudHypervisorMachine");
    let created = machines
        .create(&PostParams::default(), &machine)
        .await
        .expect("creating the machine");
    let uid = created.uid().expect("a UID");
    println!("created {name} ({uid})");
    let host_uid = std::sync::Mutex::new(None::<u32>);

    let outcome = async {
        // 1. Mid-provision: as soon as the guest uid is recorded.
        let n = wait_for(POLL, "a guest uid", READY_TIMEOUT, || async {
            machines.get(&name).await.ok()?.status?.host_uid
        })
        .await;
        *host_uid.lock().unwrap() = Some(n);
        restart_provider(&provider_unit);
        println!("restarted the provider mid-provision (guest uid {n})");
        wait_for(
            POLL,
            "Ready after a mid-provision restart",
            READY_TIMEOUT,
            || async {
                let st = machines.get(&name).await.ok()?.status?;
                st.conditions
                    .iter()
                    .any(|c| c.type_ == READY && c.status == "True")
                    .then_some(())
            },
        )
        .await;
        println!("Ready");

        // 2. With the guest running: an upgrade.
        let vmm = format!("banlieue-ch@{n}.service");
        let before = invocation(&vmm);
        let report = run_root
            .join(n.to_string())
            .join(format!("vsock.sock_{REPORT_PORT}"));
        let listener_before = inode(&report).expect("the report socket exists");
        restart_provider(&provider_unit);
        wait_for(
            POLL,
            "the new provider to adopt the guest",
            ADOPT_TIMEOUT,
            || async { inode(&report).filter(|i| *i != listener_before) },
        )
        .await;
        assert_eq!(
            invocation(&vmm),
            before,
            "the guest's VMM was restarted by a provider restart"
        );
        let st = machines
            .get(&name)
            .await
            .expect("machine")
            .status
            .expect("status");
        assert!(
            st.conditions
                .iter()
                .any(|c| c.type_ == READY && c.status == "True"),
            "still Ready after the provider restart"
        );
        println!("provider restarted with the guest running: same VMM {before:?}");
    };
    let checked = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(outcome)).await;

    // 3. Mid-delete: restart as soon as the delete is requested.
    machines
        .delete(&name, &DeleteParams::default())
        .await
        .expect("deleting the machine");
    restart_provider(&provider_unit);
    println!("restarted the provider mid-delete");
    wait_for(POLL, "the machine to be gone", DELETE_TIMEOUT, || async {
        machines.get_opt(&name).await.ok()?.is_none().then_some(())
    })
    .await;
    let mut leaks = Vec::new();
    if let Some(n) = *host_uid.lock().unwrap() {
        let vmm = format!("banlieue-ch@{n}.service");
        // An instance of an installed template always shows as loaded;
        // what matters is that it is not running or failed.
        let state = systemctl(&["show", "--value", "-p", "ActiveState", &vmm]);
        if state != "inactive" {
            leaks.push(format!("unit {vmm} ({state})"));
        }
        let run_dir = run_root.join(n.to_string());
        if present(&run_dir) {
            leaks.push(run_dir.display().to_string());
        }
    }
    leaks.extend(taps_for(&uid).into_iter().map(|t| format!("tap {t}")));
    let machine_dir = storage_dir.join(&uid);
    if present(&machine_dir) {
        leaks.push(machine_dir.display().to_string());
    }
    assert!(leaks.is_empty(), "left behind: {leaks:?}");
    println!("deleted across a restart; nothing left");
    if let Err(panic) = checked {
        std::panic::resume_unwind(panic);
    }
}
