// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! End to end on a real Cloud Hypervisor host: a `VirtualMachine` becomes a
//! running guest with an address that **applied its user-data** (the test
//! logs in with a key the user-data installed and reads a file the
//! user-data wrote), and deleting it leaves **nothing** behind on the host
//! (the leak test). The user-data uses Kairos `stages`, so the image is a
//! Kairos image; the machine running the test needs `ssh` and `ssh-keygen`.
//!
//! Everything asserted here was first verified by hand on a bootstrapped
//! host, and several of the checks exist because a hand run found the bug
//! they now guard: a guest uid unknown to NSS, a NIC Landlock silently
//! removed, a per-pass tap re-attach.
//!
//! **Runs on the host itself**, which is where the unit, the tap and the
//! directories are; the cluster can be anywhere the host reaches. It assumes
//! banlieue is installed: the controller running, this host's provider
//! running and its `Provider` `Ready`, and a `VMImage` and `VMClass` whose
//! image is ready for this Provider. It tests the machine lifecycle, not
//! the install.
//!
//! `#[ignore]`d, so running it is an explicit request; a missing setting or
//! an unreachable cluster fails loudly rather than skipping.
//!
//! ```sh
//! export KUBECONFIG=~/.kube/<cluster>.yaml
//! BANLIEUE_E2E_PROVIDER=<provider>          # this host's Provider
//! BANLIEUE_E2E_IMAGE=<vmimage>              # ready for that Provider
//! BANLIEUE_E2E_CLASS=<vmclass>              # one disk, classes the host serves
//! BANLIEUE_E2E_STORAGE_DIR=/srv/banlieue/ch # the class's directory on this host
//!   cargo test -p banlieue-provider-cloud-hypervisor --test e2e_machine -- --ignored --nocapture
//! ```
//!
//! Optional: `BANLIEUE_E2E_NAMESPACE` (default: the Provider's namespace,
//! which is where machines must live), `BANLIEUE_E2E_RUN_ROOT` (default
//! `/run/banlieue/ch`). Or `make ch-e2e`.

mod common;

use common::{condition_true, present, required, taps_for, wait_for};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use banlieue_api::banlieue::{Provider, VMImage, VirtualMachine};
use banlieue_api::infrastructure::CloudHypervisorMachine;
use banlieue_provider_cloud_hypervisor::sys;
use banlieue_provider_cloud_hypervisor::systemd::{Bus, Systemd, UnitState};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::{Api, DeleteParams, PostParams};
use kube::{Client, ResourceExt};
use serde_json::json;

/// Boot to an address: about 70 s observed; a first boot of a fresh image
/// with an install and reboot can take a few minutes.
const READY_TIMEOUT: Duration = Duration::from_secs(6 * 60);
/// sshd comes up shortly after DHCP.
const SSH_TIMEOUT: Duration = Duration::from_secs(3 * 60);
/// Teardown was 5–6 s observed; the finalizer waits for verified cleanup.
const DELETE_TIMEOUT: Duration = Duration::from_secs(3 * 60);
const POLL: Duration = Duration::from_secs(3);
const SSH_PORT: u16 = 22;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// sshd may answer before the user-data's login exists.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(2 * 60);
/// The login the user-data creates for this test.
const LOGIN_USER: &str = "e2e";
const DEFAULT_RUN_ROOT: &str = "/run/banlieue/ch";

struct Settings {
    provider: String,
    image: String,
    class: String,
    storage_dir: PathBuf,
    run_root: PathBuf,
    namespace: Option<String>,
}

fn settings() -> Settings {
    Settings {
        provider: required("BANLIEUE_E2E_PROVIDER"),
        image: required("BANLIEUE_E2E_IMAGE"),
        class: required("BANLIEUE_E2E_CLASS"),
        storage_dir: PathBuf::from(required("BANLIEUE_E2E_STORAGE_DIR")),
        run_root: PathBuf::from(
            std::env::var("BANLIEUE_E2E_RUN_ROOT").unwrap_or_else(|_| DEFAULT_RUN_ROOT.into()),
        ),
        namespace: std::env::var("BANLIEUE_E2E_NAMESPACE").ok(),
    }
}

#[tokio::test]
#[ignore = "needs a Cloud Hypervisor host running banlieue: see the module docs"]
async fn a_virtual_machine_boots_gets_an_address_and_leaves_nothing_behind() {
    let s = settings();
    let client = Client::try_default()
        .await
        .expect("a kubeconfig for the cluster (KUBECONFIG)");

    // Preconditions: fail with a clear message rather than time out later.
    let providers: Api<Provider> = Api::all(client.clone());
    let provider = providers
        .list(&Default::default())
        .await
        .expect("listing Providers")
        .into_iter()
        .find(|p| p.name_any() == s.provider)
        .unwrap_or_else(|| panic!("Provider {} not found", s.provider));
    let ns = s
        .namespace
        .clone()
        .or_else(|| provider.namespace())
        .expect("Provider namespace");
    assert!(
        provider
            .status
            .as_ref()
            .is_some_and(|st| condition_true(&st.conditions, "Ready")),
        "Provider {} is not Ready",
        s.provider
    );
    let image = Api::<VMImage>::all(client.clone())
        .get(&s.image)
        .await
        .unwrap_or_else(|e| panic!("VMImage {}: {e}", s.image));
    assert!(
        image.status.as_ref().is_some_and(|st| st
            .per_provider
            .iter()
            .any(|r| r.provider_name == s.provider && r.ready)),
        "VMImage {} is not ready for Provider {}",
        s.image,
        s.provider
    );

    let name = format!("e2e-ch-{}", std::process::id());
    let marker = format!("banlieue-e2e-{name}");
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &ns);
    let vms: Api<VirtualMachine> = Api::namespaced(client.clone(), &ns);
    let machines: Api<CloudHypervisorMachine> = Api::namespaced(client.clone(), &ns);

    let outcome = run(&s, &name, &marker, &cms, &vms, &machines).await;

    // Always clean up what this test created, whatever happened above.
    let _ = vms.delete(&name, &DeleteParams::default()).await;
    let _ = cms.delete(&name, &DeleteParams::default()).await;

    if let Err(e) = outcome {
        panic!("{e}");
    }
}

async fn run(
    s: &Settings,
    name: &str,
    marker: &str,
    cms: &Api<ConfigMap>,
    vms: &Api<VirtualMachine>,
    machines: &Api<CloudHypervisorMachine>,
) -> Result<(), String> {
    // A key for this run only, so the login proves the seed was applied.
    let keys = tempfile::tempdir().map_err(|e| e.to_string())?;
    let key = keys.path().join("id_ed25519");
    let made = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "banlieue-e2e", "-f"])
        .arg(&key)
        .status()
        .map_err(|e| format!("ssh-keygen: {e}"))?;
    if !made.success() {
        return Err(format!("ssh-keygen failed: {made}"));
    }
    let public = std::fs::read_to_string(key.with_extension("pub"))
        .map_err(|e| e.to_string())?
        .trim()
        .to_string();
    let cm: ConfigMap = serde_json::from_value(json!({
        "metadata": {"name": name},
        "data": {"user-data": format!(
            "#cloud-config\nhostname: {name}\nusers:\n  - name: {LOGIN_USER}\n    groups: [admin]\n    ssh_authorized_keys:\n      - {public}\nstages:\n  boot:\n    - name: e2e marker\n      files:\n        - path: /run/{marker}\n          content: \"{marker}\"\n          permissions: 0644\n"
        )},
    }))
    .map_err(|e| e.to_string())?;
    cms.create(&PostParams::default(), &cm)
        .await
        .map_err(|e| format!("creating ConfigMap: {e}"))?;
    let vm: VirtualMachine = serde_json::from_value(json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "VirtualMachine",
        "metadata": {"name": name},
        "spec": {
            "classRef": {"name": s.class},
            "imageRef": {"name": s.image},
            // The provider labels its failure domain `name=<Provider>`, so
            // this pins the VM to this host whatever the Provider's labels.
            "placement": {"failureDomainSelector": {"matchLabels": {"name": s.provider}}},
            "userData": {"configMapRef": {"name": name}},
            "desiredPowerState": "PoweredOn",
            "migrationPolicy": "never"
        }
    }))
    .map_err(|e| format!("VirtualMachine JSON: {e}"))?;
    vms.create(&PostParams::default(), &vm)
        .await
        .map_err(|e| format!("creating VirtualMachine: {e}"))?;

    // Ready with an address.
    let (uid, address, host_uid) = wait_for(
        POLL,
        "the machine to be Ready with an address",
        READY_TIMEOUT,
        || async {
            let m = machines.get_opt(name).await.ok()??;
            let st = m.status.as_ref()?;
            if !condition_true(&st.conditions, "Ready") {
                return None;
            }
            let addr = st.addresses.first()?.address.clone();
            Some((m.uid()?, addr, st.host_uid?))
        },
    )
    .await;
    println!("machine {uid} Ready at {address}");
    let provider_id = machines
        .get(name)
        .await
        .map_err(|e| e.to_string())?
        .spec
        .provider_id
        .unwrap_or_default();
    if !provider_id.ends_with(&uid) {
        return Err(format!(
            "spec.providerID {provider_id:?} does not name machine {uid}"
        ));
    }
    // The VirtualMachine mirrors it (ADR-0015's "status mirrors infra").
    wait_for(
        POLL,
        "the VirtualMachine to mirror Ready",
        READY_TIMEOUT,
        || async {
            let v = vms.get_opt(name).await.ok()??;
            condition_true(&v.status.as_ref()?.conditions, "Ready").then_some(())
        },
    )
    .await;

    // The guest is really up on the network: sshd answers.
    let ssh: SocketAddr = format!("{address}:{SSH_PORT}")
        .parse()
        .map_err(|e| format!("address {address:?}: {e}"))?;
    wait_for(POLL, "the guest's sshd", SSH_TIMEOUT, || async move {
        TcpStream::connect_timeout(&ssh, CONNECT_TIMEOUT)
            .ok()
            .map(drop)
    })
    .await;

    // The guest applied its user-data: the login it created accepts this
    // run's key, and the file it wrote holds the marker.
    let read = wait_for(
        POLL,
        "a login with the user-data's key",
        LOGIN_TIMEOUT,
        || {
            let key = key.clone();
            let target = format!("{LOGIN_USER}@{address}");
            let path = format!("/run/{marker}");
            async move {
                let out = Command::new("ssh")
                    .args(["-i"])
                    .arg(&key)
                    .args([
                        "-o",
                        "BatchMode=yes",
                        "-o",
                        "StrictHostKeyChecking=no",
                        "-o",
                        "UserKnownHostsFile=/dev/null",
                        "-o",
                        "ConnectTimeout=5",
                        "-o",
                        "LogLevel=ERROR",
                        &target,
                        "cat",
                        &path,
                    ])
                    .output()
                    .ok()?;
                out.status
                    .success()
                    .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
            }
        },
    )
    .await;
    if read != marker {
        return Err(format!(
            "the user-data's file holds {read:?}, not {marker:?}"
        ));
    }
    println!("logged in with the user-data's key; its file holds the marker");

    // On the host: unit, tap and directories exist.
    // An instance of the root-owned template, keyed by the guest's uid.
    let unit = format!("banlieue-ch@{host_uid}.service");
    let sd = Systemd::connect(Bus::System)
        .await
        .map_err(|e| format!("system bus: {e}"))?;
    let state = sd.state(&unit).await.map_err(|e| e.to_string())?;
    if state != Some(UnitState::Active) {
        return Err(format!(
            "{unit} is {state:?} on this host — is the test running on the Provider's host?"
        ));
    }
    let taps = taps_for(&uid);
    if taps.is_empty() || !taps.iter().all(|t| sys::interface_exists(t)) {
        return Err(format!("no tap for machine {uid} on this host"));
    }
    let machine_dir = s.storage_dir.join(&uid);
    let run_dir = s.run_root.join(host_uid.to_string());
    for dir in [&machine_dir, &run_dir] {
        if !present(dir) {
            return Err(format!("{} is missing while the guest runs", dir.display()));
        }
    }

    // Delete, and require that nothing is left.
    vms.delete(name, &DeleteParams::default())
        .await
        .map_err(|e| format!("deleting VirtualMachine: {e}"))?;
    wait_for(
        POLL,
        "the VirtualMachine and its machine to be gone",
        DELETE_TIMEOUT,
        || async {
            let vm_gone = vms.get_opt(name).await.ok()?.is_none();
            let m_gone = machines.get_opt(name).await.ok()?.is_none();
            (vm_gone && m_gone).then_some(())
        },
    )
    .await;
    let mut leaks = Vec::new();
    if let Some(st) = sd.state(&unit).await.map_err(|e| e.to_string())? {
        leaks.push(format!("unit {unit} ({st:?})"));
    }
    leaks.extend(taps_for(&uid).into_iter().map(|t| format!("tap {t}")));
    for dir in [&machine_dir, &run_dir] {
        if present(dir) {
            leaks.push(format!("directory {}", dir.display()));
        }
    }
    if !leaks.is_empty() {
        return Err(format!("deleting the VM left behind: {}", leaks.join(", ")));
    }
    println!("deleted; nothing left on the host");
    Ok(())
}
