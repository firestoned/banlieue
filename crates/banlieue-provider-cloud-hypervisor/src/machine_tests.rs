// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `machine.rs`, driven against the strict `FakeHost`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::fake::FakeHost;
    use crate::host::HostOps;
    use crate::host_config::HostConfig;
    use crate::plan::{MachinePlan, plan_machine};
    use crate::systemd::UnitState;
    use banlieue_api::common::{IpamSpec, LocalObjectReference, PowerState};
    use banlieue_api::infrastructure::{
        ChAddressSource, ChBootSource, ChBootSourceKind, ChCpuSpec, ChMemorySpec, ChNicSpec,
        CloudHypervisorMachineSpec, CloudHypervisorMachineStatus,
    };
    use banlieue_cloud_hypervisor::VmState;
    use std::net::Ipv4Addr;

    const UID: &str = "0f3c9a1e-5b7d-4e2a-9c11-3f2b7a5d9e01";

    const MACHINE_NAME: &str = "m1";
    const HOST_UID: u32 = 2_000_000;

    fn config() -> HostConfig {
        HostConfig::parse(
            r#"
[provider]
name = "ch-a"
namespace = "banlieue-system"
kubeconfig = "/etc/banlieue/kubeconfig"
[vmm]
binary = "/usr/local/bin/cloud-hypervisor"
version = "v53.0"
firmware = "/opt/banlieue/firmware/f/CLOUDHV.fd"
[paths]
run_root = "/run/banlieue/ch"
state_root = "/var/lib/banlieue"
[guests]
uid_base = 2000000
uid_count = 10
[storage_classes]
fast = "/srv/banlieue/ch"
[network_classes]
lan = "br0"
"#,
        )
        .unwrap()
    }

    fn spec() -> CloudHypervisorMachineSpec {
        CloudHypervisorMachineSpec {
            provider_id: None,
            failure_domain: Some("ch-a".into()),
            provider_ref: LocalObjectReference {
                name: "ch-a".into(),
            },
            cpus: ChCpuSpec { boot: 2, max: None },
            memory: ChMemorySpec {
                size_mi_b: 2048,
                hugepages: false,
            },
            storage_class: "fast".into(),
            boot_source: ChBootSource {
                kind: ChBootSourceKind::Image,
                image: "kairos.raw".into(),
            },
            os_disk_size_gi_b: 20,
            nics: vec![ChNicSpec {
                name: "eth0".into(),
                network_class: "lan".into(),
                mac_address: None,
                ipam: IpamSpec::default(),
            }],
            tpm_enabled: false,
            user_data: Some("#cloud-config\nhostname: db-01\n".into()),
            desired_power_state: PowerState::PoweredOn,
        }
    }

    fn plan_for(spec: &CloudHypervisorMachineSpec) -> MachinePlan {
        plan_machine(UID, MACHINE_NAME, "ch-a", spec, &config(), HOST_UID).unwrap()
    }

    fn tpm_config() -> HostConfig {
        HostConfig {
            tpm: Some(crate::host_config::TpmSection {
                swtpm: "/usr/bin/swtpm".into(),
                swtpm_setup: "/usr/bin/swtpm_setup".into(),
                setup_config: "/etc/banlieue/swtpm/swtpm_setup.conf".into(),
                ek_ca_certificate: "/var/lib/banlieue/swtpm-localca/issuercert.pem".into(),
            }),
            ..config()
        }
    }

    fn tpm_plan() -> MachinePlan {
        let mut s = spec();
        s.tpm_enabled = true;
        plan_machine(UID, MACHINE_NAME, "ch-a", &s, &tpm_config(), HOST_UID).unwrap()
    }

    async fn tpm_pass(host: &FakeHost, plan: &MachinePlan) -> Result<Observed> {
        converge(
            host,
            plan,
            "db-01",
            Some("#cloud-config\n"),
            &PowerState::PoweredOn,
        )
        .await
    }

    fn host(plan: &MachinePlan) -> FakeHost {
        FakeHost::new(&["br0"], &[plan.image.as_path()])
    }

    async fn pass(host: &FakeHost, plan: &MachinePlan, desired: &PowerState) -> Observed {
        converge(host, plan, "db-01", Some("#cloud-config\n"), desired)
            .await
            .expect("converge")
    }

    fn vm_state(host: &FakeHost, plan: &MachinePlan) -> Option<VmState> {
        host.state.lock().unwrap().vms.get(&plan.unit).copied()
    }

    // ------------------------------------------------------------------
    // Bring-up, one step per pass
    // ------------------------------------------------------------------

    /// First pass: everything on the host is prepared and the VMM unit is
    /// started, then the reconciler comes back: the socket takes a moment.
    #[tokio::test]
    async fn the_first_pass_prepares_the_host_and_starts_the_vmm() {
        let plan = plan_for(&spec());
        let h = host(&plan);
        let o = pass(&h, &plan, &PowerState::PoweredOn).await;
        assert_eq!(o.phase, Phase::StartingVmm);
        let s = h.state.lock().unwrap();
        assert!(s.disks.contains(&plan.os_disk));
        assert!(s.seeds.contains_key(&plan.seed));
        assert_eq!(
            s.taps.get(&plan.nics[0].tap).map(String::as_str),
            Some("br0")
        );
        assert_eq!(s.units.get(&plan.unit), Some(&UnitState::Active));
        assert!(s.vms.is_empty(), "no VM until the API is up");
    }

    #[tokio::test]
    async fn the_second_pass_creates_and_boots_the_vm() {
        let plan = plan_for(&spec());
        let h = host(&plan);
        pass(&h, &plan, &PowerState::PoweredOn).await;
        let o = pass(&h, &plan, &PowerState::PoweredOn).await;
        assert_eq!(o.phase, Phase::Running);
        assert_eq!(vm_state(&h, &plan), Some(VmState::Running));
    }

    /// The strict fake refuses a second vm.create, so this proves converge
    /// never repeats one.
    #[tokio::test]
    async fn later_passes_change_nothing_on_a_running_guest() {
        let plan = plan_for(&spec());
        let h = host(&plan);
        for _ in 0..4 {
            pass(&h, &plan, &PowerState::PoweredOn).await;
        }
        let calls = h.state.lock().unwrap().calls.clone();
        assert_eq!(
            calls.iter().filter(|c| *c == "vm_create").count(),
            1,
            "{calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|c| *c == "vm_boot").count(),
            1,
            "{calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("start_unit")).count(),
            1,
            "{calls:?}"
        );
    }

    /// A VM that was created but not booted (a crash between the two calls)
    /// is booted, not created again.
    #[tokio::test]
    async fn a_created_but_unbooted_vm_is_booted() {
        let plan = plan_for(&spec());
        let h = host(&plan);
        pass(&h, &plan, &PowerState::PoweredOn).await;
        h.state
            .lock()
            .unwrap()
            .vms
            .insert(plan.unit.clone(), VmState::Created);
        assert_eq!(
            pass(&h, &plan, &PowerState::PoweredOn).await.phase,
            Phase::Running
        );
    }

    #[tokio::test]
    async fn addresses_come_from_the_neighbour_table() {
        let plan = plan_for(&spec());
        let h = host(&plan);
        pass(&h, &plan, &PowerState::PoweredOn).await;
        let o = pass(&h, &plan, &PowerState::PoweredOn).await;
        assert!(o.addresses.is_empty(), "nothing seen yet");
        h.state.lock().unwrap().neighbours.push((
            plan.nics[0].mac.clone(),
            "br0".into(),
            Ipv4Addr::new(192, 0, 2, 44),
        ));
        let o = pass(&h, &plan, &PowerState::PoweredOn).await;
        assert_eq!(o.addresses, vec![Ipv4Addr::new(192, 0, 2, 44)]);
    }

    #[tokio::test]
    async fn without_user_data_no_seed_is_written() {
        let mut s = spec();
        s.user_data = None;
        let plan = plan_for(&s);
        let h = host(&plan);
        converge(&h, &plan, "db-01", None, &PowerState::PoweredOn)
            .await
            .unwrap();
        assert!(h.state.lock().unwrap().seeds.is_empty());
    }

    // ------------------------------------------------------------------
    // Failures surface; they are not papered over
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn a_missing_image_is_an_error_naming_it() {
        let plan = plan_for(&spec());
        let h = FakeHost::new(&["br0"], &[]);
        let e = converge(&h, &plan, "db-01", None, &PowerState::PoweredOn)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("kairos.raw"), "{e}");
    }

    #[tokio::test]
    async fn a_missing_bridge_is_an_error() {
        let plan = plan_for(&spec());
        let h = FakeHost::new(&[], &[plan.image.as_path()]);
        let e = converge(&h, &plan, "db-01", None, &PowerState::PoweredOn)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("br0"), "{e}");
    }

    /// ADR-0061 Decision 5: an older VMM is refused before anything is
    /// created on it.
    #[tokio::test]
    async fn an_older_vmm_is_refused_before_create() {
        let plan = plan_for(&spec());
        let h = host(&plan);
        h.state.lock().unwrap().vmm_version = "52.1.0".into();
        pass(&h, &plan, &PowerState::PoweredOn).await;
        let e = converge(&h, &plan, "db-01", None, &PowerState::PoweredOn)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("52.1.0"), "{e}");
        assert!(h.state.lock().unwrap().vms.is_empty());
    }

    /// A VMM that died is reported with systemd's reason, its unit is
    /// cleared, and the next pass (after the reconciler's backoff) starts it
    /// again. Reporting first is the point: a unit that fails at once must
    /// not become a silent restart loop.
    #[tokio::test]
    async fn a_failed_vmm_unit_is_reported_then_restarted() {
        let plan = plan_for(&spec());
        let h = host(&plan);
        pass(&h, &plan, &PowerState::PoweredOn).await;
        pass(&h, &plan, &PowerState::PoweredOn).await;
        h.fail_unit(&plan.unit, "exit-code, status 217");
        let e = converge(&h, &plan, "vm-a", None, &PowerState::PoweredOn)
            .await
            .unwrap_err();
        assert!(
            matches!(e, Error::VmmExited(ref d) if d.contains("217")),
            "{e}"
        );
        assert!(!h.state.lock().unwrap().units.contains_key(&plan.unit));
        let o = pass(&h, &plan, &PowerState::PoweredOn).await;
        assert_eq!(o.phase, Phase::StartingVmm);
        assert_eq!(
            h.state.lock().unwrap().units.get(&plan.unit),
            Some(&UnitState::Active)
        );
        assert_eq!(
            pass(&h, &plan, &PowerState::PoweredOn).await.phase,
            Phase::Running
        );
    }

    // ------------------------------------------------------------------
    // Power off
    // ------------------------------------------------------------------

    /// Off is graceful: the power button first, then the unit once the guest
    /// has shut down.
    #[tokio::test]
    async fn powering_off_presses_the_button_then_stops_the_unit() {
        let plan = plan_for(&spec());
        let h = host(&plan);
        pass(&h, &plan, &PowerState::PoweredOn).await;
        pass(&h, &plan, &PowerState::PoweredOn).await;
        let o = pass(&h, &plan, &PowerState::PoweredOff).await;
        assert_eq!(o.phase, Phase::Stopping);
        let o = pass(&h, &plan, &PowerState::PoweredOff).await;
        assert_eq!(o.phase, Phase::Stopped);
        assert!(!h.state.lock().unwrap().units.contains_key(&plan.unit));
        // The disk survives a power-off.
        assert!(h.state.lock().unwrap().disks.contains(&plan.os_disk));
    }

    #[tokio::test]
    async fn a_stopped_guest_is_started_again_when_desired_on() {
        let plan = plan_for(&spec());
        let h = host(&plan);
        for d in [
            PowerState::PoweredOn,
            PowerState::PoweredOn,
            PowerState::PoweredOff,
            PowerState::PoweredOff,
        ] {
            pass(&h, &plan, &d).await;
        }
        assert_eq!(
            pass(&h, &plan, &PowerState::PoweredOn).await.phase,
            Phase::StartingVmm
        );
        assert_eq!(
            pass(&h, &plan, &PowerState::PoweredOn).await.phase,
            Phase::Running
        );
    }

    // ------------------------------------------------------------------
    // Teardown
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn teardown_removes_unit_taps_and_files_and_is_idempotent() {
        let plan = plan_for(&spec());
        let h = host(&plan);
        pass(&h, &plan, &PowerState::PoweredOn).await;
        pass(&h, &plan, &PowerState::PoweredOn).await;
        teardown(&h, &plan).await.unwrap();
        {
            let s = h.state.lock().unwrap();
            assert!(s.units.is_empty() && s.taps.is_empty() && s.disks.is_empty());
        }
        assert!(!h.files_exist(&plan).await);
        teardown(&h, &plan).await.unwrap();
    }

    /// The unit goes first: removing files under a running VMM gives the
    /// guest I/O errors rather than a stop.
    #[tokio::test]
    async fn teardown_stops_the_unit_before_touching_files() {
        let plan = plan_for(&spec());
        let h = host(&plan);
        pass(&h, &plan, &PowerState::PoweredOn).await;
        h.state.lock().unwrap().calls.clear();
        teardown(&h, &plan).await.unwrap();
        let calls = h.state.lock().unwrap().calls.clone();
        let stop = calls
            .iter()
            .position(|c| c.starts_with("stop_unit"))
            .unwrap();
        let files = calls.iter().position(|c| c == "remove_files").unwrap();
        assert!(stop < files, "{calls:?}");
    }

    // ------------------------------------------------------------------
    // Status
    // ------------------------------------------------------------------

    fn observed(phase: Phase, addresses: Vec<Ipv4Addr>) -> Observed {
        Observed {
            phase,
            addresses,
            ..Observed::default()
        }
    }

    #[test]
    fn a_running_guest_is_provisioned_and_ready() {
        let plan = plan_for(&spec());
        let st = build_status(None, &observed(Phase::Running, vec![]), &plan, "ch-a", 3);
        assert_eq!(st.initialization.provisioned, Some(true));
        assert_eq!(st.observed_power_state, Some(PowerState::PoweredOn));
        assert_eq!(st.host_uid, Some(HOST_UID));
        assert_eq!(st.failure_domain.as_deref(), Some("ch-a"));
        assert_eq!(st.observed_generation, Some(3));
        let ready = st.conditions.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(ready.status, "True");
    }

    #[test]
    fn a_starting_guest_is_not_ready_and_says_why() {
        let plan = plan_for(&spec());
        let st = build_status(
            None,
            &observed(Phase::StartingVmm, vec![]),
            &plan,
            "ch-a",
            1,
        );
        assert_ne!(st.initialization.provisioned, Some(true));
        let ready = st.conditions.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "StartingVmm");
    }

    #[test]
    fn addresses_are_published_as_internal_ips_from_the_neighbour_table() {
        let plan = plan_for(&spec());
        let st = build_status(
            None,
            &observed(Phase::Running, vec![Ipv4Addr::new(192, 0, 2, 44)]),
            &plan,
            "ch-a",
            1,
        );
        assert_eq!(st.addresses.len(), 1);
        assert_eq!(st.addresses[0].address, "192.0.2.44");
        assert_eq!(st.address_source, Some(ChAddressSource::Neighbour));
    }

    /// A reconcile that has not seen addresses yet must not wipe ones a
    /// previous pass published: the neighbour cache expires quickly.
    #[test]
    fn known_addresses_are_kept_while_the_guest_runs() {
        let plan = plan_for(&spec());
        let before = build_status(
            None,
            &observed(Phase::Running, vec![Ipv4Addr::new(192, 0, 2, 44)]),
            &plan,
            "ch-a",
            1,
        );
        let after = build_status(
            Some(&before),
            &observed(Phase::Running, vec![]),
            &plan,
            "ch-a",
            1,
        );
        assert_eq!(after.addresses, before.addresses);
        let stopped = build_status(
            Some(&before),
            &observed(Phase::Stopped, vec![]),
            &plan,
            "ch-a",
            1,
        );
        assert!(
            stopped.addresses.is_empty(),
            "a stopped guest has no address"
        );
    }

    #[test]
    fn failure_status_keeps_the_host_uid_and_reports_the_error() {
        let previous = CloudHypervisorMachineStatus {
            host_uid: Some(HOST_UID),
            ..Default::default()
        };
        let st = failure_status(Some(&previous), "bridge br0 not found", "HostError", 2);
        assert_eq!(st.host_uid, Some(HOST_UID));
        let ready = st.conditions.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "HostError");
        assert!(ready.message.contains("br0"));
    }

    // ------------------------------------------------------------------
    // vTPM (ADR-0065 Decisions 1–2)
    // ------------------------------------------------------------------

    /// The state is cleared for a fresh manufacture exactly once, just
    /// before the manufacture unit starts, never while it runs (found live:
    /// a pass during `swtpm_setup` deleted the directory under it).
    #[tokio::test]
    async fn tpm_state_is_reset_only_when_manufacture_starts() {
        let plan = tpm_plan();
        let h = host(&plan);
        for _ in 0..3 {
            tpm_pass(&h, &plan).await.unwrap();
        }
        let calls = h.state.lock().unwrap().calls.clone();
        assert_eq!(
            calls.iter().filter(|c| *c == "reset_tpm_state").count(),
            1,
            "{calls:?}"
        );
        let reset = calls.iter().position(|c| c == "reset_tpm_state").unwrap();
        let start = calls
            .iter()
            .position(|c| c.starts_with("start_unit banlieue-swtpm-setup@"))
            .unwrap();
        assert!(reset < start, "{calls:?}");
    }

    /// Manufacture first, as its own unit; no swtpm and no VMM until the
    /// TPM exists.
    #[tokio::test]
    async fn a_vtpm_is_manufactured_before_anything_starts() {
        let plan = tpm_plan();
        let t = plan.tpm.clone().unwrap();
        let h = host(&plan);
        let o = tpm_pass(&h, &plan).await.unwrap();
        assert_eq!(o.phase, Phase::StartingTpm);
        {
            let s = h.state.lock().unwrap();
            assert!(s.units.contains_key(&t.setup_unit));
            assert!(!s.units.contains_key(&t.unit));
            assert!(!s.units.contains_key(&plan.unit));
        }
        // Still manufacturing: nothing more starts, nothing restarts.
        let o = tpm_pass(&h, &plan).await.unwrap();
        assert_eq!(o.phase, Phase::StartingTpm);
    }

    /// After manufacture: state handed to the guest, swtpm started, and
    /// the VMM only once swtpm is up. The VM boots with the TPM, and the
    /// host-minted EK certificate is observed.
    #[tokio::test]
    async fn after_manufacture_swtpm_then_the_vmm_start_and_the_ek_is_observed() {
        let plan = tpm_plan();
        let t = plan.tpm.clone().unwrap();
        let h = host(&plan);
        tpm_pass(&h, &plan).await.unwrap();
        h.finish_tpm_setup(&plan);

        let o = tpm_pass(&h, &plan).await.unwrap();
        assert_eq!(o.phase, Phase::StartingTpm);
        {
            let s = h.state.lock().unwrap();
            assert!(s.adopted.contains(&plan.uid));
            assert_eq!(s.units.get(&t.unit), Some(&UnitState::Active));
            assert!(!s.units.contains_key(&plan.unit));
        }
        let o = tpm_pass(&h, &plan).await.unwrap();
        assert_eq!(o.phase, Phase::StartingVmm);
        let o = tpm_pass(&h, &plan).await.unwrap();
        assert_eq!(o.phase, Phase::Running);
        assert_eq!(o.ek_certificates, vec![format!("ek-of-{UID}")]);
        let calls = h.state.lock().unwrap().calls.clone();
        assert_eq!(
            calls.iter().filter(|c| c.contains(&t.setup_unit)).count(),
            1,
            "manufactured exactly once: {calls:?}"
        );
    }

    /// A failed manufacture is a host problem (found live: a wrong group),
    /// not a transient one. It stays failed and is reported on every pass,
    /// instead of being cleared and retried every few seconds, which made
    /// the status flip between the error and `StartingTpm`.
    #[tokio::test]
    async fn a_failed_manufacture_stays_failed_and_reported() {
        let plan = tpm_plan();
        let t = plan.tpm.clone().unwrap();
        let h = host(&plan);
        tpm_pass(&h, &plan).await.unwrap();
        h.fail_unit(&t.setup_unit, "exit-code, status=1");
        for _ in 0..3 {
            let e = tpm_pass(&h, &plan).await.unwrap_err();
            assert!(e.to_string().contains("status=1"), "{e}");
            assert!(
                e.to_string().contains("reset-failed"),
                "says how to retry: {e}"
            );
        }
        let s = h.state.lock().unwrap();
        assert_eq!(s.units.get(&t.setup_unit), Some(&UnitState::Failed));
        assert_eq!(
            s.calls
                .iter()
                .filter(|c| c.contains(&format!("start_unit {}", t.setup_unit)))
                .count(),
            1,
            "not restarted"
        );
        assert!(!s.units.contains_key(&plan.unit));
    }

    /// A swtpm that died is restarted before the VMM is trusted again.
    #[tokio::test]
    async fn a_failed_swtpm_is_reported_and_the_vmm_is_not_started() {
        let plan = tpm_plan();
        let t = plan.tpm.clone().unwrap();
        let h = host(&plan);
        tpm_pass(&h, &plan).await.unwrap();
        h.finish_tpm_setup(&plan);
        tpm_pass(&h, &plan).await.unwrap();
        h.fail_unit(&t.unit, "signal, status=9/KILL");
        assert!(tpm_pass(&h, &plan).await.is_err());
        assert!(!h.state.lock().unwrap().units.contains_key(&plan.unit));
    }

    /// Teardown stops swtpm and any manufacture with the VMM.
    #[tokio::test]
    async fn teardown_stops_the_tpm_units_too() {
        let plan = tpm_plan();
        let t = plan.tpm.clone().unwrap();
        let h = host(&plan);
        tpm_pass(&h, &plan).await.unwrap();
        h.finish_tpm_setup(&plan);
        for _ in 0..3 {
            tpm_pass(&h, &plan).await.unwrap();
        }
        teardown(&h, &plan).await.unwrap();
        let s = h.state.lock().unwrap();
        assert!(!s.units.contains_key(&t.unit));
        assert!(!s.units.contains_key(&t.setup_unit));
        assert!(!s.units.contains_key(&plan.unit));
    }

    #[tokio::test]
    async fn status_publishes_the_tpm_and_its_ek() {
        let plan = tpm_plan();
        let o = Observed {
            phase: Phase::Running,
            addresses: vec![],
            ek_certificates: vec!["pem".into()],
            ..Observed::default()
        };
        let st = build_status(None, &o, &plan, "ch-a", 1);
        assert_eq!(st.tpm_attached, Some(true));
        assert_eq!(st.tpm_endorsement_certificates, vec!["pem".to_string()]);
        let plain = build_status(None, &o, &plan_for(&spec()), "ch-a", 1);
        assert_eq!(plain.tpm_attached, Some(false));
    }

    // ------------------------------------------------------------------
    // Deferred install and the guest's report (ADR-0065 Decisions 3–5)
    // ------------------------------------------------------------------

    fn deferred_spec() -> CloudHypervisorMachineSpec {
        let mut s = spec();
        s.boot_source = ChBootSource {
            kind: ChBootSourceKind::InstallMedia,
            image: "installer.iso".into(),
        };
        s
    }

    fn guest_ready(
        st: &banlieue_api::infrastructure::CloudHypervisorMachineStatus,
    ) -> Option<(String, String)> {
        st.conditions
            .iter()
            .find(|c| c.type_ == "GuestReady")
            .map(|c| (c.status.clone(), c.reason.clone()))
    }

    /// The report listener is up before the VMM, so a guest's first report
    /// is never lost.
    #[tokio::test]
    async fn the_report_listener_starts_before_the_vmm() {
        let plan = plan_for(&deferred_spec());
        let h = host(&plan);
        pass(&h, &plan, &PowerState::PoweredOn).await;
        let s = h.state.lock().unwrap();
        assert!(s.listening.contains(&plan.uid));
        assert!(s.disks.contains(&plan.os_disk), "empty OS disk created");
        assert!(
            s.installers.contains(&plan.install_media),
            "the installer is staged in the machine directory before the VMM starts"
        );
    }

    /// A Deferred machine whose installer is not in the cache fails before
    /// anything starts, naming the image.
    #[tokio::test]
    async fn a_missing_installer_fails_before_the_vmm_starts() {
        let plan = plan_for(&deferred_spec());
        let h = host(&plan);
        h.state.lock().unwrap().images.clear();
        let err = converge(&h, &plan, "db-01", None, &PowerState::PoweredOn)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("installer.iso"), "{err}");
        assert!(!h.state.lock().unwrap().units.contains_key(&plan.unit));
    }

    /// Installing: GuestReady is published and False; the installer is
    /// attached. Once the installed system reports, the installer is
    /// hot-unplugged in the same pass, and only then is GuestReady True.
    #[tokio::test]
    async fn the_installer_is_ejected_when_the_installed_system_reports() {
        let plan = plan_for(&deferred_spec());
        let h = host(&plan);
        pass(&h, &plan, &PowerState::PoweredOn).await;
        let o = pass(&h, &plan, &PowerState::PoweredOn).await;
        assert_eq!(o.phase, Phase::Running);
        assert!(!o.guest_installed);
        assert_eq!(o.install_media_detached, Some(false));
        let st = build_status(None, &o, &plan, "ch-a", 1);
        assert_eq!(
            guest_ready(&st),
            Some(("False".into(), "GuestNotAnnounced".into()))
        );
        assert_eq!(st.install_media_detached, Some(false));

        h.guest_reports_installed(&plan);
        let o = pass(&h, &plan, &PowerState::PoweredOn).await;
        assert!(o.guest_installed);
        assert_eq!(o.install_media_detached, Some(true));
        assert_eq!(
            h.state.lock().unwrap().removed_devices,
            vec![(plan.unit.clone(), crate::plan::DISK_ID_INSTALL.to_string())]
        );
        let st = build_status(Some(&st), &o, &plan, "ch-a", 1);
        assert_eq!(st.guest_installed, Some(true));
        assert_eq!(st.install_media_detached, Some(true));
        assert_eq!(
            guest_ready(&st),
            Some(("True".into(), "GuestAnnounced".into()))
        );
    }

    /// After the eject the reconciler plans without the installer; nothing
    /// is removed twice, and the sticky status survives a pass that no
    /// longer sees an installer at all.
    #[tokio::test]
    async fn after_the_eject_the_installer_stays_out_and_status_stays_sticky() {
        let plan = plan_for(&deferred_spec());
        let h = host(&plan);
        pass(&h, &plan, &PowerState::PoweredOn).await;
        pass(&h, &plan, &PowerState::PoweredOn).await;
        h.guest_reports_installed(&plan);
        let o = pass(&h, &plan, &PowerState::PoweredOn).await;
        let st = build_status(None, &o, &plan, "ch-a", 1);

        let detached = plan.clone().without_install_media();
        let o = pass(&h, &detached, &PowerState::PoweredOn).await;
        assert_eq!(o.install_media_detached, None);
        let st = build_status(Some(&st), &o, &detached, "ch-a", 1);
        assert_eq!(st.install_media_detached, Some(true));
        assert_eq!(st.guest_installed, Some(true));
        assert_eq!(h.state.lock().unwrap().removed_devices.len(), 1);
        assert!(
            !h.state
                .lock()
                .unwrap()
                .installers
                .contains(&plan.install_media),
            "the ejected installer's copy is deleted"
        );
    }

    /// A tpmEnabled Deferred guest is GuestReady only with its EK published.
    #[tokio::test]
    async fn a_tpm_guest_is_not_ready_without_its_ek() {
        let mut s = deferred_spec();
        s.tpm_enabled = true;
        let plan = plan_machine(UID, MACHINE_NAME, "ch-a", &s, &tpm_config(), HOST_UID).unwrap();
        let o = Observed {
            phase: Phase::Running,
            guest_installed: true,
            install_media_detached: Some(true),
            ..Observed::default()
        };
        let st = build_status(None, &o, &plan, "ch-a", 1);
        assert_eq!(
            guest_ready(&st),
            Some(("False".into(), "TpmEndorsementPending".into()))
        );
    }

    /// An Immediate image has no reason to report: GuestReady is not
    /// published, so a pool says the signal is absent instead of waiting
    /// for it forever (ADR-0046 Decision 3).
    #[tokio::test]
    async fn an_immediate_image_that_never_reports_publishes_no_guest_ready() {
        let plan = plan_for(&spec());
        let h = host(&plan);
        pass(&h, &plan, &PowerState::PoweredOn).await;
        let o = pass(&h, &plan, &PowerState::PoweredOn).await;
        let st = build_status(None, &o, &plan, "ch-a", 1);
        assert!(guest_ready(&st).is_none());
        assert_eq!(st.install_media_detached, None);
    }

    #[tokio::test]
    async fn teardown_stops_listening() {
        let plan = plan_for(&deferred_spec());
        let h = host(&plan);
        pass(&h, &plan, &PowerState::PoweredOn).await;
        teardown(&h, &plan).await.unwrap();
        assert!(!h.state.lock().unwrap().listening.contains(&plan.uid));
    }
}
