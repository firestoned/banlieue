// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! End to end on a real Cloud Hypervisor host: a `tpmEnabled`
//! `CloudHypervisorMachine` gets its own swtpm, manufactured once as the
//! provider's user, boots with it, publishes the EK certificate the host
//! minted for `<machine-name>:<machine-uid>`, and deleting it leaves no TPM
//! unit behind (ADR-0065 Decisions 1, 2, 6).
//!
//! The machine is created **directly**, not through a `VirtualMachine`:
//! the controller refuses `tpmEnabled` with an `Immediate` image
//! (ADR-0048). This exercises the provider's vTPM, not sealing; the
//! `Deferred` install that seals is `tests/e2e_deferred.rs`.
//!
//! Runs **on the host**, like `e2e_machine.rs`: the units it checks are
//! there. `#[ignore]`d; a missing setting fails loudly.
//!
//! ```sh
//! export KUBECONFIG=~/.kube/<cluster>.yaml
//! BANLIEUE_E2E_PROVIDER=<provider>          # this host's Provider, declaring `vtpm`
//! BANLIEUE_E2E_BOOT_IMAGE=<file>            # a raw image in the class's cache
//! BANLIEUE_E2E_STORAGE_CLASS=<class>        # a host storage class name
//! BANLIEUE_E2E_NETWORK_CLASS=<class>        # a host network class name
//! BANLIEUE_E2E_STORAGE_DIR=/srv/banlieue/ch # that class's directory
//!   cargo test -p banlieue-provider-cloud-hypervisor --test e2e_vtpm -- --ignored --nocapture
//! ```
//!
//! Or `make ch-vtpm-e2e`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use banlieue_api::banlieue::Provider;
use banlieue_api::infrastructure::CloudHypervisorMachine;
use banlieue_provider_cloud_hypervisor::systemd::{Bus, Systemd, UnitState};
use banlieue_provider_sdk::ek::ek_cn_matches;
use kube::api::{Api, DeleteParams, PostParams};
use kube::{Client, ResourceExt};
use serde_json::json;

/// Manufacture, swtpm, then a boot to `Ready`.
const READY_TIMEOUT: Duration = Duration::from_secs(4 * 60);
const DELETE_TIMEOUT: Duration = Duration::from_secs(3 * 60);
const POLL: Duration = Duration::from_secs(3);
const OS_DISK_GIB: u32 = 20;
const MEMORY_MIB: u32 = 2048;
const FEATURE_VTPM: &str = "vtpm";

fn required(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is not set; see the module docs of tests/e2e_vtpm.rs"))
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

fn present(p: &Path) -> bool {
    match std::fs::symlink_metadata(p) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("stat {}: {e}", p.display()),
    }
}

#[tokio::test]
#[ignore = "needs a Cloud Hypervisor host with [tpm] running banlieue: see the module docs"]
async fn a_tpm_machine_gets_its_own_swtpm_and_a_host_minted_ek() {
    let provider_name = required("BANLIEUE_E2E_PROVIDER");
    let image = required("BANLIEUE_E2E_BOOT_IMAGE");
    let storage_class = required("BANLIEUE_E2E_STORAGE_CLASS");
    let network_class = required("BANLIEUE_E2E_NETWORK_CLASS");
    let storage_dir = PathBuf::from(required("BANLIEUE_E2E_STORAGE_DIR"));

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
    assert!(
        !provider
            .status
            .as_ref()
            .map(|s| s.ek_ca_certificates.clone())
            .unwrap_or_default()
            .is_empty(),
        "the host's EK CA is published"
    );

    let machines: Api<CloudHypervisorMachine> = Api::namespaced(client.clone(), &ns);
    let name = format!("e2e-vtpm-{}", std::process::id());
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
            "tpmEnabled": true,
        }
    }))
    .expect("a valid CloudHypervisorMachine");
    let created = machines
        .create(&PostParams::default(), &machine)
        .await
        .expect("creating the machine");
    let uid = created.uid().expect("a UID");
    println!("created {name} ({uid})");
    let systemd = Systemd::connect(Bus::System).await.expect("system bus");
    // Instances of the root-owned templates, keyed by the guest's uid, which
    // the provider records on the machine once it allocates one.
    let host_uid = std::sync::Mutex::new(None::<u32>);

    let outcome = async {
        let ready = wait_for("Ready with a vTPM", READY_TIMEOUT, || async {
            let m = machines.get(&name).await.ok()?;
            let st = m.status?;
            let ready = st
                .conditions
                .iter()
                .any(|c| c.type_ == "Ready" && c.status == "True");
            (ready && st.tpm_attached == Some(true) && !st.tpm_endorsement_certificates.is_empty())
                .then_some(st)
        })
        .await;
        let n = ready.host_uid.expect("a guest uid");
        *host_uid.lock().unwrap() = Some(n);
        println!(
            "Ready as guest uid {n}; {} EK certificate(s)",
            ready.tpm_endorsement_certificates.len()
        );
        for pem in &ready.tpm_endorsement_certificates {
            assert!(
                ek_cn_matches(pem, &name, &uid),
                "EK certificate CN is not {name}:{uid}"
            );
        }
        assert_eq!(
            systemd
                .state(&format!("banlieue-swtpm@{n}.service"))
                .await
                .expect("unit state"),
            Some(UnitState::Active),
            "swtpm runs while the guest does"
        );
        // A finished oneshot instance may stay loaded as inactive.
        let setup = systemd
            .state(&format!("banlieue-swtpm-setup@{n}.service"))
            .await
            .expect("unit state");
        assert!(
            matches!(setup, None | Some(UnitState::Inactive)),
            "manufacture finished: {setup:?}"
        );
    };
    // Delete whatever happened, then report: a failed check must not leave
    // a machine (and its TPM units) behind.
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
    }
    assert!(
        !present(&storage_dir.join(&uid)),
        "machine directory left behind"
    );
    println!("deleted; no TPM unit or state left");
    if let Err(panic) = checked {
        std::panic::resume_unwind(panic);
    }
}
