// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Tests for ADR-0101's 2026-11-04 amendment: a pass decides from a live
//! read of the machine, never from the watch cache, and a recorded `vmRef`
//! whose VM is gone is recovered by the ownership marker.
//!
//! The regression tests at the top replay the incident that motivated the
//! amendment: a second pass handed a cache snapshot that predates the first
//! pass's own status write destroyed the VM the first pass had just built.

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use banlieue_api::common::{
        DiskProvisioning, Firmware, IpamSpec, LocalObjectReference, PowerState,
    };
    use banlieue_api::infrastructure::{
        VSphereDiskSpec, VSphereMachine, VSphereMachineSpec, VSphereMachineStatus, VSphereNicSpec,
    };

    use crate::client::{FakeClient, FakeStep, Inventory, VSphereClient};
    use crate::error::Result;

    use super::super::{
        InitializationStatus, LiveBase, MachineRef, MachineSource, OwnClone, Route, ensure_vm,
        find_own_clone, live_base, route, status_adopting,
    };

    const UID: &str = "7c0d1f9e-0000-4000-8000-0000000000db";
    const OTHER_UID: &str = "00000000-0000-4000-8000-00000000beef";
    const NAMESPACE: &str = "banlieue-system";
    const NAME: &str = "db-01";

    // ---- fixtures ---------------------------------------------------------

    fn spec() -> VSphereMachineSpec {
        VSphereMachineSpec {
            provider_id: None,
            failure_domain: None,
            provider_ref: LocalObjectReference {
                name: "vc1".to_string(),
            },
            template: "ubuntu-22.04-cloudinit".to_string(),
            template_folder: None,
            datacenter: "dc1".to_string(),
            cluster: "cluster-a".to_string(),
            datastore: "ds-fast-01".to_string(),
            folder: None,
            resource_pool: None,
            num_cpus: 4,
            memory_mi_b: 8192,
            firmware: Firmware::Efi,
            tpm_enabled: false,
            disks: vec![VSphereDiskSpec {
                name: "os".to_string(),
                size_gi_b: 40,
                provisioning: DiskProvisioning::Thin,
            }],
            network: vec![VSphereNicSpec {
                name: "eth0".to_string(),
                port_group: "vmnet-prod".to_string(),
                mac_address: None,
                ipam: IpamSpec::default(),
            }],
            user_data: None,
            desired_power_state: PowerState::PoweredOn,
            created_by: None,
            first_boot_sealing: None,
        }
    }

    fn inventory() -> Inventory {
        Inventory::builder()
            .with_dc("dc1")
            .with_cluster("dc1", "cluster-a")
            .with_template("dc1", "ubuntu-22.04-cloudinit")
            .with_datastore("dc1", "cluster-a", "ds-fast-01", None)
            .with_network("dc1", "cluster-a", "vmnet-prod", false)
            .build()
    }

    fn machine(uid: &str, status: Option<VSphereMachineStatus>) -> VSphereMachine {
        let mut m = VSphereMachine::new(NAME, spec());
        m.metadata.namespace = Some(NAMESPACE.to_string());
        m.metadata.uid = Some(uid.to_string());
        m.status = status;
        m
    }

    /// What a successful pass writes: provisioned, with the VM it built.
    fn recorded(vm_ref: &str) -> VSphereMachineStatus {
        VSphereMachineStatus {
            initialization: InitializationStatus {
                provisioned: Some(true),
            },
            vm_ref: Some(vm_ref.to_string()),
            ..VSphereMachineStatus::default()
        }
    }

    fn me() -> MachineRef<'static> {
        MachineRef {
            name: NAME,
            uid: UID,
        }
    }

    fn as_client(c: &FakeClient) -> &dyn VSphereClient {
        c
    }

    /// An uncached read of the machine, standing in for the API server. The
    /// test sets what "the API server holds now" independently of the
    /// `cached` object it hands the pass, which is exactly the gap the
    /// incident lived in.
    struct FakeSource {
        live: Mutex<Option<VSphereMachine>>,
        reads: AtomicUsize,
    }

    impl FakeSource {
        fn new(live: Option<VSphereMachine>) -> Self {
            Self {
                live: Mutex::new(live),
                reads: AtomicUsize::new(0),
            }
        }

        fn set(&self, live: Option<VSphereMachine>) {
            *self.live.lock().expect("fake source lock") = live;
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl MachineSource for FakeSource {
        async fn read(&self, _namespace: &str, _name: &str) -> Result<Option<VSphereMachine>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(self.live.lock().expect("fake source lock").clone())
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum PassResult {
        /// The pass ended without acting (machine gone or replaced).
        Stopped,
        /// The pass saw a provisioned machine and only observed it; carries
        /// the `vmRef` it observed.
        Observed(Option<String>),
        /// The pass ran the create path and produced this VM.
        Created(String),
    }

    /// One reconcile pass, in the order `reconcile` takes it: read the
    /// machine live, route on *that* object, then create or observe. The
    /// `cached` object is what the controller's watch cache handed in; it
    /// decides nothing here.
    async fn one_pass(
        client: &FakeClient,
        source: &FakeSource,
        cached: &VSphereMachine,
    ) -> PassResult {
        let base = match live_base(source, cached).await.expect("live read") {
            LiveBase::Gone => return PassResult::Stopped,
            LiveBase::Use(machine) => machine,
        };
        match route(&base) {
            Route::Observe => PassResult::Observed(base.status.and_then(|s| s.vm_ref)),
            Route::Create { existing_vm_ref } => {
                let outcome = ensure_vm(
                    as_client(client),
                    &base.spec,
                    &me(),
                    existing_vm_ref.as_deref(),
                    None,
                )
                .await
                .expect("create path");
                PassResult::Created(outcome.vm_ref)
            }
        }
    }

    // ---- regression: the incident ----------------------------------------

    /// THE regression. Pass 1 provisions the machine. Pass 2 starts straight
    /// after it from a cache snapshot that does not yet contain pass 1's
    /// status write, but the API server does. Before the amendment pass 2
    /// took the marked VM for "a clone from an interrupted pass", destroyed
    /// it and cloned again, leaving `status.vmRef` naming a VM that no longer
    /// existed.
    #[tokio::test]
    async fn a_second_pass_from_a_stale_cache_does_not_destroy_the_vm_the_first_pass_built() {
        let client = FakeClient::new(inventory());
        // What the watch cache hands every pass: no status yet.
        let stale_cache = machine(UID, None);
        let api_server = FakeSource::new(Some(machine(UID, None)));

        let first = one_pass(&client, &api_server, &stale_cache).await;
        let PassResult::Created(vm) = first else {
            panic!("pass 1 must create the VM: {first:?}");
        };

        // Pass 1's status write has reached the API server; the cache has
        // not seen it.
        api_server.set(Some(machine(UID, Some(recorded(&vm)))));
        let second = one_pass(&client, &api_server, &stale_cache).await;

        assert_eq!(second, PassResult::Observed(Some(vm.clone())));
        assert!(
            client.destroyed_vms().is_empty(),
            "a healthy VM was destroyed: {:?}",
            client.destroyed_vms()
        );
        assert_eq!(client.cloned_vms().len(), 1, "a second clone was made");
        assert_eq!(client.live_vms(), vec![vm]);
    }

    /// The other half: the pass after that, again from a cache that lags one
    /// write behind, must build on the live status. Re-applying the stale
    /// `vmRef` over the newer one is how pass 3 undid pass 2.
    #[tokio::test]
    async fn a_pass_builds_on_the_live_status_not_the_stale_cached_one() {
        let source = FakeSource::new(Some(machine(UID, Some(recorded("vm-new")))));
        let stale_cache = machine(UID, Some(recorded("vm-destroyed")));

        let LiveBase::Use(base) = live_base(&source, &stale_cache).await.unwrap() else {
            panic!("the machine exists");
        };

        assert_eq!(
            base.status.and_then(|s| s.vm_ref).as_deref(),
            Some("vm-new"),
            "the pass must start from what the API server holds"
        );
    }

    /// Decision 3 is unchanged when the *live* object genuinely has no
    /// `vmRef`: a marked VM is a clone from an interrupted pass, destroyed
    /// and cloned afresh.
    #[tokio::test]
    async fn a_genuinely_interrupted_pass_is_still_reclaimed() {
        let client = FakeClient::new(inventory());
        let left_behind = client.add_vm(None, NAME, Some(UID));
        let cache = machine(UID, None);
        let api_server = FakeSource::new(Some(machine(UID, None)));

        let result = one_pass(&client, &api_server, &cache).await;

        let PassResult::Created(vm) = result else {
            panic!("the create path must run: {result:?}");
        };
        assert_eq!(client.destroyed_vms(), vec![left_behind]);
        assert_eq!(client.live_vms(), vec![vm]);
    }

    // ---- live_base --------------------------------------------------------

    #[tokio::test]
    async fn a_machine_that_is_gone_ends_the_pass() {
        let source = FakeSource::new(None);
        let result = live_base(&source, &machine(UID, None)).await.unwrap();
        assert!(matches!(result, LiveBase::Gone));
    }

    /// Deleted and recreated under the same name between the event and the
    /// read: the cached object describes a machine that no longer exists, and
    /// acting on its UID against the replacement would be acting on the
    /// wrong object. The replacement has its own event.
    #[tokio::test]
    async fn a_machine_replaced_under_the_same_name_ends_the_pass() {
        let source = FakeSource::new(Some(machine(OTHER_UID, None)));
        let result = live_base(&source, &machine(UID, None)).await.unwrap();
        assert!(matches!(result, LiveBase::Gone));
    }

    #[tokio::test]
    async fn the_live_read_happens_once_per_pass() {
        let source = FakeSource::new(Some(machine(UID, None)));
        live_base(&source, &machine(UID, None)).await.unwrap();
        assert_eq!(source.reads(), 1);
    }

    // ---- route ------------------------------------------------------------

    #[test]
    fn a_provisioned_machine_is_observed() {
        assert_eq!(route(&machine(UID, Some(recorded("vm-1")))), Route::Observe);
    }

    #[test]
    fn a_machine_with_no_status_takes_the_create_path_with_no_vm_ref() {
        assert_eq!(
            route(&machine(UID, None)),
            Route::Create {
                existing_vm_ref: None
            }
        );
    }

    /// A recorded `vmRef` on a machine not yet marked provisioned still goes
    /// to `ensure_vm`, which returns it untouched (the early return of
    /// `ensure_vm`); it is never cloned over.
    #[test]
    fn a_recorded_vm_ref_is_handed_to_the_create_path() {
        let status = VSphereMachineStatus {
            vm_ref: Some("vm-1".to_string()),
            ..VSphereMachineStatus::default()
        };
        assert_eq!(
            route(&machine(UID, Some(status))),
            Route::Create {
                existing_vm_ref: Some("vm-1".to_string())
            }
        );
    }

    // ---- find_own_clone: recovering a lost vmRef (Decision 7) -------------

    /// The state the incident left behind: the recorded VM is gone, and the
    /// live one carries the machine's marker.
    #[tokio::test]
    async fn exactly_one_marked_vm_is_adopted() {
        let client = FakeClient::new(inventory());
        let live = client.add_vm(None, NAME, Some(UID));

        let found = find_own_clone(as_client(&client), &spec(), &me())
            .await
            .unwrap();

        assert_eq!(found, OwnClone::Adopt(live));
    }

    #[tokio::test]
    async fn no_marked_vm_means_the_vm_is_really_gone() {
        let client = FakeClient::new(inventory());
        let found = find_own_clone(as_client(&client), &spec(), &me())
            .await
            .unwrap();
        assert_eq!(found, OwnClone::NotFound);
    }

    /// Ownership is the exact marker, never the name (Decisions 1 and 2).
    #[tokio::test]
    async fn a_same_named_vm_that_is_not_ours_is_never_adopted() {
        for foreign in [None, Some(OTHER_UID)] {
            let client = FakeClient::new(inventory());
            client.add_vm(None, NAME, foreign);
            let found = find_own_clone(as_client(&client), &spec(), &me())
                .await
                .unwrap();
            assert_eq!(found, OwnClone::NotFound, "{foreign:?}");
        }
    }

    #[tokio::test]
    async fn only_the_machines_own_folder_is_searched() {
        let client = FakeClient::new(inventory());
        let mut s = spec();
        s.folder = Some("team-a".to_string());
        client.add_vm(Some("team-b"), NAME, Some(UID));
        let ours = client.add_vm(Some("team-a"), NAME, Some(UID));

        let found = find_own_clone(as_client(&client), &s, &me()).await.unwrap();

        assert_eq!(found, OwnClone::Adopt(ours));
    }

    /// Two marked VMs cannot both be this machine's: report, never guess.
    #[tokio::test]
    async fn two_marked_vms_are_ambiguous_and_neither_is_adopted() {
        let client = FakeClient::new(inventory());
        let a = client.add_vm(None, NAME, Some(UID));
        let b = client.add_vm(None, NAME, Some(UID));

        let found = find_own_clone(as_client(&client), &spec(), &me())
            .await
            .unwrap();

        assert_eq!(found, OwnClone::Ambiguous(vec![a, b]));
    }

    /// A vCenter fault in the lookup is an error, not "nothing found": the
    /// caller reports `BackendMissing` and adopts nothing.
    #[tokio::test]
    async fn a_failed_lookup_is_an_error_not_an_empty_answer() {
        let client = FakeClient::new(inventory());
        client.add_vm(None, NAME, Some(UID));
        client.fail(FakeStep::FindVmsNamed);

        let result = find_own_clone(as_client(&client), &spec(), &me()).await;

        assert!(result.is_err(), "{result:?}");
    }

    #[tokio::test]
    async fn an_unknown_datacenter_finds_nothing() {
        let client = FakeClient::new(inventory());
        client.add_vm(None, NAME, Some(UID));
        let mut s = spec();
        s.datacenter = "no-such-dc".to_string();

        let found = find_own_clone(as_client(&client), &s, &me()).await.unwrap();

        assert_eq!(found, OwnClone::NotFound);
    }

    // ---- status_adopting --------------------------------------------------

    /// Adoption changes `vmRef` and nothing else: the apply re-sends the whole
    /// status, so a field dropped here would be retracted by SSA.
    #[test]
    fn adopting_replaces_only_the_vm_ref() {
        let mut current = recorded("vm-gone");
        current.guest_installed = Some(true);
        current.sealed_partitions = vec!["COS_PERSISTENT".to_string()];
        current.tpm_endorsement_certificates = vec!["pem".to_string()];

        let next = status_adopting(current.clone(), "vm-live");

        assert_eq!(next.vm_ref.as_deref(), Some("vm-live"));
        assert_eq!(
            VSphereMachineStatus {
                vm_ref: Some("vm-gone".to_string()),
                ..next
            },
            current
        );
    }
}
