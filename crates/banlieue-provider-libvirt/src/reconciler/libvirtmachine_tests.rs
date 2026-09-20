// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `LibvirtMachine` reconciler.
//!
//! `reconcile` itself needs an API server, so what is covered here is
//! everything it delegates to: the pure mappings, and `converge` / `finalize`
//! driven through [`FakeMachineClient`]. Those two carry the ordering
//! guarantees that matter — volumes before the domain, destroy before
//! undefine, and a verified teardown.

#[cfg(test)]
mod tests {
    use super::super::*;
    use banlieue_api::common::{Firmware, IpamSpec, LocalObjectReference};
    use banlieue_api::infrastructure::{
        LibvirtBootSource, LibvirtBootSourceKind, LibvirtDiskBus, LibvirtDiskSpec,
        LibvirtNicSource, LibvirtNicSourceKind, LibvirtNicSpec,
    };
    use banlieue_libvirt::DomainIpAddr;

    use crate::machine_client::FakeMachineClient;

    const POOL: &str = "default";
    const SOURCE_VOLUME: &str = "kairos-base.img";

    fn spec(kind: LibvirtBootSourceKind) -> LibvirtMachineSpec {
        LibvirtMachineSpec {
            provider_id: None,
            failure_domain: Some("kvm-a-default".to_string()),
            provider_ref: LocalObjectReference {
                name: "kvm-a".to_string(),
            },
            pool: POOL.to_string(),
            domain_name: "sandboxes-agent-01".to_string(),
            boot_source: LibvirtBootSource {
                kind,
                volume: SOURCE_VOLUME.to_string(),
            },
            vcpus: 2,
            memory_mi_b: 2048,
            firmware: Firmware::Efi,
            machine_type: None,
            tpm_enabled: false,
            disks: vec![LibvirtDiskSpec {
                name: "os".to_string(),
                size_gi_b: 20,
                bus: LibvirtDiskBus::Virtio,
            }],
            network: vec![LibvirtNicSpec {
                name: "eth0".to_string(),
                source: LibvirtNicSource {
                    kind: LibvirtNicSourceKind::Network,
                    name: "default".to_string(),
                },
                model: None,
                mac_address: None,
                ipam: IpamSpec::default(),
            }],
            user_data: None,
            desired_power_state: PowerState::PoweredOn,
        }
    }

    /// A fake host that already has the pool and the imported source image —
    /// the state the VMImage reconciler leaves behind.
    fn ready_host() -> FakeMachineClient {
        let pool = StoragePool {
            name: POOL.to_string(),
            uuid: [7u8; 16],
        };
        let mut c = FakeMachineClient {
            pools: vec![pool.clone()],
            ..Default::default()
        };
        c.volumes.insert(
            (POOL.to_string(), SOURCE_VOLUME.to_string()),
            StorageVol {
                pool: POOL.to_string(),
                name: SOURCE_VOLUME.to_string(),
                key: format!("/var/lib/libvirt/images/{SOURCE_VOLUME}"),
            },
        );
        c
    }

    // ------------------------------------------------------------------
    // Pure mappings
    // ------------------------------------------------------------------

    #[test]
    fn volume_names_are_derived_from_the_domain_name() {
        assert_eq!(
            volume_name("sandboxes-agent-01", "os"),
            "sandboxes-agent-01-os.qcow2"
        );
    }

    /// The domain name is already namespace-qualified by the controller, so
    /// two namespaces' `db-01` disks cannot collide in one pool.
    #[test]
    fn volume_names_of_same_named_vms_in_different_namespaces_differ() {
        assert_ne!(
            volume_name("prod-db-01", "os"),
            volume_name("staging-db-01", "os")
        );
    }

    /// `ShuttingDown` is still executing. Reporting it off would tell a
    /// consumer it is safe to delete the disk out from under a live guest.
    #[test]
    fn shutting_down_is_still_powered_on() {
        assert_eq!(
            to_power_state(DomainState::ShuttingDown),
            PowerState::PoweredOn
        );
    }

    #[test]
    fn every_domain_state_maps_to_a_power_state() {
        for (state, want) in [
            (DomainState::Running, PowerState::PoweredOn),
            (DomainState::Blocked, PowerState::PoweredOn),
            (DomainState::Paused, PowerState::Suspended),
            (DomainState::PmSuspended, PowerState::Suspended),
            (DomainState::ShutOff, PowerState::PoweredOff),
            (DomainState::Crashed, PowerState::PoweredOff),
            (DomainState::NoState, PowerState::PoweredOff),
            (DomainState::Unknown(99), PowerState::PoweredOff),
        ] {
            assert_eq!(to_power_state(state), want, "{state:?}");
        }
    }

    #[test]
    fn address_sources_map_onto_the_crd_enum() {
        assert_eq!(
            to_address_source(InterfaceAddressSource::Agent),
            LibvirtAddressSource::GuestAgent
        );
        assert_eq!(
            to_address_source(InterfaceAddressSource::Lease),
            LibvirtAddressSource::DhcpLease
        );
        assert_eq!(
            to_address_source(InterfaceAddressSource::Arp),
            LibvirtAddressSource::ArpTable
        );
    }

    fn ifaces(addrs: &[&str]) -> Vec<DomainInterface> {
        vec![DomainInterface {
            name: "enp1s0".to_string(),
            hwaddr: None,
            addrs: addrs
                .iter()
                .map(|a| DomainIpAddr {
                    kind: 0,
                    addr: (*a).to_string(),
                    prefix: 24,
                })
                .collect(),
        }]
    }

    /// The guest agent reports `lo` on every guest. Leaving loopback in would
    /// make `127.0.0.1` the first address of every VM banlieue manages, and
    /// the first address is what most consumers take.
    #[test]
    fn loopback_and_link_local_are_not_machine_addresses() {
        let got = to_machine_addresses(&ifaces(&[
            "127.0.0.1",
            "127.1.2.3",
            "::1",
            "169.254.10.5",
            "fe80::5054:ff:fe00:1",
            "FE80::1",
            "192.0.2.24",
        ]));
        let addrs: Vec<&str> = got.iter().map(|a| a.address.as_str()).collect();
        assert_eq!(addrs, vec!["192.0.2.24"]);
    }

    /// `fec0::` is site-local, deprecated but routable, and explicitly *not*
    /// link-local — the `fe80::/10` test must not swallow it.
    #[test]
    fn site_local_ipv6_is_kept() {
        let got = to_machine_addresses(&ifaces(&["fec0::1"]));
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn addresses_are_reported_as_internal_ips() {
        let got = to_machine_addresses(&ifaces(&["192.0.2.24"]));
        assert_eq!(got[0].address_type, MachineAddressType::InternalIP);
    }

    #[test]
    fn formats_a_uuid_in_canonical_form() {
        let uuid = [
            0x0f, 0x3c, 0x9a, 0x1e, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x01,
        ];
        assert_eq!(format_uuid(&uuid), "0f3c9a1e-0000-4000-8000-000000000001");
    }

    // ------------------------------------------------------------------
    // converge
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn converge_creates_the_os_disk_then_defines_and_starts() {
        let mut c = ready_host();
        let s = spec(LibvirtBootSourceKind::InstallMedia);
        let observed = converge(&mut c, &s).await.expect("converge");

        assert_eq!(observed.domain.name, "sandboxes-agent-01");
        assert_eq!(observed.state, DomainState::Running);

        // Order is the assertion: a domain defined before its disk exists
        // points at nothing.
        let create = c
            .calls
            .iter()
            .position(|x| x == "create_volume:sandboxes-agent-01-os.qcow2")
            .expect("os disk created");
        let define = c
            .calls
            .iter()
            .position(|x| x == "define_domain:sandboxes-agent-01")
            .expect("domain defined");
        let start = c
            .calls
            .iter()
            .position(|x| x == "start_domain:sandboxes-agent-01")
            .expect("domain started");
        assert!(create < define, "{:?}", c.calls);
        assert!(define < start, "{:?}", c.calls);
    }

    /// A pool that does not notice a new file makes the *next* reconcile's
    /// lookup fail for no visible reason, so the refresh is not optional.
    #[tokio::test]
    async fn converge_refreshes_the_pool_after_creating_a_volume() {
        let mut c = ready_host();
        let s = spec(LibvirtBootSourceKind::InstallMedia);
        converge(&mut c, &s).await.expect("converge");
        assert!(
            c.calls.contains(&"refresh_pool:default".to_string()),
            "{:?}",
            c.calls
        );
    }

    /// Re-entering the reconciler must not recreate a disk that already
    /// exists — that would discard a running VM's data.
    #[tokio::test]
    async fn converge_is_idempotent_and_reuses_an_existing_disk() {
        let mut c = ready_host();
        let s = spec(LibvirtBootSourceKind::InstallMedia);
        converge(&mut c, &s).await.expect("first pass");
        let after_first = c.calls.len();
        converge(&mut c, &s).await.expect("second pass");

        // Count the OS disk specifically: a machine also creates a
        // cloud-init seed volume (ADR-0054), and counting every create
        // would conflate the two.
        let creates = c
            .calls
            .iter()
            .filter(|x| x.as_str() == "create_volume:sandboxes-agent-01-os.qcow2")
            .count();
        assert_eq!(creates, 1, "OS disk created twice: {:?}", c.calls);
        let seed_creates = c
            .calls
            .iter()
            .filter(|x| x.as_str() == "create_volume:sandboxes-agent-01-cidata.iso")
            .count();
        assert_eq!(seed_creates, 1, "seed created twice: {:?}", c.calls);
        assert!(c.calls.len() > after_first, "second pass did nothing");
    }

    /// A second pass over an already-running domain must not restart it.
    #[tokio::test]
    async fn converge_does_not_restart_a_running_domain() {
        let mut c = ready_host();
        let s = spec(LibvirtBootSourceKind::InstallMedia);
        converge(&mut c, &s).await.expect("first pass");
        converge(&mut c, &s).await.expect("second pass");
        let starts = c
            .calls
            .iter()
            .filter(|x| x.starts_with("start_domain:"))
            .count();
        assert_eq!(starts, 1, "{:?}", c.calls);
    }

    #[tokio::test]
    async fn converge_stops_a_domain_the_spec_wants_powered_off() {
        let mut c = ready_host();
        let mut s = spec(LibvirtBootSourceKind::InstallMedia);
        converge(&mut c, &s).await.expect("start it");

        s.desired_power_state = PowerState::PoweredOff;
        let observed = converge(&mut c, &s).await.expect("stop it");
        assert_eq!(observed.state, DomainState::ShutOff);
    }

    /// The boot source must already be on the host — importing it belongs to
    /// the VMImage reconciler, and a machine that silently proceeded without
    /// it would define a domain booting from nothing.
    #[tokio::test]
    async fn converge_fails_clearly_when_the_image_has_not_been_imported() {
        let mut c = FakeMachineClient {
            pools: vec![StoragePool {
                name: POOL.to_string(),
                uuid: [7u8; 16],
            }],
            ..Default::default()
        };
        let err = converge(&mut c, &spec(LibvirtBootSourceKind::InstallMedia))
            .await
            .expect_err("should fail");
        assert!(
            err.to_string().contains("has not finished importing"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn converge_fails_clearly_when_the_pool_is_missing() {
        let mut c = FakeMachineClient::default();
        let err = converge(&mut c, &spec(LibvirtBootSourceKind::InstallMedia))
            .await
            .expect_err("should fail");
        assert!(err.to_string().contains("storage pool"), "{err}");
    }

    #[tokio::test]
    async fn converge_creates_a_volume_per_data_disk() {
        let mut c = ready_host();
        let mut s = spec(LibvirtBootSourceKind::InstallMedia);
        s.disks.push(LibvirtDiskSpec {
            name: "data".to_string(),
            size_gi_b: 100,
            bus: LibvirtDiskBus::Virtio,
        });
        converge(&mut c, &s).await.expect("converge");
        assert!(
            c.calls
                .contains(&"create_volume:sandboxes-agent-01-data.qcow2".to_string()),
            "{:?}",
            c.calls
        );
    }

    /// No address yet is the normal state for a guest that is still
    /// installing — `None`, not an error, so the reconciler requeues.
    #[tokio::test]
    async fn converge_reports_no_addresses_while_the_guest_is_booting() {
        let mut c = ready_host();
        let observed = converge(&mut c, &spec(LibvirtBootSourceKind::InstallMedia))
            .await
            .expect("converge");
        assert!(observed.addresses.is_empty());
        assert!(observed.address_source.is_none());
    }

    #[tokio::test]
    async fn converge_reports_addresses_and_their_source() {
        let mut c = ready_host();
        c.addresses_by_source.insert(
            InterfaceAddressSource::Agent as u32,
            ifaces(&["127.0.0.1", "192.0.2.24"]),
        );
        let observed = converge(&mut c, &spec(LibvirtBootSourceKind::InstallMedia))
            .await
            .expect("converge");
        assert_eq!(observed.addresses.len(), 1);
        assert_eq!(observed.addresses[0].address, "192.0.2.24");
        assert_eq!(
            observed.address_source,
            Some(LibvirtAddressSource::GuestAgent)
        );
    }

    // ------------------------------------------------------------------
    // finalize
    // ------------------------------------------------------------------

    /// Destroy before undefine before volume deletion. Deleting a running
    /// domain's disk gives the guest I/O errors rather than a clean stop.
    #[tokio::test]
    async fn finalize_tears_down_in_the_only_safe_order() {
        let mut c = ready_host();
        let s = spec(LibvirtBootSourceKind::InstallMedia);
        converge(&mut c, &s).await.expect("converge");
        c.calls.clear();

        finalize_backend(&mut c, &s).await.expect("finalize");

        let destroy = c.calls.iter().position(|x| x.starts_with("destroy:"));
        let undefine = c.calls.iter().position(|x| x.starts_with("undefine:"));
        let delete = c.calls.iter().position(|x| x.starts_with("delete_volume:"));
        assert!(destroy < undefine, "{:?}", c.calls);
        assert!(undefine < delete, "{:?}", c.calls);
    }

    /// The boot source is a shared image owned by the VMImage reconciler.
    /// Deleting it with one machine would break every other VM using it.
    #[tokio::test]
    async fn finalize_never_deletes_the_shared_boot_source() {
        let mut c = ready_host();
        let s = spec(LibvirtBootSourceKind::InstallMedia);
        converge(&mut c, &s).await.expect("converge");
        finalize_backend(&mut c, &s).await.expect("finalize");

        assert!(
            c.volumes
                .contains_key(&(POOL.to_string(), SOURCE_VOLUME.to_string())),
            "the shared image was deleted"
        );
    }

    #[tokio::test]
    async fn finalize_removes_every_disk_it_created() {
        let mut c = ready_host();
        let mut s = spec(LibvirtBootSourceKind::InstallMedia);
        s.disks.push(LibvirtDiskSpec {
            name: "data".to_string(),
            size_gi_b: 10,
            bus: LibvirtDiskBus::Virtio,
        });
        converge(&mut c, &s).await.expect("converge");
        finalize_backend(&mut c, &s).await.expect("finalize");

        assert_eq!(c.volumes.len(), 1, "only the shared image should remain");
        assert!(c.domains.is_empty(), "domain still defined");
    }

    /// Deleting a machine that was never realised must not fail — that is the
    /// common case when a VirtualMachine is deleted mid-provision.
    #[tokio::test]
    async fn finalize_is_a_no_op_when_nothing_was_created() {
        let mut c = ready_host();
        finalize_backend(&mut c, &spec(LibvirtBootSourceKind::InstallMedia))
            .await
            .expect("finalize must tolerate an unrealised machine");
    }

    // ------------------------------------------------------------------
    // Backing format
    // ------------------------------------------------------------------

    /// From the name, never the contents. Probing is what libvirt refuses to
    /// do and what banlieue must not reintroduce: a raw image whose first
    /// bytes look like a qcow2 header would otherwise be read as one, and the
    /// backing file it names can be any path the daemon can read.
    #[test]
    fn backing_format_comes_from_the_volume_name() {
        assert_eq!(backing_format("ubuntu-22.04.qcow2"), "qcow2");
        assert_eq!(backing_format("base.qcow"), "qcow2");
        assert_eq!(backing_format("kairos-sandbox.raw"), "raw");
        assert_eq!(backing_format("disk.img"), "raw");
    }

    /// banlieue's own imports are always `.raw` (ADR-0011), so the fallback
    /// is correct for them by construction rather than by luck.
    #[test]
    fn an_unextensioned_volume_falls_back_to_raw() {
        assert_eq!(backing_format("noextension"), "raw");
        assert_eq!(backing_format(""), "raw");
    }

    /// An overlay over a qcow2 backing file must declare qcow2, or current
    /// libvirt refuses the volume outright.
    #[tokio::test]
    async fn an_immediate_machine_declares_its_backing_format() {
        let pool = StoragePool {
            name: POOL.to_string(),
            uuid: [7u8; 16],
        };
        let mut c = FakeMachineClient {
            pools: vec![pool],
            ..Default::default()
        };
        c.volumes.insert(
            (POOL.to_string(), "ubuntu.qcow2".to_string()),
            StorageVol {
                pool: POOL.to_string(),
                name: "ubuntu.qcow2".to_string(),
                key: "/var/lib/libvirt/images/ubuntu.qcow2".to_string(),
            },
        );
        let mut s = spec(LibvirtBootSourceKind::BackingVolume);
        s.boot_source.volume = "ubuntu.qcow2".to_string();

        converge(&mut c, &s).await.expect("converge");
        assert!(
            c.calls
                .contains(&"create_volume:sandboxes-agent-01-os.qcow2".to_string()),
            "{:?}",
            c.calls
        );
    }

    // ------------------------------------------------------------------
    // cloud-init seed (ADR-0054)
    // ------------------------------------------------------------------

    #[test]
    fn seed_volume_is_named_after_the_domain() {
        assert_eq!(
            seed_volume_name("sandboxes-agent-01"),
            "sandboxes-agent-01-cidata.iso"
        );
    }

    /// The gap roadmap 13 was blocked on: `spec.userData` reached
    /// `LibvirtMachine` and then went nowhere. It must end up inside the
    /// seed volume, byte for byte.
    #[tokio::test]
    async fn user_data_reaches_the_seed_volume() {
        let mut c = ready_host();
        let mut s = spec(LibvirtBootSourceKind::InstallMedia);
        s.user_data = Some("#cloud-config\nruncmd:\n  - [echo, marker]\n".to_string());

        converge(&mut c, &s).await.expect("converge");

        let seed = c
            .uploaded
            .get("sandboxes-agent-01-cidata.iso")
            .expect("seed uploaded");
        assert!(
            seed.windows(13).any(|w| w == b"#cloud-config"),
            "user-data missing from the seed"
        );
        assert!(
            seed.windows(6).any(|w| w == b"marker"),
            "user-data payload not carried verbatim"
        );
    }

    /// `instance-id` comes from the domain UUID, so the seed can only be
    /// built after the first define — libvirt assigns the UUID there.
    #[tokio::test]
    async fn the_seed_carries_the_domains_own_uuid() {
        let mut c = ready_host();
        let s = spec(LibvirtBootSourceKind::InstallMedia);
        let observed = converge(&mut c, &s).await.expect("converge");

        let seed = c.uploaded.get("sandboxes-agent-01-cidata.iso").unwrap();
        let uuid = format_uuid(&observed.domain.uuid);
        let ucs = uuid.as_bytes();
        assert!(
            seed.windows(ucs.len()).any(|w| w == ucs),
            "instance-id must be the domain's UUID"
        );
    }

    /// Deterministic image + existing volume means no rewrite. Rewriting
    /// every reconcile would churn the host's storage for no change.
    #[tokio::test]
    async fn a_second_converge_does_not_rewrite_the_seed() {
        let mut c = ready_host();
        let s = spec(LibvirtBootSourceKind::InstallMedia);
        converge(&mut c, &s).await.expect("first");
        converge(&mut c, &s).await.expect("second");
        let uploads = c
            .calls
            .iter()
            .filter(|x| x.starts_with("upload_volume:"))
            .count();
        assert_eq!(uploads, 1, "{:?}", c.calls);
    }

    /// The seed is this machine's, so teardown takes it with everything
    /// else. Leaving it behind would collide with the next VM of the same
    /// name — and that one's cloud-init would read a stale instance-id.
    #[tokio::test]
    async fn finalize_removes_the_seed_volume() {
        let mut c = ready_host();
        let s = spec(LibvirtBootSourceKind::InstallMedia);
        converge(&mut c, &s).await.expect("converge");
        assert!(c.volumes.contains_key(&(
            POOL.to_string(),
            "sandboxes-agent-01-cidata.iso".to_string()
        )));

        finalize_backend(&mut c, &s).await.expect("finalize");
        assert!(
            !c.volumes.contains_key(&(
                POOL.to_string(),
                "sandboxes-agent-01-cidata.iso".to_string()
            )),
            "the seed volume outlived its domain"
        );
    }

    // ------------------------------------------------------------------
    // GuestReady stickiness (ADR-0043 Decision 5)
    // ------------------------------------------------------------------

    /// The first observation is recorded either way: `None` means "not
    /// looked yet", and once we have looked, `false` is a real answer.
    #[test]
    fn a_first_observation_is_recorded_whichever_way_it_went() {
        assert_eq!(sticky_guest_installed(None, false), Some(false));
        assert_eq!(sticky_guest_installed(None, true), Some(true));
    }

    /// The decision this function exists for. The marker lives in the
    /// guest's `/run`, so it vanishes on a power cycle — but a VM that was
    /// stopped has not become uninstalled. Without stickiness a warm pool
    /// member would drop out of its pool on every power cycle, and the pool
    /// would replace a perfectly good VM.
    #[test]
    fn installed_never_goes_back_to_not_installed() {
        assert_eq!(sticky_guest_installed(Some(true), false), Some(true));
        assert_eq!(sticky_guest_installed(Some(true), true), Some(true));
    }

    /// Not-installed is not sticky: a guest still installing must be able
    /// to become installed, which is the entire lifecycle.
    #[test]
    fn not_installed_can_still_become_installed() {
        assert_eq!(sticky_guest_installed(Some(false), true), Some(true));
        assert_eq!(sticky_guest_installed(Some(false), false), Some(false));
    }

    // ------------------------------------------------------------------
    // GuestReady is observed and published (ADR-0043)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn converge_asks_a_running_guest_whether_it_is_installed() {
        let mut c = ready_host();
        let s = spec(LibvirtBootSourceKind::InstallMedia);
        let observed = converge(&mut c, &s).await.expect("converge");

        assert!(!observed.guest_installed, "the fake host reports no marker");
        assert!(
            c.calls
                .iter()
                .any(|x| x == "guest_installed:sandboxes-agent-01"),
            "the guest must actually be asked: {:?}",
            c.calls
        );
    }

    #[tokio::test]
    async fn converge_reports_a_guest_that_has_announced_itself() {
        let mut c = ready_host();
        c.guest_installed.insert("sandboxes-agent-01".to_string());
        let s = spec(LibvirtBootSourceKind::InstallMedia);
        let observed = converge(&mut c, &s).await.expect("converge");
        assert!(observed.guest_installed);
    }

    /// A stopped domain has no agent to answer, so asking is a wasted round
    /// trip on every reconcile of every powered-off VM.
    #[tokio::test]
    async fn converge_does_not_ask_a_stopped_guest() {
        let mut c = ready_host();
        let mut s = spec(LibvirtBootSourceKind::InstallMedia);
        s.desired_power_state = PowerState::PoweredOff;
        let observed = converge(&mut c, &s).await.expect("converge");

        assert!(!observed.guest_installed);
        assert!(
            !c.calls.iter().any(|x| x.starts_with("guest_installed:")),
            "a stopped domain must not be asked: {:?}",
            c.calls
        );
    }

    /// The signal has to reach the CR, not just the Observed struct.
    #[test]
    fn build_status_publishes_guestready_both_ways() {
        let machine = machine_cr();
        let mut observed = observed_running();

        observed.guest_installed = false;
        let st = build_status(&machine, &observed, 1);
        assert_eq!(st.guest_installed, Some(false));
        let c = find_condition(&st, condition_types::GUEST_READY).expect("GuestReady published");
        assert_eq!(c.status, condition_status::FALSE);

        observed.guest_installed = true;
        let st = build_status(&machine, &observed, 1);
        assert_eq!(st.guest_installed, Some(true));
        let c = find_condition(&st, condition_types::GUEST_READY).expect("GuestReady published");
        assert_eq!(c.status, condition_status::TRUE);
    }

    /// ADR-0043 Decision 4. If `Ready` started depending on the guest
    /// signal, every existing Immediate-mode VM whose image has no phase
    /// stage would regress to not-ready for a marker it never sends.
    #[test]
    fn ready_does_not_depend_on_guestready() {
        let machine = machine_cr();
        let observed = observed_running();
        assert!(!observed.guest_installed);

        let st = build_status(&machine, &observed, 1);
        let ready = find_condition(&st, condition_types::READY).expect("Ready published");
        assert_eq!(
            ready.status,
            condition_status::TRUE,
            "a running domain is Ready even with no guest marker"
        );
    }

    /// A `LibvirtMachine` CR wrapping the shared test spec.
    fn machine_cr() -> LibvirtMachine {
        LibvirtMachine {
            metadata: kube::api::ObjectMeta {
                name: Some("agent-01".to_string()),
                namespace: Some("sandboxes".to_string()),
                ..Default::default()
            },
            spec: spec(LibvirtBootSourceKind::InstallMedia),
            status: None,
        }
    }

    /// A running domain with no addresses yet — the state a member is in
    /// for most of a Deferred install.
    fn observed_running() -> Observed {
        Observed {
            domain: Domain {
                name: "sandboxes-agent-01".to_string(),
                uuid: [1u8; 16],
                id: -1,
            },
            state: DomainState::Running,
            addresses: Vec::new(),
            address_source: None,
            guest_installed: false,
        }
    }

    fn find_condition(
        st: &LibvirtMachineStatus,
        type_: &str,
    ) -> Option<k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition> {
        st.conditions.iter().find(|c| c.type_ == type_).cloned()
    }
}
