// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::address_conflict`] (ADR-0083).
//!
//! Every decision the duplicate-address check makes is exercised here as a
//! pure function over synthetic VMs and classes. What these cannot prove is
//! that the reconcile acts on the answer (no infra CR for a blocked VM, power
//! still applied to a provisioned one); that is
//! `tests/live_duplicate_address.rs`, against a real API server.

#[cfg(test)]
mod tests {
    use banlieue_api::banlieue::{
        HardwareSpec, HeldAddress, MigrationPolicy, NetworkInterfaceOverride, NetworkInterfaceSpec,
        NetworkSpec, PlacementSpec, VMClass, VMClassSpec, VirtualMachine, VirtualMachineSpec,
        VirtualMachineStatus,
    };
    use banlieue_api::common::{
        Firmware, IpamShape, LocalObjectReference, MachineAddress, MachineAddressType, PowerState,
        StaticIpamConfig,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
    use k8s_openapi::jiff::Timestamp;
    use kube::ResourceExt;
    use kube::core::ObjectMeta;

    use super::super::*;

    const NS: &str = "team-a";
    const OTHER_NS: &str = "team-b";
    const LAN: &str = "lan";
    const DMZ: &str = "dmz";
    const ADDR: &str = "192.0.2.10";
    const ADDR2: &str = "192.0.2.11";
    const ADDR3: &str = "192.0.2.12";
    const OLDEST: i64 = 500;
    const OLDER: i64 = 1_000;
    const NEWER: i64 = 2_000;
    const NEWEST: i64 = 3_000;

    // --- Builders ------------------------------------------------------------

    /// A class named `name` with one interface per `(interface, networkClass)`.
    fn class(name: &str, nics: &[(&str, &str)]) -> VMClass {
        VMClass {
            metadata: ObjectMeta {
                name: Some(name.into()),
                ..Default::default()
            },
            spec: VMClassSpec {
                hardware: HardwareSpec {
                    cpus: 1,
                    memory_mi_b: 512,
                    disks: vec![],
                },
                network: NetworkSpec {
                    interfaces: nics
                        .iter()
                        .map(|(n, nc)| NetworkInterfaceSpec {
                            name: (*n).into(),
                            network_class: (*nc).into(),
                            ipam: IpamShape::default(),
                            mtu: None,
                        })
                        .collect(),
                },
                firmware: Firmware::Efi,
                features: vec![],
                tpm_enabled: false,
            },
        }
    }

    fn lan_class() -> VMClass {
        class("lan-class", &[("eth0", LAN)])
    }

    fn two_nic_class() -> VMClass {
        class("two-nic", &[("eth0", LAN), ("eth1", LAN)])
    }

    /// A VM on class `class_name`, created at `created` (unix seconds).
    fn vm(ns: &str, name: &str, class_name: &str, created: i64) -> VirtualMachine {
        VirtualMachine {
            metadata: ObjectMeta {
                name: Some(name.into()),
                namespace: Some(ns.into()),
                creation_timestamp: Some(Time(
                    Timestamp::from_second(created).expect("valid timestamp"),
                )),
                ..Default::default()
            },
            spec: VirtualMachineSpec {
                class_ref: LocalObjectReference {
                    name: class_name.into(),
                },
                image_ref: LocalObjectReference { name: "i".into() },
                placement: PlacementSpec::default(),
                desired_power_state: PowerState::PoweredOn,
                user_data: None,
                migration_policy: MigrationPolicy::Automatic,
                paused: false,
                network_overrides: Vec::new(),
                hardware_override: None,
                folder: None,
            },
            status: None,
        }
    }

    /// Declare `address` on `interface`.
    fn declares(mut v: VirtualMachine, interface: &str, address: &str) -> VirtualMachine {
        v.spec.network_overrides.push(NetworkInterfaceOverride {
            name: interface.into(),
            static_: StaticIpamConfig {
                address: address.into(),
                prefix: 24,
                gateway: None,
                nameservers: vec![],
                domain: None,
            },
        });
        v
    }

    /// Record that this VM's infra CR was applied with `address`.
    fn holds(
        mut v: VirtualMachine,
        interface: &str,
        network_class: &str,
        address: &str,
    ) -> VirtualMachine {
        v.status
            .get_or_insert_with(VirtualMachineStatus::default)
            .held_addresses
            .push(HeldAddress {
                interface: interface.into(),
                network_class: network_class.into(),
                address: address.into(),
            });
        v
    }

    /// A running VM: declares and holds `address` on eth0/lan.
    fn running(ns: &str, name: &str, created: i64, address: &str) -> VirtualMachine {
        holds(
            declares(vm(ns, name, "lan-class", created), "eth0", address),
            "eth0",
            LAN,
            address,
        )
    }

    fn blocked(v: &VirtualMachine, all: &[VirtualMachine], classes: &[VMClass]) -> bool {
        find_duplicate_address(v, all, classes).is_some()
    }

    // --- The basic block -----------------------------------------------------

    #[test]
    fn newer_vm_declaring_an_older_vms_address_is_blocked() {
        let classes = vec![lan_class()];
        let a = declares(vm(NS, "a", "lan-class", OLDER), "eth0", ADDR);
        let b = declares(vm(NS, "b", "lan-class", NEWER), "eth0", ADDR);
        let all = vec![a, b.clone()];

        let dup = find_duplicate_address(&b, &all, &classes).expect("b must be blocked");
        assert_eq!(dup.address, ADDR);
        assert_eq!(dup.network_class, LAN);
        assert_eq!(dup.interface, "eth0");
        assert_eq!(dup.holder, Holder::SameNamespace(format!("{NS}/a")));
    }

    #[test]
    fn older_vm_is_not_blocked_by_a_newer_claimant() {
        let classes = vec![lan_class()];
        let a = declares(vm(NS, "a", "lan-class", OLDER), "eth0", ADDR);
        let b = declares(vm(NS, "b", "lan-class", NEWER), "eth0", ADDR);
        let all = vec![a.clone(), b];

        assert!(!blocked(&a, &all, &classes));
    }

    #[test]
    fn distinct_addresses_do_not_conflict() {
        let classes = vec![lan_class()];
        let a = declares(vm(NS, "a", "lan-class", OLDER), "eth0", ADDR);
        let b = declares(vm(NS, "b", "lan-class", NEWER), "eth0", ADDR2);
        let all = vec![a, b.clone()];

        assert!(!blocked(&b, &all, &classes));
    }

    #[test]
    fn a_vm_never_conflicts_with_a_stale_copy_of_itself() {
        // The store may hold an older version of the VM being reconciled.
        let classes = vec![lan_class()];
        let current = running(NS, "a", OLDER, ADDR);
        let stale = declares(vm(NS, "a", "lan-class", OLDER), "eth0", ADDR);
        let all = vec![stale];

        assert!(!blocked(&current, &all, &classes));
    }

    #[test]
    fn vm_with_no_static_override_is_never_blocked() {
        let classes = vec![lan_class()];
        let holder = running(NS, "a", OLDER, ADDR);
        let dhcp = vm(NS, "b", "lan-class", NEWER);
        let all = vec![holder, dhcp.clone()];

        assert!(!blocked(&dhcp, &all, &classes));
    }

    // --- networkClass scoping ------------------------------------------------

    #[test]
    fn same_address_on_different_network_classes_is_allowed() {
        let classes = vec![lan_class(), class("dmz-class", &[("eth0", DMZ)])];
        let on_lan = declares(vm(NS, "a", "lan-class", OLDER), "eth0", ADDR);
        let on_dmz = declares(vm(NS, "b", "dmz-class", NEWER), "eth0", ADDR);
        let all = vec![on_lan, on_dmz.clone()];

        assert!(!blocked(&on_dmz, &all, &classes));
    }

    #[test]
    fn network_class_comes_from_the_overridden_interface_not_the_first() {
        let mixed = class("mixed", &[("eth0", DMZ), ("eth1", LAN)]);
        let classes = vec![lan_class(), mixed];
        let a = declares(vm(NS, "a", "lan-class", OLDER), "eth0", ADDR);
        let b = declares(vm(NS, "b", "mixed", NEWER), "eth1", ADDR);
        let all = vec![a, b.clone()];

        let dup = find_duplicate_address(&b, &all, &classes).expect("collides on lan");
        assert_eq!(dup.network_class, LAN);
        assert_eq!(dup.interface, "eth1");
    }

    #[test]
    fn override_naming_no_class_interface_is_ignored() {
        // Every infra builder drops such an override, so it claims nothing.
        let classes = vec![lan_class()];
        let a = declares(vm(NS, "a", "lan-class", OLDER), "eth0", ADDR);
        let b = declares(vm(NS, "b", "lan-class", NEWER), "nope", ADDR);
        let all = vec![a, b.clone()];

        assert!(!blocked(&b, &all, &classes));
        let c = declares(vm(NS, "c", "lan-class", NEWEST), "eth0", ADDR2);
        let with_stray = declares(vm(NS, "s", "lan-class", OLDEST), "nope", ADDR2);
        assert!(
            !blocked(&c, &[with_stray, c.clone()], &classes),
            "a stray override is not a claimant either"
        );
    }

    #[test]
    fn claimant_with_unknown_class_matches_any_network_class() {
        // Fail closed: an unresolvable claimant might be on our network.
        let classes = vec![lan_class()];
        let orphan = declares(vm(NS, "a", "deleted-class", OLDER), "eth0", ADDR);
        let b = declares(vm(NS, "b", "lan-class", NEWER), "eth0", ADDR);
        let all = vec![orphan, b.clone()];

        assert!(blocked(&b, &all, &classes));
    }

    // --- Cross-namespace -----------------------------------------------------

    #[test]
    fn collision_across_namespaces_is_detected_but_holder_is_not_named() {
        let classes = vec![lan_class()];
        let theirs = running(OTHER_NS, "secret-name", OLDER, ADDR);
        let ours = declares(vm(NS, "b", "lan-class", NEWER), "eth0", ADDR);
        let all = vec![theirs, ours.clone()];

        let dup = find_duplicate_address(&ours, &all, &classes).expect("blocked");
        assert_eq!(dup.holder, Holder::OtherNamespace);
        let msg = dup.message();
        assert!(
            !msg.contains("secret-name") && !msg.contains(OTHER_NS),
            "must not leak another tenant's VM name: {msg}"
        );
    }

    #[test]
    fn message_names_address_network_interface_and_same_namespace_holder() {
        let classes = vec![lan_class()];
        let a = running(NS, "a", OLDER, ADDR);
        let b = declares(vm(NS, "b", "lan-class", NEWER), "eth0", ADDR);
        let all = vec![a, b.clone()];

        let msg = find_duplicate_address(&b, &all, &classes)
            .expect("blocked")
            .message();
        for needle in [ADDR, LAN, "eth0", &format!("{NS}/a")] {
            assert!(msg.contains(needle), "missing {needle:?}: {msg}");
        }
    }

    // --- Incumbency: held addresses (review #1, #2, #7) ----------------------

    #[test]
    fn a_holder_keeps_its_address_while_its_guest_reports_none() {
        // Review #1: a rebooting libvirt guest, and every vSphere guest (#2),
        // report no status.addresses. Holding comes from heldAddresses, which
        // the controller wrote when it applied the infra CR, so the older
        // declarer stays blocked.
        let classes = vec![lan_class()];
        let older_declarer = declares(vm(NS, "a", "lan-class", OLDER), "eth0", ADDR);
        let newer_holder = running(NS, "b", NEWER, ADDR);
        let all = vec![older_declarer.clone(), newer_holder.clone()];

        let dup = find_duplicate_address(&older_declarer, &all, &classes)
            .expect("the held address is not available, however old the declarer");
        assert_eq!(dup.holder, Holder::SameNamespace(format!("{NS}/b")));
        assert!(!blocked(&newer_holder, &all, &classes));
    }

    #[test]
    fn guest_reported_addresses_play_no_part() {
        // An observed address is guest-influenced and transient; only
        // declared and held claims count.
        let classes = vec![lan_class()];
        let mut reporter = vm(NS, "a", "lan-class", OLDER);
        reporter.status = Some(VirtualMachineStatus {
            addresses: vec![MachineAddress {
                address_type: MachineAddressType::InternalIP,
                address: ADDR.into(),
            }],
            ..Default::default()
        });
        let b = declares(vm(NS, "b", "lan-class", NEWER), "eth0", ADDR);
        let all = vec![reporter, b.clone()];

        assert!(!blocked(&b, &all, &classes));
    }

    #[test]
    fn a_held_address_is_matched_on_its_network() {
        // Review #7: holding X on dmz says nothing about X on lan.
        let classes = vec![lan_class(), class("dmz-class", &[("eth0", DMZ)])];
        let dmz_holder = holds(
            declares(vm(NS, "a", "dmz-class", NEWER), "eth0", ADDR),
            "eth0",
            DMZ,
            ADDR,
        );
        let lan_declarer = declares(vm(NS, "b", "lan-class", OLDER), "eth0", ADDR);
        let all = vec![dmz_holder.clone(), lan_declarer.clone()];

        assert!(!blocked(&lan_declarer, &all, &classes));
        assert!(!blocked(&dmz_holder, &all, &classes));
    }

    #[test]
    fn an_older_running_vm_edited_onto_a_held_address_is_blocked() {
        let classes = vec![lan_class()];
        // a holds ADDR2, then is edited to declare ADDR, which b holds.
        let edited = holds(
            declares(vm(NS, "a", "lan-class", OLDER), "eth0", ADDR),
            "eth0",
            LAN,
            ADDR2,
        );
        let holder = running(NS, "b", NEWER, ADDR);
        let all = vec![edited.clone(), holder.clone()];

        assert!(blocked(&edited, &all, &classes));
        assert!(!blocked(&holder, &all, &classes));
    }

    #[test]
    fn a_blocked_edit_keeps_the_old_address_reserved() {
        // a's edit was withheld, so its guest is still on ADDR2; nobody else
        // may take ADDR2 until a's infra CR is re-applied without it.
        let classes = vec![lan_class()];
        let edited = holds(
            declares(vm(NS, "a", "lan-class", OLDER), "eth0", ADDR),
            "eth0",
            LAN,
            ADDR2,
        );
        let holder = running(NS, "b", NEWER, ADDR);
        let wants_old = declares(vm(NS, "c", "lan-class", OLDEST), "eth0", ADDR2);
        let all = vec![edited, holder, wants_old.clone()];

        let dup = find_duplicate_address(&wants_old, &all, &classes).expect("ADDR2 is held");
        assert_eq!(dup.holder, Holder::SameNamespace(format!("{NS}/a")));
    }

    #[test]
    fn of_two_vms_that_both_hold_an_address_the_older_keeps_it() {
        // Duplicates that predate ADR-0083, or a lost race.
        let classes = vec![lan_class()];
        let a = running(NS, "a", OLDER, ADDR);
        let b = running(NS, "b", NEWER, ADDR);
        let all = vec![a.clone(), b.clone()];

        assert!(!blocked(&a, &all, &classes));
        assert!(blocked(&b, &all, &classes));
    }

    #[test]
    fn a_terminating_vm_still_holds_its_address() {
        let classes = vec![lan_class()];
        let mut dying = running(NS, "a", NEWER, ADDR);
        dying.metadata.deletion_timestamp = Some(Time(Timestamp::now()));
        let b = declares(vm(NS, "b", "lan-class", OLDER), "eth0", ADDR);
        let all = vec![dying, b.clone()];

        assert!(blocked(&b, &all, &classes));
    }

    // --- One global order (review #4, #5) ------------------------------------

    #[test]
    fn two_vms_sharing_several_addresses_cannot_block_each_other() {
        // Review #4: B (older) and C (newer) both declare ADDR2 on eth1 and
        // ADDR3 on eth0; C holds ADDR3. Per-address winners would block each
        // other forever (C wins ADDR3, B wins ADDR2).
        let classes = vec![two_nic_class()];
        let b = declares(
            declares(vm(NS, "b", "two-nic", OLDER), "eth1", ADDR2),
            "eth0",
            ADDR3,
        );
        let c = holds(
            declares(
                declares(vm(NS, "c", "two-nic", NEWER), "eth1", ADDR2),
                "eth0",
                ADDR3,
            ),
            "eth0",
            LAN,
            ADDR3,
        );
        let all = vec![b.clone(), c.clone()];

        assert!(blocked(&b, &all, &classes), "ADDR3 is held by c");
        assert!(
            !blocked(&c, &all, &classes),
            "b is blocked, so it reserves nothing and c proceeds"
        );
    }

    #[test]
    fn a_blocked_vm_does_not_block_others_on_its_other_addresses() {
        // Review #5: a blocks b on ADDR; b also declares ADDR2; c declares
        // only ADDR2. b will never provision, so c must not wait on it.
        let classes = vec![two_nic_class()];
        let a = running(NS, "a", OLDEST, ADDR);
        let b = declares(
            declares(vm(NS, "b", "two-nic", OLDER), "eth0", ADDR),
            "eth1",
            ADDR2,
        );
        let c = declares(vm(NS, "c", "two-nic", NEWER), "eth1", ADDR2);
        let all = vec![a, b.clone(), c.clone()];

        assert!(blocked(&b, &all, &classes));
        assert!(!blocked(&c, &all, &classes));
    }

    #[test]
    fn equal_creation_times_break_ties_by_name() {
        let classes = vec![lan_class()];
        let a = declares(vm(NS, "a", "lan-class", OLDER), "eth0", ADDR);
        let b = declares(vm(NS, "b", "lan-class", OLDER), "eth0", ADDR);
        let all = vec![a.clone(), b.clone()];

        assert!(!blocked(&a, &all, &classes));
        assert!(blocked(&b, &all, &classes));
    }

    // --- Address normalisation ----------------------------------------------

    #[test]
    fn equivalent_ipv6_spellings_conflict() {
        let classes = vec![lan_class()];
        let a = declares(vm(NS, "a", "lan-class", OLDER), "eth0", "2001:db8::1");
        let b = declares(
            vm(NS, "b", "lan-class", NEWER),
            "eth0",
            "2001:0db8:0000::0001",
        );
        let all = vec![a, b.clone()];

        assert!(blocked(&b, &all, &classes));
    }

    #[test]
    fn surrounding_whitespace_does_not_hide_a_conflict() {
        let classes = vec![lan_class()];
        let a = declares(vm(NS, "a", "lan-class", OLDER), "eth0", ADDR);
        let b = declares(vm(NS, "b", "lan-class", NEWER), "eth0", " 192.0.2.10 ");
        let all = vec![a, b.clone()];

        assert!(blocked(&b, &all, &classes));
    }

    // --- held_addresses ------------------------------------------------------

    #[test]
    fn held_addresses_records_resolved_normalised_claims_only() {
        let v = declares(
            declares(vm(NS, "a", "lan-class", OLDER), "eth0", " 2001:0db8::1 "),
            "nope",
            ADDR,
        );

        assert_eq!(
            held_addresses(&v, &lan_class()),
            vec![HeldAddress {
                interface: "eth0".into(),
                network_class: LAN.into(),
                address: "2001:db8::1".into(),
            }]
        );
    }

    #[test]
    fn held_addresses_is_empty_for_a_dhcp_vm() {
        assert!(held_addresses(&vm(NS, "a", "lan-class", OLDER), &lan_class()).is_empty());
    }

    // --- contending_vms ------------------------------------------------------

    fn names(vms: &[VirtualMachine]) -> Vec<String> {
        let mut v: Vec<String> = vms.iter().map(ResourceExt::name_any).collect();
        v.sort();
        v
    }

    #[test]
    fn contenders_are_every_vm_transitively_sharing_an_address() {
        // v declares ADDR; w declares ADDR and ADDR2; u only HOLDS ADDR2 (an
        // edit away from it was withheld); z is unrelated.
        let v = declares(vm(NS, "v", "lan-class", OLDER), "eth0", ADDR);
        let w = declares(
            declares(vm(NS, "w", "two-nic", OLDER), "eth0", ADDR),
            "eth1",
            ADDR2,
        );
        let u = holds(
            declares(vm(OTHER_NS, "u", "lan-class", OLDER), "eth0", ADDR3),
            "eth0",
            LAN,
            ADDR2,
        );
        let z = declares(vm(NS, "z", "lan-class", OLDER), "eth0", "192.0.2.99");
        let store = [v.clone(), w, u, z];

        assert_eq!(names(&contending_vms(&v, store.iter())), ["u", "w"]);
    }

    #[test]
    fn a_vm_with_no_addresses_has_no_contenders() {
        let store = [declares(vm(NS, "a", "lan-class", OLDER), "eth0", ADDR)];
        assert!(contending_vms(&vm(NS, "b", "lan-class", NEWER), store.iter()).is_empty());
    }
}
