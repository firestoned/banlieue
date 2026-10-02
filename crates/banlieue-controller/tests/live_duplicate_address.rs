// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Duplicate static-address blocking against a **real API server**
//! (ADR-0083).
//!
//! `address_conflict_tests.rs` proves every *decision* as a pure function.
//! What it cannot prove is that `virtualmachine::reconcile` acts on the
//! answer: that the blocked VM really gets `Ready=False reason=DuplicateAddress`
//! **and no infrastructure CR**, while the holder's reconcile really does
//! produce one, and that the block lifts once the holder is gone. Also that
//! `status.heldAddresses` really persists through the status subresource
//! (it is a new CRD field), and that a VM blocked *after* it was provisioned
//! keeps its infra CR's old address while a power-off still reaches it.
//!
//! A `libvirt`-class `Provider` fixture is created with a hand-written
//! status so the holder actually schedules and gets a `LibvirtMachine`.
//! Without that, "the blocked VM has no infra CR" would be true for the
//! wrong reason: nothing would have one. No libvirt host is contacted:
//! the provider is not running, so its machine is only ever a CR.
//!
//! ```sh
//! kind create cluster --name banlieue-address-test
//! kubectl --context kind-banlieue-address-test apply -f deploy/crds/
//! make address-live-test
//! ```
//!
//! Needs the banlieue CRDs installed and **no controller running**: the
//! test drives `virtualmachine::reconcile` itself, and a live controller
//! would race it. Every test works in generated namespaces and cluster-
//! scoped objects with a random suffix, and removes them on the way out.

use std::sync::Arc;

use banlieue_api::banlieue::{Provider, VMClass, VMImage, VirtualMachine};
use banlieue_api::common::{PowerState, condition_types};
use banlieue_api::infrastructure::LibvirtMachine;
use banlieue_controller::context::Context;
use banlieue_controller::reconciler::address_conflict::REASON_DUPLICATE_ADDRESS;
use banlieue_controller::reconciler::virtualmachine;
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams};
use kube::{Client, ResourceExt};
use serde_json::json;

/// The contested address (RFC 5737).
const ADDR: &str = "192.0.2.10";
/// A free address (RFC 5737).
const FREE_ADDR: &str = "192.0.2.11";
const NETWORK_CLASS: &str = "lan";
const STORAGE_CLASS: &str = "gold";
const PROVIDER: &str = "kvm";
const INTERFACE: &str = "eth0";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

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

fn nonce() -> String {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).unwrap();
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// One test's world: a namespace (or two) each holding a schedulable
/// `Provider`, plus a cluster-scoped `VMClass` and `VMImage`.
struct World {
    client: Client,
    namespaces: Vec<String>,
    class: String,
    image: String,
}

impl World {
    async fn new(client: &Client, namespace_count: usize) -> Self {
        let id = nonce();
        let world = Self {
            client: client.clone(),
            namespaces: (0..namespace_count)
                .map(|i| format!("banlieue-addr-{id}-{i}"))
                .collect(),
            class: format!("addr-test-{id}"),
            image: format!("addr-test-{id}"),
        };
        world.create_class().await;
        world.create_image().await;
        for ns in &world.namespaces {
            world.create_namespace(ns).await;
            world.create_provider(ns).await;
        }
        world
    }

    async fn create_namespace(&self, ns: &str) {
        let api: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(self.client.clone());
        api.create(
            &PostParams::default(),
            &serde_json::from_value(json!({
                "apiVersion": "v1", "kind": "Namespace", "metadata": { "name": ns },
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
        let per_provider: Vec<_> = self
            .namespaces
            .iter()
            .map(|ns| {
                json!({
                    "providerName": PROVIDER, "providerNamespace": ns,
                    "ready": true, "resolvedRef": "base.qcow2",
                })
            })
            .collect();
        api.patch_status(
            &self.image,
            &PatchParams::default(),
            &Patch::Merge(json!({ "status": { "perProvider": per_provider } })),
        )
        .await
        .expect("patch VMImage status");
    }

    /// A libvirt-class `Provider` with one failure domain offering the
    /// class's storage and network. Nothing runs behind it.
    async fn create_provider(&self, ns: &str) {
        let api: Api<Provider> = Api::namespaced(self.client.clone(), ns);
        let provider: Provider = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1", "kind": "Provider",
            "metadata": { "name": PROVIDER, "namespace": ns },
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
        api.create(&PostParams::default(), &provider)
            .await
            .expect("create Provider");
        api.patch_status(
            PROVIDER,
            &PatchParams::default(),
            &Patch::Merge(json!({ "status": { "failureDomains": [{
                "name": "fd1",
                "attributes": {
                    "availableStorageClasses": [STORAGE_CLASS],
                    "availableNetworkClasses": [NETWORK_CLASS],
                },
            }]}})),
        )
        .await
        .expect("patch Provider status");
    }

    /// Create a VM declaring `address` on [`INTERFACE`].
    async fn create_vm(&self, ns: &str, name: &str, address: &str) -> VirtualMachine {
        let vm: VirtualMachine = serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1", "kind": "VirtualMachine",
            "metadata": { "name": name, "namespace": ns },
            "spec": {
                "classRef": { "name": self.class },
                "imageRef": { "name": self.image },
                "networkOverrides": [
                    { "name": INTERFACE, "static": { "address": address, "prefix": 24 } },
                ],
            },
        }))
        .expect("vm fixture");
        Api::namespaced(self.client.clone(), ns)
            .create(&PostParams::default(), &vm)
            .await
            .expect("create VirtualMachine")
    }

    /// Run one reconcile of `ns/name` with a context of the given scope,
    /// reading the VM fresh so the reconcile sees what the apiserver holds.
    async fn reconcile(&self, ctx: &Arc<Context>, ns: &str, name: &str) {
        let vm = self.vms(ns).get(name).await.expect("get VM to reconcile");
        virtualmachine::reconcile(Arc::new(vm), ctx.clone())
            .await
            .unwrap_or_else(|e| panic!("reconcile {ns}/{name}: {e}"));
    }

    fn ctx(&self, namespace: Option<&str>) -> Arc<Context> {
        Arc::new(Context::new(
            self.client.clone(),
            namespace.map(str::to_string),
        ))
    }

    fn vms(&self, ns: &str) -> Api<VirtualMachine> {
        Api::namespaced(self.client.clone(), ns)
    }

    fn machines(&self, ns: &str) -> Api<LibvirtMachine> {
        Api::namespaced(self.client.clone(), ns)
    }

    /// The static address on a `LibvirtMachine`'s first NIC.
    async fn machine_address(&self, ns: &str, name: &str) -> String {
        let m = self
            .machines(ns)
            .get(name)
            .await
            .expect("get LibvirtMachine");
        m.spec
            .network
            .first()
            .and_then(|n| n.ipam.static_.as_ref())
            .map(|s| s.address.clone())
            .unwrap_or_default()
    }

    /// The addresses the VM's `status.heldAddresses` records.
    async fn held(&self, ns: &str, name: &str) -> Vec<String> {
        let vm = self.vms(ns).get(name).await.expect("get VM");
        vm.status
            .map(|s| s.held_addresses.into_iter().map(|h| h.address).collect())
            .unwrap_or_default()
    }

    /// `(reason, message)` of the VM's `Ready` condition.
    async fn ready(&self, ns: &str, name: &str) -> (String, String) {
        let vm = self.vms(ns).get(name).await.expect("get VM");
        vm.status
            .and_then(|s| {
                s.conditions
                    .into_iter()
                    .find(|c| c.type_ == condition_types::READY)
            })
            .map(|c| (c.reason, c.message))
            .unwrap_or_default()
    }

    /// Best-effort teardown: strip finalizers (nothing else will), then
    /// delete namespaces and the cluster-scoped fixtures.
    async fn teardown(&self) {
        let strip = Patch::Merge(json!({ "metadata": { "finalizers": [] } }));
        for ns in &self.namespaces {
            if let Ok(list) = self.vms(ns).list(&Default::default()).await {
                for vm in list {
                    let _ = self
                        .vms(ns)
                        .patch(&vm.name_any(), &PatchParams::default(), &strip)
                        .await;
                }
            }
            let api: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(self.client.clone());
            let _ = api.delete(ns, &DeleteParams::background()).await;
        }
        let _ = Api::<VMClass>::all(self.client.clone())
            .delete(&self.class, &DeleteParams::default())
            .await;
        let _ = Api::<VMImage>::all(self.client.clone())
            .delete(&self.image, &DeleteParams::default())
            .await;
    }
}

/// Run `body` against a fresh world, tearing it down even when it panics.
async fn with_world<F, Fut>(namespace_count: usize, body: F)
where
    F: FnOnce(Arc<World>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let world = Arc::new(World::new(&client().await, namespace_count).await);
    let result = tokio::spawn(body(world.clone())).await;
    world.teardown().await;
    if let Err(e) = result {
        std::panic::resume_unwind(e.into_panic());
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The holder schedules and gets a `LibvirtMachine`; the later VM on the
/// same address gets `DuplicateAddress` and **no** machine. A third VM on a
/// free address in the same world also gets a machine, so the block is the
/// address and nothing else.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs and no controller running"]
async fn later_vm_on_a_taken_address_is_blocked_and_gets_no_infra_cr() {
    with_world(1, |w| async move {
        let ns = w.namespaces[0].clone();
        let ctx = w.ctx(Some(&ns));
        w.create_vm(&ns, "holder", ADDR).await;
        w.create_vm(&ns, "wanter", ADDR).await;
        w.create_vm(&ns, "bystander", FREE_ADDR).await;

        for name in ["holder", "wanter", "bystander"] {
            w.reconcile(&ctx, &ns, name).await;
        }

        assert!(
            w.machines(&ns).get_opt("holder").await.unwrap().is_some(),
            "holder must be scheduled and get a LibvirtMachine; otherwise the \
             absence of one for `wanter` proves nothing (Ready: {:?})",
            w.ready(&ns, "holder").await
        );
        assert!(
            w.machines(&ns)
                .get_opt("bystander")
                .await
                .unwrap()
                .is_some(),
            "a free address must not be blocked"
        );
        assert_eq!(
            w.held(&ns, "holder").await,
            [ADDR],
            "status.heldAddresses must persist through the status subresource \
             (re-apply deploy/crds/ if this cluster has the old schema)"
        );

        let (reason, message) = w.ready(&ns, "wanter").await;
        assert_eq!(reason, REASON_DUPLICATE_ADDRESS, "message: {message}");
        assert!(message.contains(ADDR), "{message}");
        assert!(message.contains(&format!("{ns}/holder")), "{message}");
        assert!(
            w.machines(&ns).get_opt("wanter").await.unwrap().is_none(),
            "a blocked VM must never reach a provider"
        );
    })
    .await;
}

/// The block lifts once the holder is gone, and not before: a holder that
/// is terminating still holds, because its guest runs until its infra CR
/// clears.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs and no controller running"]
async fn deleting_the_holder_unblocks_the_later_vm() {
    with_world(1, |w| async move {
        let ns = w.namespaces[0].clone();
        let ctx = w.ctx(Some(&ns));
        w.create_vm(&ns, "holder", ADDR).await;
        w.create_vm(&ns, "wanter", ADDR).await;
        w.reconcile(&ctx, &ns, "holder").await;
        w.reconcile(&ctx, &ns, "wanter").await;
        assert_eq!(w.ready(&ns, "wanter").await.0, REASON_DUPLICATE_ADDRESS);

        // Delete the holder. Its controller finalizer keeps it Terminating
        // until its reconcile sees no infra CR.
        w.vms(&ns)
            .delete("holder", &DeleteParams::default())
            .await
            .expect("delete holder");
        w.reconcile(&ctx, &ns, "wanter").await;
        assert_eq!(
            w.ready(&ns, "wanter").await.0,
            REASON_DUPLICATE_ADDRESS,
            "a terminating holder still holds its address"
        );

        // Two finalize passes: the first deletes the LibvirtMachine (no
        // provider finalizer here, so it goes at once), the second sees it
        // gone and drops the VM's finalizer.
        w.reconcile(&ctx, &ns, "holder").await;
        w.reconcile(&ctx, &ns, "holder").await;
        assert!(
            w.vms(&ns).get_opt("holder").await.unwrap().is_none(),
            "holder should be fully deleted"
        );

        w.reconcile(&ctx, &ns, "wanter").await;
        assert_ne!(w.ready(&ns, "wanter").await.0, REASON_DUPLICATE_ADDRESS);
        assert!(
            w.machines(&ns).get_opt("wanter").await.unwrap().is_some(),
            "once unblocked, the VM proceeds to its infra CR"
        );
    })
    .await;
}

/// A cluster-scoped controller sees a holder in another namespace, and the
/// condition message does not name it.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs and no controller running"]
async fn holder_in_another_namespace_blocks_without_being_named() {
    with_world(2, |w| async move {
        let theirs = w.namespaces[0].clone();
        let ours = w.namespaces[1].clone();
        let ctx = w.ctx(None);
        w.create_vm(&theirs, "private-holder", ADDR).await;
        w.create_vm(&ours, "wanter", ADDR).await;
        w.reconcile(&ctx, &theirs, "private-holder").await;
        w.reconcile(&ctx, &ours, "wanter").await;

        let (reason, message) = w.ready(&ours, "wanter").await;
        assert_eq!(reason, REASON_DUPLICATE_ADDRESS, "message: {message}");
        assert!(
            !message.contains("private-holder") && !message.contains(&theirs),
            "must not leak another namespace's VM: {message}"
        );
        assert!(w.machines(&ours).get_opt("wanter").await.unwrap().is_none());
    })
    .await;
}

/// Review items #1 to #3 together. `newer` holds ADDR; `older`, already
/// running on FREE_ADDR, is edited onto ADDR. Holding comes from
/// `status.heldAddresses`, not from guest-reported addresses (there are none
/// here: no provider runs), so `older` is blocked despite its age. Its
/// `LibvirtMachine` keeps FREE_ADDR, and a power-off still reaches it.
#[tokio::test]
#[ignore = "needs a cluster with the banlieue CRDs and no controller running"]
async fn a_running_vm_edited_onto_a_held_address_is_blocked_but_not_frozen() {
    with_world(1, |w| async move {
        let ns = w.namespaces[0].clone();
        let ctx = w.ctx(Some(&ns));
        w.create_vm(&ns, "older", FREE_ADDR).await;
        w.create_vm(&ns, "newer", ADDR).await;
        w.reconcile(&ctx, &ns, "older").await;
        w.reconcile(&ctx, &ns, "newer").await;
        assert_eq!(w.machine_address(&ns, "older").await, FREE_ADDR);
        assert_eq!(w.held(&ns, "newer").await, [ADDR]);

        // Edit the older, running VM onto the address `newer` holds, and ask
        // for it to be powered off in the same edit.
        w.vms(&ns)
            .patch(
                "older",
                &PatchParams::default(),
                &Patch::Merge(json!({ "spec": {
                    "desiredPowerState": "PoweredOff",
                    "networkOverrides": [
                        { "name": INTERFACE, "static": { "address": ADDR, "prefix": 24 } },
                    ],
                }})),
            )
            .await
            .expect("edit older");
        w.reconcile(&ctx, &ns, "older").await;

        let (reason, message) = w.ready(&ns, "older").await;
        assert_eq!(reason, REASON_DUPLICATE_ADDRESS, "message: {message}");
        assert!(message.contains("already provisioned"), "{message}");
        assert_eq!(
            w.machine_address(&ns, "older").await,
            FREE_ADDR,
            "the contested address must never be applied to a running VM's infra CR"
        );
        assert_eq!(
            w.held(&ns, "older").await,
            [FREE_ADDR],
            "the guest is still on its old address, so it stays held"
        );
        let machine = w.machines(&ns).get("older").await.unwrap();
        assert_eq!(
            machine.spec.desired_power_state,
            PowerState::PoweredOff,
            "a blocked, provisioned VM must still be able to power off"
        );

        // The holder is untouched.
        w.reconcile(&ctx, &ns, "newer").await;
        assert_ne!(w.ready(&ns, "newer").await.0, REASON_DUPLICATE_ADDRESS);
        assert_eq!(w.machine_address(&ns, "newer").await, ADDR);
    })
    .await;
}
