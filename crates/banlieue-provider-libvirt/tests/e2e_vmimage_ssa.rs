// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! e2e: the `VMImage.status` field-manager split of ADR-0010.
//!
//! Three controllers write one `VMImage.status`:
//!
//! - `banlieue.io/imagebuilder` owns `buildArtifact`
//! - `banlieue.io/provider-<backend>` owns its own `perProvider[]` rows
//! - the aggregate `Ready` condition is derived from all of them
//!
//! Every unit test asserts each controller *writes* its own field. None proves
//! they *coexist*, because coexistence is a property of server-side apply's
//! field ownership — not of the Rust types, and not of the CRD schema being
//! accepted. The failure mode is the expensive kind: the patch returns 200 and
//! the other manager's data is silently gone.
//!
//! `examples/04-vmimage-ubuntu.yaml` ships sources for vsphere, proxmox *and*
//! libvirt, so more than one provider reconciling the same `VMImage` is the
//! documented case, not a corner.
//!
//! ## What this file proves, and what it did not (ADR-0087)
//!
//! The first test applies as *different* managers and proves they coexist.
//! That is necessary but not sufficient: it says nothing about two writers
//! sharing **one** manager name, which is what the host-resident Cloud
//! Hypervisor provider (ADR-0060) does when every host applies as the bare
//! class constant. That case regressed in exactly the way ADR-0015 described,
//! went unnoticed for two months, and cost ~362,000 writes to one object
//! before anyone looked. The second test pins the mechanism so it cannot
//! regress a third time.
//!
//! `#[ignore]`d by default so `cargo test` stays hermetic. Run it with:
//!
//! ```sh
//! kubectl apply -f deploy/crds/banlieue.io_vmimages.yaml
//! cargo test -p banlieue-provider-libvirt --test e2e_vmimage_ssa -- --ignored --nocapture
//! ```

use banlieue_api::banlieue::{
    Architecture, GuestAgent, ImagePerProviderStatus, ImageSource, ImageSourceKind, OsFamily,
    VMImage, VMImageSpec,
};
use banlieue_provider_sdk::ssa::provider_field_manager;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::api::{Api, DeleteParams, ObjectMeta, Patch, PatchParams, PostParams};
use kube::{Client, Resource};
use serde_json::json;

const IMAGE_NAME: &str = "e2e-ssa-multi-backend";

const FM_IMAGEBUILDER: &str = "banlieue.io/imagebuilder";
const FM_VSPHERE: &str = "banlieue.io/provider-vsphere";
const FM_LIBVIRT: &str = "banlieue.io/provider-libvirt";
const FM_CONTROLLER: &str = "banlieue.io/controller";

async fn client() -> Client {
    Client::try_default()
        .await
        .expect("no reachable cluster — point KUBECONFIG at one with the VMImage CRD installed")
}

fn source(provider_class: &str, kind: ImageSourceKind, reference: &str) -> ImageSource {
    ImageSource {
        provider_class: provider_class.to_string(),
        kind,
        reference: reference.to_string(),
        import_from: None,
        checksum: None,
    }
}

/// Apply a `status` fragment as `manager`, forcing ownership exactly as the
/// reconcilers do.
async fn apply_status(api: &Api<VMImage>, manager: &str, status: serde_json::Value) {
    let patch = json!({
        "apiVersion": VMImage::api_version(&()).to_string(),
        "kind": VMImage::kind(&()).to_string(),
        "metadata": { "name": IMAGE_NAME },
        "status": status,
    });
    api.patch_status(
        IMAGE_NAME,
        &PatchParams::apply(manager).force(),
        &Patch::Apply(&patch),
    )
    .await
    .unwrap_or_else(|e| panic!("{manager} status apply failed: {e}"));
}

fn row(name: &str, ready: bool, reason: &str) -> ImagePerProviderStatus {
    ImagePerProviderStatus {
        provider_name: name.to_string(),
        provider_namespace: "banlieue-system".to_string(),
        ready,
        resolved_ref: None,
        reason: Some(reason.to_string()),
        message: None,
        zones: vec![],
    }
}

fn condition(status: &str, reason: &str, message: &str) -> Condition {
    Condition {
        type_: "Ready".to_string(),
        status: status.to_string(),
        reason: reason.to_string(),
        message: message.to_string(),
        last_transition_time: Time(k8s_openapi::jiff::Timestamp::now()),
        observed_generation: Some(1),
    }
}

#[tokio::test]
#[ignore = "requires a cluster with the VMImage CRD installed"]
async fn two_providers_and_the_imagebuilder_can_share_one_vmimage_status() {
    let client = client().await;
    let api: Api<VMImage> = Api::all(client.clone());

    let _ = api.delete(IMAGE_NAME, &DeleteParams::default()).await;

    let image = VMImage {
        metadata: ObjectMeta {
            name: Some(IMAGE_NAME.to_string()),
            ..Default::default()
        },
        spec: VMImageSpec {
            os_family: OsFamily::Linux,
            os_distribution: "ubuntu".to_string(),
            os_version: "22.04".to_string(),
            architecture: Architecture::Amd64,
            guest_agent: GuestAgent::CloudInit,
            // Exactly the shape examples/04-vmimage-ubuntu.yaml ships.
            sources: vec![
                source("vsphere", ImageSourceKind::Template, "ubuntu-22.04"),
                source("libvirt", ImageSourceKind::Url, "https://example.com/u.raw"),
            ],
            cloud_configs: vec![],
            template: None,
            iso_overlay: None,
            trusted_boot: None,
        },
        status: None,
    };
    api.create(&PostParams::default(), &image)
        .await
        .expect("create VMImage");

    // 1. banlieue-imagebuilder publishes the shared raw disk.
    apply_status(
        &api,
        FM_IMAGEBUILDER,
        json!({
            "buildArtifact": {
                "kind": "cloudImage",
                "phase": "Ready",
                "osArtifactRef": "build-1",
                "file": "u.raw",
            }
        }),
    )
    .await;

    // 2. The vSphere provider publishes its row, as its reconciler does:
    //    the whole `perProvider` list, containing only its own entry, and
    //    NOTHING in `conditions` (ADR-0015).
    apply_status(
        &api,
        FM_VSPHERE,
        json!({ "perProvider": [row("vc-1", true, "Reconciled")] }),
    )
    .await;

    // 3. The libvirt provider does the same for its own.
    apply_status(
        &api,
        FM_LIBVIRT,
        json!({ "perProvider": [row("kvm-1", false, "Importing")] }),
    )
    .await;

    // 4. banlieue-controller aggregates, owning `conditions` alone.
    apply_status(
        &api,
        FM_CONTROLLER,
        json!({ "conditions": [condition("False", "Importing", "1 of 2 provider(s) do not have this image")] }),
    )
    .await;

    // 5. A provider reconciles again. Its write must not disturb the
    //    controller's condition or the other provider's row.
    apply_status(
        &api,
        FM_LIBVIRT,
        json!({ "perProvider": [row("kvm-1", true, "Reconciled")] }),
    )
    .await;

    let got = api.get_status(IMAGE_NAME).await.expect("read back status");
    let status = got.status.expect("status is set");

    let names: Vec<&str> = status
        .per_provider
        .iter()
        .map(|r| r.provider_name.as_str())
        .collect();
    eprintln!("perProvider rows  : {names:?}");
    eprintln!(
        "conditions        : {:?}",
        status
            .conditions
            .iter()
            .map(|c| (&c.type_, &c.status, &c.message))
            .collect::<Vec<_>>()
    );
    eprintln!(
        "buildArtifact   : {:?}",
        status.build_artifact.as_ref().map(|a| &a.phase)
    );

    // The imagebuilder's field is a distinct key, so it is expected to survive.
    assert!(
        status.build_artifact.is_some(),
        "the imagebuilder's buildArtifact must not be clobbered by a provider"
    );

    // The original defect: providers erasing each other's rows.
    assert!(
        names.contains(&"vc-1") && names.contains(&"kvm-1"),
        "both providers' rows must coexist; got {names:?}. Without \
         x-kubernetes-list-type=map, server-side apply treats `perProvider` as \
         ATOMIC: one manager owns the whole array and force() hands it over \
         wholesale, discarding the other provider's rows (ADR-0015)."
    );

    // The libvirt row must reflect its *latest* write, not the first one —
    // merge-keyed means updated in place, not appended twice.
    let kvm: Vec<&_> = status
        .per_provider
        .iter()
        .filter(|r| r.provider_name == "kvm-1")
        .collect();
    assert_eq!(
        kvm.len(),
        1,
        "merge key must update in place, not duplicate"
    );
    assert!(kvm[0].ready, "the later write must win for its own row");

    // The controller's condition must survive a provider's subsequent write.
    assert_eq!(
        status.conditions.len(),
        1,
        "exactly one Ready condition, owned by the controller"
    );
    assert_eq!(
        status.conditions[0].message, "1 of 2 provider(s) do not have this image",
        "a provider write must not clobber the controller's aggregate"
    );

    api.delete(IMAGE_NAME, &DeleteParams::default())
        .await
        .expect("cleanup");
}

// ============================================================================
// ADR-0087: a field manager names a writer, not a class
// ============================================================================

const FM_CH_CLASS: &str = "banlieue.io/provider-cloud-hypervisor";

/// Two hosts of one provider class, each applying **only its own row** under
/// the **same** manager name, erase each other.
///
/// This is the mechanism, isolated. It is not a bug in the apiserver: a field
/// manager owns exactly the field set it last applied, so the second apply
/// legitimately removes the row the first one stopped declaring. The bug is
/// using one manager name for two writers.
///
/// Asserting the *broken* behaviour on purpose. If a future change makes this
/// test fail, SSA's ownership semantics have changed and ADR-0087's reasoning
/// needs rechecking, which is worth being told about.
#[tokio::test]
#[ignore = "requires a cluster with the VMImage CRD installed"]
async fn two_writers_sharing_one_manager_erase_each_other() {
    let client = client().await;
    let api: Api<VMImage> = Api::all(client.clone());
    let _ = api.delete(IMAGE_NAME, &DeleteParams::default()).await;
    create_image(&api).await;

    // Host A publishes its row.
    apply_status(
        &api,
        FM_CH_CLASS,
        json!({ "perProvider": [row("host-a", true, "Reconciled")] }),
    )
    .await;

    // Host B publishes its own, under the SAME manager.
    apply_status(
        &api,
        FM_CH_CLASS,
        json!({ "perProvider": [row("host-b", true, "Reconciled")] }),
    )
    .await;

    let names = per_provider_names(&api).await;
    assert_eq!(
        names,
        vec!["host-b"],
        "sharing a manager must lose host-a's row; if both survive, SSA \
         ownership semantics changed and ADR-0087 needs rechecking"
    );

    let _ = api.delete(IMAGE_NAME, &DeleteParams::default()).await;
}

/// The fix: the same two hosts, each applying only its own row, under
/// **per-writer** managers built by `provider_field_manager`. Both rows
/// survive, and neither host's re-reconcile disturbs the other.
#[tokio::test]
#[ignore = "requires a cluster with the VMImage CRD installed"]
async fn two_writers_with_scoped_managers_coexist() {
    let client = client().await;
    let api: Api<VMImage> = Api::all(client.clone());
    let _ = api.delete(IMAGE_NAME, &DeleteParams::default()).await;
    create_image(&api).await;

    let fm_a = provider_field_manager(FM_CH_CLASS, "banlieue-system", "host-a");
    let fm_b = provider_field_manager(FM_CH_CLASS, "banlieue-system", "host-b");
    assert_ne!(fm_a, fm_b, "the helper must produce distinct managers");

    apply_status(
        &api,
        &fm_a,
        json!({ "perProvider": [row("host-a", true, "Reconciled")] }),
    )
    .await;
    apply_status(
        &api,
        &fm_b,
        json!({ "perProvider": [row("host-b", true, "Reconciled")] }),
    )
    .await;

    let mut names = per_provider_names(&api).await;
    names.sort();
    assert_eq!(
        names,
        vec!["host-a", "host-b"],
        "per-writer managers must let both hosts' rows coexist"
    );

    // Host A reconciles again, flipping its own row. Host B's must be
    // untouched: this is the step whose failure produced the write loop,
    // because each host's removal woke the other.
    apply_status(
        &api,
        &fm_a,
        json!({ "perProvider": [row("host-a", false, "ImageNotFound")] }),
    )
    .await;

    let status = api
        .get_status(IMAGE_NAME)
        .await
        .expect("read back")
        .status
        .expect("status set");
    let rows = &status.per_provider;
    assert_eq!(rows.len(), 2, "re-reconcile must not drop a row: {rows:?}");
    let a = rows
        .iter()
        .find(|r| r.provider_name == "host-a")
        .expect("host-a");
    let b = rows
        .iter()
        .find(|r| r.provider_name == "host-b")
        .expect("host-b");
    assert!(!a.ready, "host-a's own update must land");
    assert!(b.ready, "host-b's row must survive host-a's apply");

    let _ = api.delete(IMAGE_NAME, &DeleteParams::default()).await;
}

/// Create the fixture image used by the ADR-0087 tests: a single
/// cloud-hypervisor source, which is the shape that triggered the loop.
async fn create_image(api: &Api<VMImage>) {
    let image = VMImage {
        metadata: ObjectMeta {
            name: Some(IMAGE_NAME.to_string()),
            ..Default::default()
        },
        spec: VMImageSpec {
            os_family: OsFamily::Linux,
            os_distribution: "debian".to_string(),
            os_version: "13".to_string(),
            architecture: Architecture::Amd64,
            guest_agent: GuestAgent::CloudInit,
            sources: vec![source(
                "cloud-hypervisor",
                ImageSourceKind::BackingFile,
                "base.raw",
            )],
            cloud_configs: vec![],
            template: None,
            iso_overlay: None,
            trusted_boot: None,
        },
        status: None,
    };
    api.create(&PostParams::default(), &image)
        .await
        .expect("create VMImage");
}

async fn per_provider_names(api: &Api<VMImage>) -> Vec<String> {
    api.get_status(IMAGE_NAME)
        .await
        .expect("read back status")
        .status
        .expect("status is set")
        .per_provider
        .iter()
        .map(|r| r.provider_name.clone())
        .collect()
}
