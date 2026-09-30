// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `ProxmoxMachine` reconciler.
//!
//! `converge` and `finalize_backend` take `&dyn ProxmoxApi`, so these drive
//! the real lifecycle against `FakeProxmox`, which refuses what Proxmox
//! refuses — no node, no TLS, no cluster.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::common::{
        Firmware, IpamSpec, LocalObjectReference, PowerState, StaticIpamConfig,
    };
    use banlieue_api::infrastructure::{
        ProxmoxMachineSpec, ProxmoxMachineStatus, ProxmoxNicModel, ProxmoxNicSpec,
    };
    use banlieue_proxmox::{FakeProxmox, GuestInterface, GuestIp, Params, ProxmoxApi};

    const NAME: &str = "web-1";
    const UID: &str = "0f3c9a1e-0000-4000-8000-000000000001";
    const TEMPLATE: u32 = 9000;

    fn machine() -> MachineRef<'static> {
        MachineRef {
            name: NAME,
            uid: UID,
        }
    }

    fn spec() -> ProxmoxMachineSpec {
        ProxmoxMachineSpec {
            provider_id: None,
            failure_domain: Some("pve-a-pve1".into()),
            provider_ref: LocalObjectReference {
                name: "pve-a".into(),
            },
            node: "pve1".into(),
            template_vmid: TEMPLATE,
            storage: "local-lvm".into(),
            pool: None,
            cores: 2,
            sockets: 1,
            memory_mi_b: 4096,
            cpu_type: None,
            firmware: Firmware::Bios,
            tpm_enabled: false,
            os_disk_size_gi_b: 20,
            data_disks: vec![],
            nics: vec![ProxmoxNicSpec {
                name: "eth0".into(),
                bridge: "vmbr0".into(),
                vlan: None,
                model: ProxmoxNicModel::Virtio,
                mac_address: None,
                ipam: IpamSpec::default(),
            }],
            iso_storage: None,
            user_data: None,
            desired_power_state: PowerState::PoweredOn,
        }
    }

    fn static_ipam() -> IpamSpec {
        IpamSpec {
            static_: Some(StaticIpamConfig {
                address: "192.0.2.10".into(),
                prefix: 24,
                gateway: Some("192.0.2.1".into()),
                nameservers: vec![],
                domain: None,
            }),
            ..Default::default()
        }
    }

    /// A node with a 10 GiB template.
    async fn world() -> FakeProxmox {
        let f = FakeProxmox::single_node("pve1");
        f.add_template("pve1", TEMPLATE, "tpl");
        f.set_vm_config(
            "pve1",
            TEMPLATE,
            &Params::new().set("scsi0", "local-lvm:vm-9000-disk-0,size=10G"),
        )
        .await
        .unwrap();
        f
    }

    /// One full reconcile pass against `api`, as `reconcile` composes it.
    async fn pass(
        api: &FakeProxmox,
        spec: &ProxmoxMachineSpec,
        recorded: Option<u32>,
    ) -> Result<Observed> {
        let alloc = allocate_vmid(api, &machine(), recorded).await?;
        converge(api, &machine(), spec, &alloc, true).await
    }

    /// A VM that belongs to the machine with `uid`: named `name`, marker set
    /// the way the clone sets it.
    async fn owned_vm(f: &FakeProxmox, id: u32, name: &str, uid: &str) {
        f.add_vm("pve1", id, name);
        f.set_vm_config(
            "pve1",
            id,
            &Params::new().set("description", ownership_marker(uid)),
        )
        .await
        .unwrap();
    }

    /// One reconcile pass for a machine with an arbitrary uid.
    async fn pass_as(
        api: &FakeProxmox,
        uid: &str,
        spec: &ProxmoxMachineSpec,
        recorded: Option<u32>,
    ) -> Result<Observed> {
        let m = MachineRef { name: NAME, uid };
        let alloc = allocate_vmid(api, &m, recorded).await?;
        converge(api, &m, spec, &alloc, true).await
    }

    fn vm_named(vms: &[banlieue_proxmox::ClusterVm], name: &str) -> usize {
        vms.iter()
            .filter(|v| v.name.as_deref() == Some(name))
            .count()
    }

    // ---- VMID allocation ---------------------------------------------

    #[tokio::test]
    async fn a_first_pass_allocates_the_next_free_vmid() {
        let f = world().await;
        let a = allocate_vmid(&f, &machine(), None).await.unwrap();
        assert_eq!(a, Allocation::Fresh { vmid: 100 });
    }

    #[tokio::test]
    async fn a_recorded_vmid_whose_vm_is_ours_is_adopted() {
        let f = world().await;
        owned_vm(&f, 105, NAME, UID).await;
        let a = allocate_vmid(&f, &machine(), Some(105)).await.unwrap();
        assert_eq!(
            a,
            Allocation::Existing {
                vmid: 105,
                node: "pve1".into()
            }
        );
    }

    /// The crash window: cloned, but died before status was patched.
    #[tokio::test]
    async fn a_cloned_vm_carrying_our_marker_is_adopted_not_duplicated() {
        let f = world().await;
        f.clone_vm(
            "pve1",
            TEMPLATE,
            &banlieue_proxmox::CloneParams::new(100)
                .name(NAME)
                .description(&ownership_marker(UID)),
        )
        .await
        .unwrap();
        let a = allocate_vmid(&f, &machine(), None).await.unwrap();
        assert_eq!(
            a,
            Allocation::Existing {
                vmid: 100,
                node: "pve1".into()
            }
        );
    }

    #[tokio::test]
    async fn a_recorded_vmid_taken_by_another_vm_is_abandoned() {
        let f = world().await;
        f.add_vm("pve1", 100, "somebody-elses");
        let a = allocate_vmid(&f, &machine(), Some(100)).await.unwrap();
        assert_eq!(a, Allocation::Fresh { vmid: 101 });
    }

    #[tokio::test]
    async fn a_recorded_vmid_with_no_vm_yet_is_reused() {
        let f = world().await;
        let a = allocate_vmid(&f, &machine(), Some(150)).await.unwrap();
        assert_eq!(a, Allocation::Fresh { vmid: 150 });
    }

    #[tokio::test]
    async fn a_template_with_the_machines_name_is_not_adopted() {
        let f = world().await;
        f.add_template("pve1", 200, NAME);
        let a = allocate_vmid(&f, &machine(), None).await.unwrap();
        assert!(matches!(a, Allocation::Fresh { .. }), "{a:?}");
    }

    // ---- create ------------------------------------------------------

    #[tokio::test]
    async fn create_clones_configures_grows_and_starts() {
        let f = world().await;
        let o = pass(&f, &spec(), None).await.unwrap();
        assert_eq!((o.vmid, o.node.as_str()), (100, "pve1"));
        assert_eq!(o.power, PowerState::PoweredOn);

        let cfg = f.vm_config("pve1", 100).await.unwrap();
        assert_eq!(cfg.get("cores"), Some("2"));
        assert_eq!(cfg.get("memory"), Some("4096"));
        assert_eq!(cfg.get("agent"), Some("1"));
        assert_eq!(cfg.get("net0"), Some("virtio,bridge=vmbr0"));
        assert!(cfg.get("scsi0").unwrap().contains("size=20G"), "{cfg:?}");
        let vms = f.cluster_vms().await.unwrap();
        assert_eq!(vm_named(&vms, NAME), 1);
        assert!(f.vm_status("pve1", 100).await.unwrap().is_running());
    }

    #[tokio::test]
    async fn the_template_is_left_untouched() {
        let f = world().await;
        pass(&f, &spec(), None).await.unwrap();
        let t = f.vm_config("pve1", TEMPLATE).await.unwrap();
        assert!(t.get("scsi0").unwrap().contains("size=10G"));
        assert_eq!(t.get("cores"), None);
    }

    #[tokio::test]
    async fn a_missing_template_names_the_vmid() {
        let f = FakeProxmox::single_node("pve1");
        let e = pass(&f, &spec(), None).await.unwrap_err();
        assert!(matches!(e, Error::Invalid { .. }), "{e}");
        assert!(e.to_string().contains("9000"), "{e}");
    }

    #[tokio::test]
    async fn a_template_vmid_that_is_a_live_guest_is_refused() {
        let f = FakeProxmox::single_node("pve1");
        f.add_vm("pve1", TEMPLATE, "live-guest");
        let e = pass(&f, &spec(), None).await.unwrap_err();
        assert!(e.to_string().contains("not a template"), "{e}");
        assert!(e.to_string().contains("9000"), "{e}");
        // Nothing was cloned.
        assert_eq!(f.cluster_vms().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_failed_clone_task_surfaces_and_leaves_no_vm() {
        let f = world().await;
        f.fail_next_task("storage is full");
        let e = pass(&f, &spec(), None).await.unwrap_err();
        assert!(e.to_string().contains("storage is full"), "{e}");
        assert_eq!(vm_named(&f.cluster_vms().await.unwrap(), NAME), 0);
    }

    #[tokio::test]
    async fn a_clone_onto_a_vmid_taken_since_allocation_is_refused() {
        let f = world().await;
        let alloc = allocate_vmid(&f, &machine(), None).await.unwrap();
        f.add_vm("pve1", 100, "raced-in");
        let e = converge(&f, &machine(), &spec(), &alloc, true)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("already exists"), "{e}");
    }

    #[tokio::test]
    async fn an_invalid_spec_is_refused_before_anything_is_cloned() {
        let f = world().await;
        let mut s = spec();
        s.user_data = Some("#cloud-config\n".into()); // no isoStorage
        let e = pass(&f, &s, None).await.unwrap_err();
        assert!(e.to_string().contains("isoStorage"), "{e}");
        assert_eq!(vm_named(&f.cluster_vms().await.unwrap(), NAME), 0);
    }

    #[tokio::test]
    async fn a_pool_and_a_different_target_node_reach_the_clone() {
        let f = world().await;
        f.add_node("pve2");
        let mut s = spec();
        s.node = "pve2".into();
        s.pool = Some("banlieue".into());
        let o = pass(&f, &s, None).await.unwrap();
        assert_eq!(o.node, "pve2");
        let vms = f.cluster_vms().await.unwrap();
        assert_eq!(vms.iter().find(|v| v.vmid == 100).unwrap().node, "pve2");
    }

    // ---- resize ------------------------------------------------------

    #[tokio::test]
    async fn the_os_disk_is_never_shrunk() {
        let f = FakeProxmox::single_node("pve1");
        f.add_template("pve1", TEMPLATE, "tpl");
        f.set_vm_config(
            "pve1",
            TEMPLATE,
            &Params::new().set("scsi0", "local-lvm:vm-9000-disk-0,size=30G"),
        )
        .await
        .unwrap();
        pass(&f, &spec(), None).await.unwrap();
        let cfg = f.vm_config("pve1", 100).await.unwrap();
        assert!(cfg.get("scsi0").unwrap().contains("size=30G"), "{cfg:?}");
    }

    #[tokio::test]
    async fn a_template_without_scsi0_fails_loudly() {
        let f = FakeProxmox::single_node("pve1");
        f.add_template("pve1", TEMPLATE, "tpl");
        let e = pass(&f, &spec(), None).await.unwrap_err();
        assert!(e.to_string().contains("scsi0"), "{e}");
    }

    // ---- seed --------------------------------------------------------

    fn seeded() -> ProxmoxMachineSpec {
        let mut s = spec();
        s.user_data = Some("#cloud-config\nhostname: web-1\n".into());
        s.iso_storage = Some("local".into());
        s
    }

    #[tokio::test]
    async fn user_data_is_uploaded_as_a_seed_iso_and_attached() {
        let f = world().await;
        pass(&f, &seeded(), None).await.unwrap();
        let vols = f.storage_content("pve1", "local").await.unwrap();
        let volid = format!("local:iso/banlieue-{UID}.iso");
        assert!(vols.iter().any(|v| v.volid == volid), "{vols:?}");
        let cfg = f.vm_config("pve1", 100).await.unwrap();
        assert_eq!(
            cfg.get("ide2"),
            Some(format!("{volid},media=cdrom").as_str())
        );
    }

    #[tokio::test]
    async fn a_seed_is_not_built_without_user_data_or_static_addressing() {
        let f = world().await;
        pass(&f, &spec(), None).await.unwrap();
        assert!(f.storage_content("pve1", "local").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn static_addressing_without_an_iso_storage_is_refused() {
        let f = world().await;
        let mut s = spec();
        s.nics[0].ipam = static_ipam();
        let e = pass(&f, &s, None).await.unwrap_err();
        assert!(e.to_string().contains("isoStorage"), "{e}");
    }

    #[tokio::test]
    async fn static_addressing_gets_a_seed_and_a_known_mac() {
        let f = world().await;
        let mut s = spec();
        s.nics[0].ipam = static_ipam();
        s.iso_storage = Some("local".into());
        pass(&f, &s, None).await.unwrap();
        let cfg = f.vm_config("pve1", 100).await.unwrap();
        let mac = crate::network::derive_mac(UID, "eth0");
        assert_eq!(
            cfg.get("net0"),
            Some(format!("virtio={mac},bridge=vmbr0").as_str())
        );
        assert_eq!(f.storage_content("pve1", "local").await.unwrap().len(), 1);
    }

    /// Roadmap 06: create-VM-then-delete leaves no orphaned seed.
    #[tokio::test]
    async fn delete_removes_the_vm_and_its_seed_leaving_nothing_behind() {
        let f = world().await;
        let o = pass(&f, &seeded(), None).await.unwrap();
        finalize_backend(&f, &machine(), &seeded(), Some(o.vmid))
            .await
            .unwrap();
        assert_eq!(vm_named(&f.cluster_vms().await.unwrap(), NAME), 0);
        assert!(f.storage_content("pve1", "local").await.unwrap().is_empty());
        // The template is somebody else's and stays.
        assert!(
            f.cluster_vms()
                .await
                .unwrap()
                .iter()
                .any(|v| v.vmid == TEMPLATE)
        );
    }

    #[tokio::test]
    async fn re_uploading_the_seed_on_a_reconfigure_leaves_one_volume() {
        let f = world().await;
        let o = pass(&f, &seeded(), None).await.unwrap();
        pass(&f, &seeded(), Some(o.vmid)).await.unwrap();
        assert_eq!(f.storage_content("pve1", "local").await.unwrap().len(), 1);
    }

    // ---- idempotence -------------------------------------------------

    #[tokio::test]
    async fn a_second_reconcile_changes_nothing() {
        let f = world().await;
        let first = pass(&f, &seeded(), None).await.unwrap();
        let before = f.vm_config("pve1", first.vmid).await.unwrap();
        let alloc = allocate_vmid(&f, &machine(), Some(first.vmid))
            .await
            .unwrap();
        assert!(matches!(alloc, Allocation::Existing { .. }));
        let second = converge(&f, &machine(), &seeded(), &alloc, false)
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(before, f.vm_config("pve1", first.vmid).await.unwrap());
        assert_eq!(vm_named(&f.cluster_vms().await.unwrap(), NAME), 1);
    }

    #[tokio::test]
    async fn a_reconfigure_pass_is_idempotent_too() {
        let f = world().await;
        let first = pass(&f, &spec(), None).await.unwrap();
        let again = pass(&f, &spec(), Some(first.vmid)).await.unwrap();
        assert_eq!(first, again);
    }

    #[tokio::test]
    async fn a_reconfigure_applies_a_changed_spec() {
        let f = world().await;
        let first = pass(&f, &spec(), None).await.unwrap();
        let mut s = spec();
        s.cores = 8;
        s.os_disk_size_gi_b = 40;
        pass(&f, &s, Some(first.vmid)).await.unwrap();
        let cfg = f.vm_config("pve1", first.vmid).await.unwrap();
        assert_eq!(cfg.get("cores"), Some("8"));
        assert!(cfg.get("scsi0").unwrap().contains("size=40G"));
    }

    // ---- power -------------------------------------------------------

    #[tokio::test]
    async fn powered_off_leaves_the_vm_stopped() {
        let f = world().await;
        let mut s = spec();
        s.desired_power_state = PowerState::PoweredOff;
        let o = pass(&f, &s, None).await.unwrap();
        assert_eq!(o.power, PowerState::PoweredOff);
        assert!(!f.vm_status("pve1", o.vmid).await.unwrap().is_running());
    }

    #[tokio::test]
    async fn powering_off_a_running_vm_and_back_on() {
        let f = world().await;
        let o = pass(&f, &spec(), None).await.unwrap();
        let mut off = spec();
        off.desired_power_state = PowerState::PoweredOff;
        let o2 = pass(&f, &off, Some(o.vmid)).await.unwrap();
        assert_eq!(o2.power, PowerState::PoweredOff);
        let o3 = pass(&f, &spec(), Some(o.vmid)).await.unwrap();
        assert_eq!(o3.power, PowerState::PoweredOn);
    }

    #[tokio::test]
    async fn a_failed_start_task_surfaces() {
        let f = world().await;
        let alloc = allocate_vmid(&f, &machine(), None).await.unwrap();
        // Clone succeeds; the start is the second task.
        let e = {
            // Fail the *next-next* task by failing after clone: drive manually.
            converge(&f, &machine(), &spec(), &alloc, true)
                .await
                .unwrap();
            f.stop_vm("pve1", 100).await.unwrap();
            f.fail_next_task("kvm: could not start");
            converge(
                &f,
                &machine(),
                &spec(),
                &Allocation::Existing {
                    vmid: 100,
                    node: "pve1".into(),
                },
                false,
            )
            .await
            .unwrap_err()
        };
        assert!(e.to_string().contains("could not start"), "{e}");
    }

    // ---- addresses ---------------------------------------------------

    #[tokio::test]
    async fn static_addresses_are_reported_from_ipam() {
        let f = world().await;
        let mut s = spec();
        s.nics[0].ipam = static_ipam();
        s.iso_storage = Some("local".into());
        let o = pass(&f, &s, None).await.unwrap();
        assert_eq!(o.address_source, Some(ProxmoxAddressSource::Static));
        assert_eq!(o.addresses.len(), 1);
        assert_eq!(o.addresses[0].address, "192.0.2.10");
    }

    fn iface(name: &str, addrs: &[(&str, &str)]) -> GuestInterface {
        GuestInterface {
            name: name.into(),
            hardware_address: None,
            ip_addresses: addrs
                .iter()
                .map(|(a, t)| GuestIp {
                    ip_address: (*a).into(),
                    ip_address_type: (*t).into(),
                    prefix: None,
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn guest_agent_addresses_skip_loopback_and_link_local() {
        let f = world().await;
        f.set_guest_interfaces(
            100,
            vec![
                iface("lo", &[("127.0.0.1", "ipv4"), ("::1", "ipv6")]),
                iface(
                    "eth0",
                    &[
                        ("192.0.2.20", "ipv4"),
                        ("fe80::1", "ipv6"),
                        ("169.254.1.1", "ipv4"),
                        ("2001:db8::20", "ipv6"),
                    ],
                ),
            ],
        );
        let o = pass(&f, &spec(), None).await.unwrap();
        let got: Vec<&str> = o.addresses.iter().map(|a| a.address.as_str()).collect();
        assert_eq!(got, ["192.0.2.20", "2001:db8::20"]);
        assert_eq!(o.address_source, Some(ProxmoxAddressSource::GuestAgent));
    }

    /// An agent that is not up yet is "no addresses yet", never a failure.
    #[tokio::test]
    async fn an_unavailable_agent_is_no_addresses_not_an_error() {
        let f = world().await;
        let first = pass(&f, &spec(), None).await.unwrap();
        // Take the agent away, as a template without one would.
        f.set_vm_config("pve1", first.vmid, &Params::new().set("delete", "agent"))
            .await
            .unwrap();
        let alloc = Allocation::Existing {
            vmid: first.vmid,
            node: "pve1".into(),
        };
        let o = converge(&f, &machine(), &spec(), &alloc, false)
            .await
            .unwrap();
        assert!(o.addresses.is_empty());
        assert_eq!(o.address_source, None);
    }

    #[tokio::test]
    async fn a_stopped_vm_reports_no_agent_addresses() {
        let f = world().await;
        let mut s = spec();
        s.desired_power_state = PowerState::PoweredOff;
        let o = pass(&f, &s, None).await.unwrap();
        assert!(o.addresses.is_empty());
    }

    // ---- delete ------------------------------------------------------

    #[tokio::test]
    async fn deleting_a_machine_that_was_never_realised_succeeds() {
        let f = world().await;
        finalize_backend(&f, &machine(), &spec(), None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn deleting_stops_a_running_vm_first() {
        let f = world().await;
        let o = pass(&f, &spec(), None).await.unwrap();
        assert!(f.vm_status("pve1", o.vmid).await.unwrap().is_running());
        finalize_backend(&f, &machine(), &spec(), Some(o.vmid))
            .await
            .unwrap();
        assert_eq!(vm_named(&f.cluster_vms().await.unwrap(), NAME), 0);
    }

    #[tokio::test]
    async fn delete_finds_an_unrecorded_vm_of_ours_by_name_and_marker() {
        let f = world().await;
        owned_vm(&f, 100, NAME, UID).await;
        finalize_backend(&f, &machine(), &spec(), None)
            .await
            .unwrap();
        assert_eq!(vm_named(&f.cluster_vms().await.unwrap(), NAME), 0);
    }

    #[tokio::test]
    async fn delete_never_touches_a_vm_that_is_not_ours() {
        let f = world().await;
        f.add_vm("pve1", 100, "somebody-elses");
        finalize_backend(&f, &machine(), &spec(), Some(100))
            .await
            .unwrap();
        assert_eq!(
            vm_named(&f.cluster_vms().await.unwrap(), "somebody-elses"),
            1
        );
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let f = world().await;
        let o = pass(&f, &seeded(), None).await.unwrap();
        finalize_backend(&f, &machine(), &seeded(), Some(o.vmid))
            .await
            .unwrap();
        finalize_backend(&f, &machine(), &seeded(), Some(o.vmid))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_failed_delete_task_is_an_error_so_the_finalizer_stays() {
        let f = world().await;
        let mut s = spec();
        s.desired_power_state = PowerState::PoweredOff;
        let o = pass(&f, &s, None).await.unwrap();
        f.fail_next_task("volume is busy");
        let e = finalize_backend(&f, &machine(), &s, Some(o.vmid))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("volume is busy"), "{e}");
        assert_eq!(vm_named(&f.cluster_vms().await.unwrap(), NAME), 1);
    }

    #[tokio::test]
    async fn a_seed_left_behind_by_a_vm_that_is_gone_is_still_removed() {
        let f = world().await;
        f.upload_iso("pve1", "local", &format!("banlieue-{UID}.iso"), vec![1])
            .await
            .unwrap();
        finalize_backend(&f, &machine(), &seeded(), None)
            .await
            .unwrap();
        assert!(f.storage_content("pve1", "local").await.unwrap().is_empty());
    }

    // ---- ownership (the machine UID, not the name) -------------------

    #[tokio::test]
    async fn the_clone_marks_the_vm_with_the_machine_uid() {
        let f = world().await;
        let o = pass(&f, &spec(), None).await.unwrap();
        let cfg = f.vm_config("pve1", o.vmid).await.unwrap();
        assert_eq!(cfg.get("description"), Some(ownership_marker(UID).as_str()));
    }

    /// Names are unique only per namespace and Proxmox allows duplicates: a
    /// same-named VM without our marker is somebody else's.
    #[tokio::test]
    async fn a_foreign_vm_with_the_same_name_is_not_adopted() {
        let f = world().await;
        f.add_vm("pve1", 100, NAME);
        let a = allocate_vmid(&f, &machine(), None).await.unwrap();
        assert_eq!(a, Allocation::Fresh { vmid: 101 });
        let o = pass(&f, &spec(), None).await.unwrap();
        assert_eq!(o.vmid, 101);
        assert_eq!(vm_named(&f.cluster_vms().await.unwrap(), NAME), 2);
        // The foreign VM was neither configured nor started.
        assert!(!f.vm_status("pve1", 100).await.unwrap().is_running());
        assert_eq!(f.vm_config("pve1", 100).await.unwrap().get("cores"), None);
    }

    #[tokio::test]
    async fn a_foreign_vm_with_the_same_name_survives_our_delete() {
        let f = world().await;
        f.add_vm("pve1", 100, NAME);
        let o = pass(&f, &spec(), None).await.unwrap();
        assert_ne!(o.vmid, 100, "status.vmid must not point at the foreign VM");
        finalize_backend(&f, &machine(), &spec(), Some(o.vmid))
            .await
            .unwrap();
        let vms = f.cluster_vms().await.unwrap();
        assert!(
            vms.iter().any(|v| v.vmid == 100),
            "foreign VM was destroyed"
        );
        assert!(
            !vms.iter().any(|v| v.vmid == o.vmid),
            "our VM should be gone"
        );
    }

    /// Delete with no recorded id must not fall back to "the VM with my name".
    #[tokio::test]
    async fn an_unrecorded_delete_never_destroys_a_same_named_foreign_vm() {
        let f = world().await;
        f.add_vm("pve1", 100, NAME);
        finalize_backend(&f, &machine(), &spec(), None)
            .await
            .unwrap();
        assert_eq!(vm_named(&f.cluster_vms().await.unwrap(), NAME), 1);
    }

    #[tokio::test]
    async fn a_stale_recorded_vmid_held_by_a_foreign_vm_is_left_alone() {
        let f = world().await;
        f.add_vm("pve1", 100, "somebody-elses");
        finalize_backend(&f, &machine(), &spec(), Some(100))
            .await
            .unwrap();
        assert_eq!(
            vm_named(&f.cluster_vms().await.unwrap(), "somebody-elses"),
            1
        );
    }

    #[tokio::test]
    async fn two_machines_with_the_same_name_and_different_uids_get_distinct_vms() {
        let f = world().await;
        let a = pass_as(&f, "uid-a", &spec(), None).await.unwrap();
        let b = pass_as(&f, "uid-b", &spec(), None).await.unwrap();
        assert_ne!(a.vmid, b.vmid);
        assert_eq!(vm_named(&f.cluster_vms().await.unwrap(), NAME), 2);

        // Each machine finds only its own VM, recorded or not.
        let ma = MachineRef {
            name: NAME,
            uid: "uid-a",
        };
        let found = allocate_vmid(&f, &ma, None).await.unwrap();
        assert_eq!(
            found,
            Allocation::Existing {
                vmid: a.vmid,
                node: "pve1".into()
            }
        );

        // Deleting one leaves the other.
        finalize_backend(&f, &ma, &spec(), Some(a.vmid))
            .await
            .unwrap();
        let vms = f.cluster_vms().await.unwrap();
        assert!(!vms.iter().any(|v| v.vmid == a.vmid));
        assert!(vms.iter().any(|v| v.vmid == b.vmid));
    }

    #[tokio::test]
    async fn a_uid_that_prefixes_another_does_not_collide() {
        let f = world().await;
        owned_vm(&f, 100, NAME, "abcd").await;
        let short = MachineRef {
            name: NAME,
            uid: "abc",
        };
        let a = allocate_vmid(&f, &short, None).await.unwrap();
        assert!(matches!(a, Allocation::Fresh { .. }), "{a:?}");
        // And the reverse: `abc`'s VM is not adopted by `abcd`... except its own.
        let long = MachineRef {
            name: NAME,
            uid: "abcd",
        };
        let a = allocate_vmid(&f, &long, None).await.unwrap();
        assert_eq!(
            a,
            Allocation::Existing {
                vmid: 100,
                node: "pve1".into()
            }
        );
        finalize_backend(&f, &short, &spec(), Some(100))
            .await
            .unwrap();
        assert_eq!(vm_named(&f.cluster_vms().await.unwrap(), NAME), 1);
    }

    #[tokio::test]
    async fn an_admin_editing_the_description_around_the_marker_does_not_orphan_the_vm() {
        let f = world().await;
        let o = pass(&f, &spec(), None).await.unwrap();
        let edited = format!(
            "Owned by the web team\n{}\nrenew in 2027",
            ownership_marker(UID)
        );
        f.set_vm_config("pve1", o.vmid, &Params::new().set("description", edited))
            .await
            .unwrap();
        let a = allocate_vmid(&f, &machine(), Some(o.vmid)).await.unwrap();
        assert_eq!(
            a,
            Allocation::Existing {
                vmid: o.vmid,
                node: "pve1".into()
            }
        );
        finalize_backend(&f, &machine(), &spec(), Some(o.vmid))
            .await
            .unwrap();
        assert_eq!(vm_named(&f.cluster_vms().await.unwrap(), NAME), 0);
    }

    #[tokio::test]
    async fn deleting_the_marker_line_orphans_the_vm_by_design() {
        let f = world().await;
        let o = pass(&f, &spec(), None).await.unwrap();
        f.set_vm_config("pve1", o.vmid, &Params::new().set("delete", "description"))
            .await
            .unwrap();
        let a = allocate_vmid(&f, &machine(), Some(o.vmid)).await.unwrap();
        assert!(matches!(a, Allocation::Fresh { .. }), "{a:?}");
        finalize_backend(&f, &machine(), &spec(), Some(o.vmid))
            .await
            .unwrap();
        assert_eq!(vm_named(&f.cluster_vms().await.unwrap(), NAME), 1);
    }

    /// Renaming is harmless: the recorded VMID still finds it.
    #[tokio::test]
    async fn renaming_our_vm_does_not_lose_it() {
        let f = world().await;
        let o = pass(&f, &spec(), None).await.unwrap();
        f.rename_vm(o.vmid, "renamed-by-an-admin");
        let a = allocate_vmid(&f, &machine(), Some(o.vmid)).await.unwrap();
        assert_eq!(
            a,
            Allocation::Existing {
                vmid: o.vmid,
                node: "pve1".into()
            }
        );
    }

    #[tokio::test]
    async fn our_vm_on_another_node_is_found_where_it_is() {
        let f = world().await;
        f.add_node("pve2");
        let mut s = spec();
        s.node = "pve2".into();
        let o = pass(&f, &s, None).await.unwrap();
        let a = allocate_vmid(&f, &machine(), Some(o.vmid)).await.unwrap();
        assert_eq!(
            a,
            Allocation::Existing {
                vmid: o.vmid,
                node: "pve2".into()
            }
        );
    }

    // ---- status ------------------------------------------------------

    fn observed(power: PowerState) -> Observed {
        Observed {
            vmid: 100,
            node: "pve1".into(),
            power,
            configured: true,
            addresses: vec![],
            address_source: None,
        }
    }

    #[test]
    fn provisioned_only_when_the_desired_power_state_is_reached() {
        let s = spec();
        let on = build_status(None, &s, &observed(PowerState::PoweredOn), 3);
        assert_eq!(on.initialization.provisioned, Some(true));
        let off = build_status(None, &s, &observed(PowerState::PoweredOff), 3);
        assert_eq!(off.initialization.provisioned, Some(false));

        let mut want_off = spec();
        want_off.desired_power_state = PowerState::PoweredOff;
        let st = build_status(None, &want_off, &observed(PowerState::PoweredOff), 3);
        assert_eq!(st.initialization.provisioned, Some(true));
    }

    #[test]
    fn an_unconfigured_vm_is_never_provisioned() {
        let mut o = observed(PowerState::PoweredOn);
        o.configured = false;
        let st = build_status(None, &spec(), &o, 1);
        assert_eq!(st.initialization.provisioned, Some(false));
    }

    #[test]
    fn status_records_vmid_node_power_and_generation() {
        let st = build_status(None, &spec(), &observed(PowerState::PoweredOn), 7);
        assert_eq!(st.vmid, Some(100));
        assert_eq!(st.node.as_deref(), Some("pve1"));
        assert_eq!(st.observed_power_state, Some(PowerState::PoweredOn));
        assert_eq!(st.observed_generation, Some(7));
        assert_eq!(st.failure_domain.as_deref(), Some("pve-a-pve1"));
        assert_eq!(st.tpm_attached, None);
    }

    #[test]
    fn tpm_attached_follows_the_spec() {
        let mut s = spec();
        s.tpm_enabled = true;
        let st = build_status(None, &s, &observed(PowerState::PoweredOn), 1);
        assert_eq!(st.tpm_attached, Some(true));
    }

    #[test]
    fn ready_is_true_only_when_provisioned() {
        let st = build_status(None, &spec(), &observed(PowerState::PoweredOn), 1);
        let ready = st.conditions.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(ready.status, "True");
        let st = build_status(None, &spec(), &observed(PowerState::PoweredOff), 1);
        let ready = st.conditions.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(ready.status, "False");
    }

    #[test]
    fn a_failure_keeps_what_was_known_and_does_not_advance_the_generation() {
        let prior = build_status(None, &spec(), &observed(PowerState::PoweredOn), 1);
        let st = failure_status(Some(&prior), "boom", 2);
        assert_eq!(st.vmid, Some(100));
        assert_eq!(st.observed_generation, Some(1));
        let ready = st.conditions.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(
            (ready.status.as_str(), ready.message.as_str()),
            ("False", "boom")
        );
        assert_eq!(ready.observed_generation, Some(2));
    }

    #[test]
    fn a_failure_with_no_prior_status_still_reports() {
        let st = failure_status(None, "boom", 1);
        assert_eq!(st.observed_generation, None);
        assert_eq!(st.conditions.len(), 1);
    }

    #[test]
    fn configuring_is_needed_until_a_pass_has_completed_for_this_generation() {
        let done = ProxmoxMachineStatus {
            observed_generation: Some(4),
            ..Default::default()
        };
        assert!(!needs_configure(Some(&done), 4));
        assert!(needs_configure(Some(&done), 5));
        assert!(needs_configure(Some(&ProxmoxMachineStatus::default()), 1));
        assert!(needs_configure(None, 1));
    }

    #[test]
    fn requeue_is_fast_only_while_waiting_for_addresses() {
        let s = spec();
        let mut o = observed(PowerState::PoweredOn);
        assert!(poll_soon(&s, &o));
        o.addresses = vec![banlieue_api::common::MachineAddress {
            address_type: banlieue_api::common::MachineAddressType::InternalIP,
            address: "192.0.2.20".into(),
        }];
        assert!(!poll_soon(&s, &o));
        // A VM meant to be off has no addresses to wait for.
        let mut want_off = spec();
        want_off.desired_power_state = PowerState::PoweredOff;
        assert!(!poll_soon(&want_off, &observed(PowerState::PoweredOff)));
    }
}
