// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! End to end on a real Cloud Hypervisor host: a `VirtualMachine` becomes a
//! running guest with an address, and deleting it leaves **nothing** behind
//! on the host (the leak test).
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

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
/// Tap names are `bch` + the first 10 hex digits of the machine UID + NIC.
const TAP_PREFIX: &str = "bch";
const TAP_UID_DIGITS: usize = 10;
const DEFAULT_RUN_ROOT: &str = "/run/banlieue/ch";

struct Settings {
    provider: String,
    image: String,
    class: String,
    storage_dir: PathBuf,
    run_root: PathBuf,
    namespace: Option<String>,
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is not set; see the module docs of tests/e2e_machine.rs")
    })
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

/// Poll `check` until it returns `Some`, or fail naming `what`.
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

fn condition_true(
    conditions: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition],
    kind: &str,
) -> bool {
    conditions
        .iter()
        .any(|c| c.type_ == kind && c.status == "True")
}

/// Host interfaces that are this machine's taps.
fn taps_for(uid: &str) -> Vec<String> {
    let hex: String = uid
        .chars()
        .filter(|c| *c != '-')
        .take(TAP_UID_DIGITS)
        .collect();
    let prefix = format!("{TAP_PREFIX}{hex}");
    std::fs::read_dir("/sys/class/net")
        .expect("reading /sys/class/net")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with(&prefix))
        .collect()
}

/// Whether `p` exists, without needing to read inside it (the directories
/// are `2770 guest:banlieue`; their `0711` parents let anyone stat them).
fn present(p: &Path) -> bool {
    match std::fs::symlink_metadata(p) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("stat {}: {e}", p.display()),
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
    let cm: ConfigMap = serde_json::from_value(json!({
        "metadata": {"name": name},
        "data": {"user-data": format!(
            "#cloud-config\nhostname: {name}\nstages:\n  boot:\n    - name: e2e marker\n      files:\n        - path: /run/{marker}\n          content: \"{marker}\"\n          permissions: 0644\n"
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
    wait_for("the guest's sshd", SSH_TIMEOUT, || async move {
        TcpStream::connect_timeout(&ssh, CONNECT_TIMEOUT)
            .ok()
            .map(drop)
    })
    .await;

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
