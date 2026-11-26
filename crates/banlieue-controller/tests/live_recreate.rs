// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! A drift recreate converges, against a **real API server**.
//!
//! `migration_tests.rs` proves the decisions as pure functions: a drift
//! under `migrationPolicy: Automatic` recreates, the recreate records the
//! new placement, and the pass after it is in place. What only an API server
//! can show is that `virtualmachine::reconcile` acts on them in order: the
//! new placement persists through the status subresource **before** the old
//! infra CR goes, the apply waits while that CR is still terminating (a
//! finalizer holds it here, as a running provider's would), and once it is
//! gone the next pass creates the machine in the new failure domain and the
//! pass after leaves it alone.
//!
//! Before the fix the recreate left `status.scheduled` on the old placement,
//! so every pass saw the same drift and deleted again: the VM was destroyed
//! and never recreated. This suite fails on that code.
//!
//! A `libvirt`-class `Provider` fixture with two failure domains and a
//! hand-written status stands in for a provider; nothing runs behind it.
//!
//! ```sh
//! kind create cluster --name banlieue-recreate-test
//! kubectl --context kind-banlieue-recreate-test apply -f deploy/crds/
//! make recreate-live-test
//! ```
//!
//! Needs the banlieue CRDs installed and **no controller running**: the test
//! drives `virtualmachine::reconcile` itself, and a live controller would
//! race it. It works in a generated namespace and cluster-scoped objects
//! with a random suffix, and removes them on the way out.

mod common;

use std::sync::Arc;

use banlieue_api::banlieue::{Provider, VMClass, VMImage, VirtualMachine};
use banlieue_api::common::condition_types;
use banlieue_api::infrastructure::LibvirtMachine;
use banlieue_controller::context::Context;
use banlieue_controller::reconciler::virtualmachine;
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use kube::{Client, ResourceExt};
use serde_json::json;

use common::nonce;

const NETWORK_CLASS: &str = "lan";
const STORAGE_CLASS: &str = "gold";
const PROVIDER: &str = "kvm";
const INTERFACE: &str = "eth0";
const VM: &str = "drifter";
const FAILURE_DOMAINS: [&str; 2] = ["fd1", "fd2"];
/// A finalizer the test puts on the infra CR so its deletion stays pending,
/// as it does while a running provider tears the guest down.
const HOLD: &str = "test.banlieue.io/hold";
/// Reconcile passes allowed for one step to settle. A correct reconciler
/// needs one; a few more absorb a pass that only refreshes status.
const MAX_PASSES: usize = 4;

/// Connect, or fail loudly: an `#[ignore]`d test being run at all is a
/// request to talk to a cluster, so an unreachable one is a failure, never
/// a silent pass (see `live_claim.rs`).
async fn client() -> Client {
    let client = Client::try_default()
        .await
        .expect("no kubeconfig; point KUBECONFIG at a cluster with the banlieue CRDs");
    let api: Api<LibvirtMachine> = Api::all(client.clone());
    api.list(&Default::default()).await.unwrap_or_else(|e| {
        panic!(
            "cannot list LibvirtMachines: {e}\n\
             This suite needs the banlieue CRDs installed (kubectl apply -f deploy/crds/) \
             and no controller running."
        )
    });
    client
}

/// One test's world: a namespace holding a `Provider` with two failure
/// domains, plus a cluster-scoped `VMClass` and `VMImage`.
struct World {
    client: Client,
    ns: String,
    class: String,
    image: String,
}

impl World {
    async fn new(client: &Client) -> Self {
        let id = nonce();
        let world = Self {
            client: client.clone(),
            ns: format!("banlieue-recreate-{id}"),
            class: format!("recreate-test-{id}"),
            image: format!("recreate-test-{id}"),
        };
        world.create_namespace().await;
        world.create_class().await;
        world.create_image().await;
        world.create_provider().await;
        world.set_failure_domains(&FAILURE_DOMAINS).await;
        world
    }

    async fn create_namespace(&self) {
        let api: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(self.client.clone());
        api.create(
            &PostParams::default(),
            &serde_json::from_value(json!({
                "apiVersion": "v1", "kind": "Namespace", "metadata": { "name": self.ns },
            }))
            .unwrap(),
        )
        .await
        .expect("create namespace");
    }

    async fn create_class(&self) {
        let api: Api<VMClass> = Api::all(self.client.clone());
        let class: VMClass = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1", "kind": "VMClass",
            "metadata": { "name": self.class },
            "spec": {
                "hardware": {
                    "cpus": 1, "memoryMiB": 512,
                    "disks": [{ "name": "os", "sizeGiB": 10, "storageClass": STORAGE_CLASS }],
                },
                "network": { "interfaces": [
                    { "name": INTERFACE, "networkClass": NETWORK_CLASS, "ipam": {} },
                ]},
            },
        }))
        .expect("class fixture");
        api.create(&PostParams::default(), &class)
            .await
            .expect("create VMClass");
    }

    /// A `BackingFile` image reported ready for [`PROVIDER`]. The status is
    /// written by hand because no image controller is running.
    async fn create_image(&self) {
        let api: Api<VMImage> = Api::all(self.client.clone());
        let image: VMImage = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1", "kind": "VMImage",
            "metadata": { "name": self.image },
            "spec": {
                "osFamily": "linux", "osDistribution": "test", "osVersion": "1",
                "architecture": "amd64",
                "sources": [{ "providerClass": "libvirt", "kind": "BackingFile", "ref": "base.qcow2" }],
            },
        }))
        .expect("image fixture");
        api.create(&PostParams::default(), &image)
            .await
            .expect("create VMImage");
        api.patch_status(
            &self.image,
            &PatchParams::default(),
            &Patch::Merge(json!({ "status": { "perProvider": [{
                "providerName": PROVIDER, "providerNamespace": self.ns,
                "ready": true, "resolvedRef": "base.qcow2",
            }]}})),
        )
        .await
        .expect("patch VMImage status");
    }

    /// A libvirt-class `Provider`. Nothing runs behind it.
    async fn create_provider(&self) {
        let provider: Provider = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1", "kind": "Provider",
            "metadata": { "name": PROVIDER, "namespace": self.ns },
            "spec": {
                "providerClassRef": { "name": "libvirt" },
                "connection": { "endpoint": "qemu+tls://bar.foo.io/system" },
                "capabilities": {
                    "storageClasses": [{ "name": STORAGE_CLASS, "target": { "pool": "default" } }],
                    "networkClasses": [{ "name": NETWORK_CLASS, "target": { "network": "default" } }],
                },
            },
        }))
        .expect("provider fixture");
        self.providers()
            .create(&PostParams::default(), &provider)
            .await
            .expect("create Provider");
    }

    /// Set the `Provider`'s failure domains, each offering the class's
    /// storage and network. Dropping one is the drift this suite causes.
    async fn set_failure_domains(&self, names: &[&str]) {
        let domains: Vec<_> = names
            .iter()
            .map(|name| {
                json!({
                    "name": name,
                    "attributes": {
                        "availableStorageClasses": [STORAGE_CLASS],
                        "availableNetworkClasses": [NETWORK_CLASS],
                    },
                })
            })
            .collect();
        self.providers()
            .patch_status(
                PROVIDER,
                &PatchParams::default(),
                &Patch::Merge(json!({ "status": { "failureDomains": domains } })),
            )
            .await
            .expect("patch Provider status");
    }

    /// A VM with the default `migrationPolicy` (`Automatic`).
    async fn create_vm(&self) {
        let vm: VirtualMachine = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1", "kind": "VirtualMachine",
            "metadata": { "name": VM, "namespace": self.ns },
            "spec": {
                "classRef": { "name": self.class },
                "imageRef": { "name": self.image },
            },
        }))
        .expect("vm fixture");
        self.vms()
            .create(&PostParams::default(), &vm)
            .await
            .expect("create VirtualMachine");
    }

    /// One reconcile of the VM, read fresh so it sees what the apiserver
    /// holds.
    async fn reconcile(&self) {
        let vm = self.vms().get(VM).await.expect("get VM to reconcile");
        let ctx = Arc::new(Context::new(self.client.clone(), Some(self.ns.clone())));
        virtualmachine::reconcile(Arc::new(vm), ctx)
            .await
            .unwrap_or_else(|e| panic!("reconcile {}/{VM}: {e}", self.ns));
    }

    fn vms(&self) -> Api<VirtualMachine> {
        Api::namespaced(self.client.clone(), &self.ns)
    }

    fn machines(&self) -> Api<LibvirtMachine> {
        Api::namespaced(self.client.clone(), &self.ns)
    }

    fn providers(&self) -> Api<Provider> {
        Api::namespaced(self.client.clone(), &self.ns)
    }

    /// The VM's `status.scheduled.failureDomain`, if any.
    async fn scheduled_domain(&self) -> Option<String> {
        self.vms()
            .get(VM)
            .await
            .expect("get VM")
            .status
            .and_then(|s| s.scheduled)
            .map(|s| s.failure_domain)
    }

    /// The VM's `PlacementValid` condition reason, if any.
    async fn placement_reason(&self) -> Option<String> {
        self.vms()
            .get(VM)
            .await
            .expect("get VM")
            .status
            .and_then(|s| {
                s.conditions
                    .into_iter()
                    .find(|c| c.type_ == condition_types::PLACEMENT_VALID)
            })
            .map(|c| c.reason)
    }

    /// The infra CR: `(failureDomain, terminating, uid)`, or `None` when
    /// it does not exist.
    async fn machine(&self) -> Option<(String, bool, String)> {
        self.machines()
            .get_opt(VM)
            .await
            .expect("get LibvirtMachine")
            .map(|m| {
                (
                    m.spec.failure_domain.clone().unwrap_or_default(),
                    m.metadata.deletion_timestamp.is_some(),
                    m.uid().unwrap_or_default(),
                )
            })
    }

    /// Reconcile until the infra CR exists and is not terminating, or fail.
    async fn reconcile_until_machine(&self) -> (String, String) {
        for _ in 0..MAX_PASSES {
            self.reconcile().await;
            if let Some((domain, false, uid)) = self.machine().await {
                return (domain, uid);
            }
        }
        panic!(
            "no live LibvirtMachine after {MAX_PASSES} passes; scheduled {:?}, machine {:?}",
            self.scheduled_domain().await,
            self.machine().await
        );
    }

    async fn set_hold(&self, held: bool) {
        let finalizers: Vec<&str> = if held { vec![HOLD] } else { vec![] };
        self.machines()
            .patch(
                VM,
                &PatchParams::default(),
                &Patch::Merge(json!({ "metadata": { "finalizers": finalizers } })),
            )
            .await
            .expect("set the test finalizer");
    }

    /// Best-effort teardown: strip finalizers (nothing else will), then
    /// delete the namespace and the cluster-scoped fixtures.
    async fn teardown(&self) {
        let strip = Patch::Merge(json!({ "metadata": { "finalizers": [] } }));
        let _ = self
            .machines()
            .patch(VM, &PatchParams::default(), &strip)
            .await;
        let _ = self.vms().patch(VM, &PatchParams::default(), &strip).await;
        let api: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(self.client.clone());
        let _ = api.delete(&self.ns, &DeleteParams::background()).await;
        let _ = Api::<VMClass>::all(self.client.clone())
            .delete(&self.class, &DeleteParams::default())
            .await;
        let _ = Api::<VMImage>::all(self.client.clone())
            .delete(&self.image, &DeleteParams::default())
            .await;
    }
}

/// Run `body` against a fresh world, tearing it down even when it panics.
async fn with_world<F, Fut>(body: F)
where
    F: FnOnce(Arc<World>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let world = Arc::new(World::new(&client().await).await);
    let result = tokio::spawn(body(world.clone())).await;
    world.teardown().await;
    if let Err(e) = result {
        std::panic::resume_unwind(e.into_panic());
    }
}

/// The whole recreate, step by step, with the old infra CR held by a
/// finalizer while the new placement is already recorded.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs and no controller running"]
async fn a_drift_recreate_moves_the_machine_and_then_stays_put() {
    with_world(|w| async move {
        w.create_vm().await;
        let (first, first_uid) = w.reconcile_until_machine().await;
        let other = FAILURE_DOMAINS
            .iter()
            .find(|d| **d != first)
            .expect("two failure domains")
            .to_string();

        // Hold the machine, then take its failure domain away: drift.
        w.set_hold(true).await;
        w.set_failure_domains(&[&other]).await;

        // The recreate pass records the new placement and deletes the old
        // machine, which the hold keeps terminating.
        w.reconcile().await;
        assert_eq!(
            w.scheduled_domain().await.as_deref(),
            Some(other.as_str()),
            "the new placement must be recorded before the old machine goes"
        );
        assert_eq!(
            w.placement_reason().await.as_deref(),
            Some("FailureDomainChanged")
        );
        let (domain, terminating, uid) = w.machine().await.expect("still held");
        assert!(terminating, "the old machine is being deleted");
        assert_eq!(
            (domain.as_str(), uid.as_str()),
            (first.as_str(), first_uid.as_str())
        );

        // While it terminates, a pass neither re-deletes it in a loop nor
        // patches the new placement onto it.
        w.reconcile().await;
        let (domain, terminating, uid) = w.machine().await.expect("still held");
        assert!(terminating);
        assert_eq!(
            (domain.as_str(), uid.as_str()),
            (first.as_str(), first_uid.as_str()),
            "nothing may be applied onto a terminating machine"
        );
        assert_eq!(w.scheduled_domain().await.as_deref(), Some(other.as_str()));

        // Release it: the next pass creates the machine where it now belongs.
        w.set_hold(false).await;
        let (moved, moved_uid) = w.reconcile_until_machine().await;
        assert_eq!(moved, other, "recreated in the new failure domain");
        assert_ne!(moved_uid, first_uid, "a fresh machine, not the old one");

        // And it stays: a later pass does not delete it again.
        w.reconcile().await;
        let (domain, terminating, uid) = w.machine().await.expect("kept");
        assert!(!terminating, "a converged VM is not recreated again");
        assert_eq!((domain, uid), (other, moved_uid));
    })
    .await;
}
