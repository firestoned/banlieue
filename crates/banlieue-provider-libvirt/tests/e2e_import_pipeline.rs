// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Full ADR-0010 pipeline against a real cluster and a real libvirt host.
//!
//! The other suites each cover one half and neither covers the seam:
//!
//! - `banlieue-operator/tests/e2e_*` run against kind, which has no libvirt.
//! - `banlieue-libvirt/tests/live_libvirtd.rs` speaks the protocol to a real
//!   host, with no Kubernetes involved.
//!
//! What was left untested is the whole of it end to end — `VMImage` →
//! `OSArtifact` → artifacts PVC → one import Job per declared pool → a volume
//! on the host → per-zone status → the aggregate condition. Every property
//! asserted below was verified by hand first and cost real debugging to find;
//! this suite exists so none of them can regress silently.
//!
//! `#[ignore]`d so `cargo test` stays hermetic. Run it with:
//!
//! ```sh
//! export KUBECONFIG=~/dev/kubeconfig/homelab.yaml
//! BANLIEUE_E2E_PROVIDER=homelab-kvm \
//!   LIBVIRT_HOST=<host> LIBVIRT_TLS_DIR=~/.config/banlieue/libvirt \
//!   cargo test -p banlieue-provider-libvirt --test e2e_import_pipeline -- --ignored --nocapture
//! ```
//!
//! `LIBVIRT_HOST`/`LIBVIRT_TLS_DIR` are optional: without them the suite still
//! asserts everything Kubernetes can see and skips only the on-host volume
//! check. It assumes banlieue is already installed and the named `Provider`
//! reports `Ready` — it tests the pipeline, not the install.
//!
//! **Disk.** Each run creates a ~10Gi artifacts PVC, and with a node-local
//! provisioner that lands on the build node alongside the build container's
//! own multi-gigabyte scratch. A build node with too little ephemeral storage
//! evicts the build pod rather than failing it, which surfaces as
//! `ContainerStatusUnknown` and an `Error` OSArtifact with no log to read —
//! the reason is on the *pod*, as `Evicted: The node was low on resource:
//! ephemeral-storage`. Budget roughly 15Gi free, and note that PVCs from
//! previous runs persist until their `VMImage` is deleted.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use banlieue_api::banlieue::{
    Architecture, GuestAgent, ImageSourceKind, Provider, VMImage, VMImageSpec,
};
use banlieue_libvirt::{DEFAULT_TLS_PORT, TlsIdentity, connect_open, connect_tls};
use k8s_openapi::api::batch::v1::Job;
use kube::api::{Api, DeleteParams, ListParams, ObjectMeta, PostParams};
use kube::{Client, ResourceExt};

/// The build pulls an OCI image and writes a multi-gigabyte disk; on a homelab
/// host that is minutes, not seconds.
const PIPELINE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const POLL: Duration = Duration::from_secs(10);

/// Digest-pinned so the admission policy accepts it and the build is
/// reproducible (security review 2026-07-31).
const KAIROS_IMAGE: &str = "quay.io/kairos/ubuntu:24.04-standard-amd64-generic-v3.7.2-k0s-v1.34.3-k0s.0@sha256:e4860078c024269e81ce561ce91cf9639a4e75c23ea4cd32d3405005087192a7";

const IMAGE_NAME: &str = "e2e-import-pipeline";

fn provider_name() -> Option<String> {
    std::env::var("BANLIEUE_E2E_PROVIDER").ok()
}

fn libvirt_settings() -> Option<(String, PathBuf)> {
    let host = std::env::var("LIBVIRT_HOST").ok()?;
    let dir = std::env::var("LIBVIRT_TLS_DIR").ok()?;
    let dir = match (dir.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => dir,
    };
    Some((host, PathBuf::from(dir)))
}

fn load_identity(dir: &Path) -> TlsIdentity {
    let read = |name: &str| {
        std::fs::read(dir.join(name))
            .unwrap_or_else(|e| panic!("reading {}/{name}: {e}", dir.display()))
    };
    TlsIdentity {
        ca_pem: read("ca.pem"),
        client_cert_pem: read("client-cert.pem"),
        client_key_pem: read("client-key.pem"),
    }
}

async fn client() -> Client {
    Client::try_default()
        .await
        .expect("no reachable cluster — set KUBECONFIG")
}

/// Poll until `check` yields `Some`, reporting progress so a 20-minute build
/// does not look like a hang.
async fn wait_for<T, F, Fut>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + PIPELINE_TIMEOUT;
    let mut last = String::new();
    loop {
        if let Some(v) = check().await {
            eprintln!("  ✓ {what}");
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {}s waiting for: {what}",
            PIPELINE_TIMEOUT.as_secs()
        );
        if last != what {
            eprintln!("  … {what}");
            last = what.to_string();
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Storage pools the Provider **declares**, which is what imports must target.
///
/// Read from the spec, not from `status…raw["pools"]`: the latter is discovery
/// output listing every pool on the host, and importing into all of them writes
/// gigabytes into storage nobody declared.
fn declared_pools(provider: &Provider) -> BTreeSet<String> {
    let verified: BTreeSet<&str> = provider
        .status
        .as_ref()
        .map(|s| {
            s.failure_domains
                .iter()
                .flat_map(|fd| {
                    fd.attributes
                        .available_storage_classes
                        .iter()
                        .map(String::as_str)
                })
                .collect()
        })
        .unwrap_or_default();
    provider
        .spec
        .capabilities
        .storage_classes
        .iter()
        .filter(|c| verified.contains(c.name.as_str()))
        .filter_map(|c| c.target.get("pool").cloned())
        .collect()
}

/// Every pool the host actually has, from the Provider's own probe.
fn discovered_pools(provider: &Provider) -> BTreeSet<String> {
    provider
        .status
        .as_ref()
        .and_then(|s| s.failure_domains.first())
        .and_then(|fd| fd.attributes.raw.get("pools"))
        .map(|p| {
            p.split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn volumes_in_pool(host: &str, dir: &Path, pool_name: &str) -> Vec<String> {
    let identity = load_identity(dir);
    let mut session = connect_tls(host, DEFAULT_TLS_PORT, &identity)
        .await
        .expect("TLS to libvirt host");
    connect_open(&mut session, Some("qemu:///system"), false)
        .await
        .expect("CONNECT_OPEN");
    let pool = banlieue_libvirt::list_all_storage_pools(&mut session)
        .await
        .expect("listing pools")
        .into_iter()
        .find(|p| p.name == pool_name)
        .unwrap_or_else(|| panic!("pool {pool_name} not found on host"));
    banlieue_libvirt::storage_pool_list_all_volumes(&mut session, &pool)
        .await
        .expect("listing volumes")
        .into_iter()
        .map(|v| v.name)
        .collect()
}

#[tokio::test]
#[ignore = "full pipeline against a real cluster + libvirt host; set BANLIEUE_E2E_PROVIDER"]
async fn a_vmimage_is_built_once_and_imported_into_every_declared_pool() {
    let Some(provider_name) = provider_name() else {
        panic!("set BANLIEUE_E2E_PROVIDER to the name of a Ready libvirt Provider");
    };
    let client = client().await;

    let providers: Api<Provider> = Api::all(client.clone());
    let provider = providers
        .list(&ListParams::default())
        .await
        .expect("listing Providers")
        .into_iter()
        .find(|p| p.name_any() == provider_name)
        .unwrap_or_else(|| panic!("Provider {provider_name} not found"));

    let declared = declared_pools(&provider);
    let discovered = discovered_pools(&provider);
    assert!(
        !declared.is_empty(),
        "Provider {provider_name} declares no verified storage classes; nothing to import into"
    );
    eprintln!("  declared pools:   {declared:?}");
    eprintln!("  discovered pools: {discovered:?}");

    let images: Api<VMImage> = Api::all(client.clone());
    let _ = images.delete(IMAGE_NAME, &DeleteParams::default()).await;

    let spec = VMImageSpec {
        os_family: banlieue_api::banlieue::OsFamily::Linux,
        os_distribution: "ubuntu".to_string(),
        os_version: "24.04".to_string(),
        architecture: Architecture::Amd64,
        guest_agent: GuestAgent::CloudInit,
        sources: vec![banlieue_api::banlieue::ImageSource {
            provider_class: "libvirt".to_string(),
            kind: ImageSourceKind::Url,
            reference: "unused-for-url-sources".to_string(),
            import_from: Some(KAIROS_IMAGE.to_string()),
            checksum: None,
        }],
        cloud_config: None,
        template: None,
    };
    images
        .create(
            &PostParams::default(),
            &VMImage {
                metadata: ObjectMeta {
                    name: Some(IMAGE_NAME.to_string()),
                    ..Default::default()
                },
                spec,
                status: None,
            },
        )
        .await
        .expect("create VMImage");

    // ---- the imagebuilder half -----------------------------------------
    wait_for("banlieue-imagebuilder publishes a Ready build artifact", || {
        let api = images.clone();
        async move {
            let s = api.get_status(IMAGE_NAME).await.ok()?.status?;
            let a = s.build_artifact?;
            matches!(a.phase, banlieue_api::banlieue::BuildArtifactPhase::Ready).then_some(a)
        }
    })
    .await;

    // ---- the provider half ---------------------------------------------
    // Cluster-wide, deliberately: import Jobs run in the BUILD namespace
    // (ADR-0016), which is not the Provider's. Looking in the Provider's
    // namespace finds nothing and looks exactly like the provider never
    // reconciled.
    let jobs: Api<Job> = Api::all(client.clone());
    let import_jobs = wait_for("one import Job per declared pool", || {
        let api = jobs.clone();
        let want = declared.len();
        async move {
            let found: Vec<Job> = api
                .list(&ListParams::default().labels("app.kubernetes.io/component=libvirt-import"))
                .await
                .ok()?
                .into_iter()
                .filter(|j| j.name_any().contains(IMAGE_NAME))
                .collect();
            (found.len() == want).then_some(found)
        }
    })
    .await;

    // A Job per DECLARED pool — never per discovered pool.
    let targeted: BTreeSet<String> = import_jobs
        .iter()
        .filter_map(|j| j.labels().get("banlieue.io/pool").cloned())
        .collect();
    assert_eq!(
        targeted, declared,
        "imports must target exactly the declared pools"
    );
    for undeclared in discovered.difference(&declared) {
        assert!(
            !targeted.contains(undeclared),
            "{undeclared} exists on the host but was never declared — importing into it \
             writes gigabytes into storage nobody asked banlieue to use"
        );
    }

    // Placement follows the artifacts PVC, so the Job must NOT be pinned.
    for job in &import_jobs {
        let pod = &job.spec.as_ref().expect("job spec").template.spec;
        let selector = pod.as_ref().and_then(|p| p.node_selector.as_ref());
        assert!(
            selector.is_none_or(|s| s.is_empty()),
            "import Jobs must carry no nodeSelector — the scheduler resolves placement \
             from the bound PV's own affinity; got {selector:?}"
        );
    }

    wait_for("every import Job succeeds", || {
        let api = jobs.clone();
        async move {
            let all: Vec<Job> = api
                .list(&ListParams::default().labels("app.kubernetes.io/component=libvirt-import"))
                .await
                .ok()?
                .into_iter()
                .filter(|j| j.name_any().contains(IMAGE_NAME))
                .collect();
            all.iter()
                .all(|j| j.status.as_ref().and_then(|s| s.succeeded).unwrap_or(0) > 0)
                .then_some(())
        }
    })
    .await;

    // ---- status ---------------------------------------------------------
    let status = wait_for("aggregate Ready=True", || {
        let api = images.clone();
        async move {
            let s = api.get_status(IMAGE_NAME).await.ok()?.status?;
            s.conditions
                .iter()
                .any(|c| c.type_ == "Ready" && c.status == "True")
                .then_some(s)
        }
    })
    .await;

    let ready_zones: BTreeSet<String> = status
        .per_provider
        .iter()
        .flat_map(|r| r.zones.iter())
        .filter(|z| z.ready)
        .map(|z| z.name.clone())
        .collect();
    assert_eq!(
        ready_zones, declared,
        "one ready zone per declared pool, and no others"
    );

    // `buildArtifact` belongs to banlieue-imagebuilder's field manager and
    // `perProvider` to the provider's; both must survive on one object.
    assert!(
        status.build_artifact.is_some(),
        "the imagebuilder's buildArtifact must coexist with the provider's perProvider"
    );

    // ---- ground truth on the host ---------------------------------------
    if let Some((host, dir)) = libvirt_settings() {
        let volume = format!("{IMAGE_NAME}.raw");
        for pool in &declared {
            let vols = volumes_in_pool(&host, &dir, pool).await;
            assert!(
                vols.contains(&volume),
                "{volume} missing from declared pool {pool}: {vols:?}"
            );
            eprintln!("  ✓ {pool} holds {volume}");
        }
        for pool in discovered.difference(&declared) {
            let vols = volumes_in_pool(&host, &dir, pool).await;
            assert!(
                !vols.contains(&volume),
                "{volume} was written into UNDECLARED pool {pool}"
            );
            eprintln!("  ✓ {pool} (undeclared) untouched");
        }
    } else {
        eprintln!("  … skipping on-host volume check (LIBVIRT_HOST/LIBVIRT_TLS_DIR unset)");
    }

    images
        .delete(IMAGE_NAME, &DeleteParams::default())
        .await
        .expect("cleanup VMImage");
}
