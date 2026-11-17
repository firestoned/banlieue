// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the pure decisions in `reconciler.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::machine::{Observed, Phase};
    use banlieue_api::capi::{BOOTSTRAP_DATA_READY, CLUSTER_NAME_LABEL, bootstrap_reasons};
    use banlieue_api::common::{IpamSpec, PowerState, ProviderReference};
    use banlieue_api::infrastructure::{
        ChBootSource, ChBootSourceKind, ChCpuSpec, ChMemorySpec, CloudHypervisorMachineSpec,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
    use kube::api::ObjectMeta;
    use std::net::Ipv4Addr;

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

    fn machine(
        name: &str,
        ns: &str,
        provider: &str,
        host_uid: Option<u32>,
    ) -> CloudHypervisorMachine {
        machine_ref(name, ns, provider, None, host_uid)
    }

    /// A machine in `ns` whose `providerRef` names `provider` in `provider_ns`.
    fn machine_ref(
        name: &str,
        ns: &str,
        provider: &str,
        provider_ns: Option<&str>,
        host_uid: Option<u32>,
    ) -> CloudHypervisorMachine {
        let _ = IpamSpec::default();
        CloudHypervisorMachine {
            metadata: ObjectMeta {
                name: Some(name.into()),
                namespace: Some(ns.into()),
                uid: Some(format!(
                    "0f3c9a1e-5b7d-4e2a-9c11-3f2b7a5d9e{:02x}",
                    name.len()
                )),
                ..Default::default()
            },
            spec: CloudHypervisorMachineSpec {
                provider_id: None,
                failure_domain: None,
                provider_ref: ProviderReference {
                    name: provider.into(),
                    namespace: provider_ns.map(str::to_string),
                },
                cpus: ChCpuSpec { boot: 1, max: None },
                memory: ChMemorySpec {
                    size_mi_b: 512,
                    hugepages: false,
                },
                storage_class: "fast".into(),
                boot_source: ChBootSource {
                    kind: ChBootSourceKind::Image,
                    image: "kairos.raw".into(),
                },
                os_disk_size_gi_b: 20,
                nics: vec![],
                tpm_enabled: false,
                user_data: None,
                desired_power_state: PowerState::PoweredOn,
                first_boot_sealing: None,
                identity: None,
                move_in: None,
                move_out: None,
            },
            status: host_uid.map(|u| CloudHypervisorMachineStatus {
                host_uid: Some(u),
                ..Default::default()
            }),
        }
    }

    /// One Provider is one host (ADR-0060). Another host's machines are not
    /// touched, and a bare `providerRef` in a tenant namespace resolves to the
    /// tenant namespace, not to this host's Provider (ADR-0134).
    #[test]
    fn only_this_hosts_machines_are_ours() {
        let c = config();
        assert!(is_ours(&machine("a", "banlieue-system", "ch-a", None), &c));
        assert!(!is_ours(&machine("a", "banlieue-system", "ch-b", None), &c));
        assert!(!is_ours(&machine("a", "team-a", "ch-a", None), &c));
    }

    /// A tenant machine names the shared host by name AND namespace.
    #[test]
    fn a_tenant_machine_naming_our_provider_in_home_is_ours() {
        let c = config();
        let m = machine_ref("a", "team-a", "ch-a", Some("banlieue-system"), None);
        assert!(is_ours(&m, &c));
    }

    #[test]
    fn a_tenant_machine_naming_another_namespace_or_host_is_not_ours() {
        let c = config();
        let other_ns = machine_ref("a", "team-a", "ch-a", Some("team-b"), None);
        let other_host = machine_ref("a", "team-a", "ch-b", Some("banlieue-system"), None);
        assert!(!is_ours(&other_ns, &c));
        assert!(!is_ours(&other_host, &c));
    }

    /// Guest uids are one pool per host, so the ledger spans every namespace
    /// the host serves.
    #[test]
    fn used_uids_span_every_namespace_the_host_serves() {
        let c = config();
        let machines = vec![
            machine("a", "banlieue-system", "ch-a", Some(2_000_000)),
            machine_ref(
                "b",
                "team-a",
                "ch-a",
                Some("banlieue-system"),
                Some(2_000_001),
            ),
            machine("c", "team-a", "ch-a", Some(2_000_002)),
        ];
        let used = used_host_uids(&machines, &c, "nobody");
        assert_eq!(used, [2_000_000, 2_000_001].into());
    }

    #[test]
    fn used_uids_come_from_this_hosts_machines_only_and_exclude_self() {
        let c = config();
        let me = machine("self", "banlieue-system", "ch-a", Some(2_000_005));
        let machines = vec![
            machine("a", "banlieue-system", "ch-a", Some(2_000_000)),
            machine("bb", "banlieue-system", "ch-b", Some(2_000_001)),
            machine("ccc", "banlieue-system", "ch-a", None),
            me.clone(),
        ];
        let used = used_host_uids(&machines, &c, &me.uid().unwrap());
        assert_eq!(used, [2_000_000].into());
    }

    #[test]
    fn requeue_is_quick_while_changing_and_slow_when_settled() {
        let quick = requeue_for(
            &Observed {
                phase: Phase::StartingVmm,
                addresses: vec![],
                ..Observed::default()
            },
            false,
        );
        let waiting_dhcp = requeue_for(
            &Observed {
                phase: Phase::Running,
                addresses: vec![],
                ..Observed::default()
            },
            false,
        );
        let settled = requeue_for(&running_with_address(), false);
        assert_eq!(quick, Action::requeue(Duration::from_secs(1)));
        assert_eq!(waiting_dhcp, requeue_default());
        assert_eq!(settled, requeue_long());
        assert_eq!(
            requeue_for(
                &Observed {
                    phase: Phase::Stopping,
                    addresses: vec![],
                    ..Observed::default()
                },
                false,
            ),
            quick
        );
    }

    /// Roadmap 18 phase C: a VMM start is four or five quick steps
    /// (manufacture, swtpm, VMM unit, API socket), each a pass. At 3 s a
    /// pass they cost a sealed clone about 8 s of its 106 s, nearly all of
    /// it waiting; each step itself takes well under a second.
    #[test]
    fn a_vmm_start_step_is_looked_at_again_within_a_second() {
        for phase in [Phase::StartingTpm, Phase::StartingVmm, Phase::Stopping] {
            assert_eq!(
                requeue_for(
                    &Observed {
                        phase: phase.clone(),
                        ..Observed::default()
                    },
                    false,
                ),
                Action::requeue(Duration::from_secs(1)),
                "{phase:?}"
            );
        }
    }

    fn running_with_address() -> Observed {
        Observed {
            phase: Phase::Running,
            addresses: vec![Ipv4Addr::new(192, 0, 2, 9)],
            ..Observed::default()
        }
    }

    /// ADR-0112: the report itself wakes the reconciler, but nothing wakes it
    /// at the deadline, so a guest that never reports must still be looked at
    /// often enough for `FirstBootSealingTimeout` to land on time, not up to
    /// a long requeue late.
    #[test]
    fn requeue_stays_default_while_a_sealing_report_is_due() {
        assert_eq!(
            requeue_for(&running_with_address(), true),
            requeue_default()
        );
        // A transition still wins: it is quicker.
        assert_eq!(
            requeue_for(
                &Observed {
                    phase: Phase::StartingVmm,
                    ..Observed::default()
                },
                true,
            ),
            Action::requeue(Duration::from_secs(1))
        );
    }

    fn ready_at(secs: i64) -> CloudHypervisorMachineStatus {
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
        CloudHypervisorMachineStatus {
            conditions: vec![Condition {
                type_: "Ready".into(),
                status: "True".into(),
                reason: "GuestRunning".into(),
                message: String::new(),
                last_transition_time: Time(
                    k8s_openapi::jiff::Timestamp::from_second(secs).unwrap(),
                ),
                observed_generation: None,
            }],
            ..CloudHypervisorMachineStatus::default()
        }
    }

    #[test]
    fn a_sealing_report_is_due_only_while_pending() {
        let req: banlieue_api::common::FirstBootSealing =
            serde_json::from_str(r#"{"timeoutSeconds": 180}"#).unwrap();
        let at = |s: i64| k8s_openapi::jiff::Timestamp::from_second(s).unwrap();
        let st = ready_at(1_000);
        // No declaration: nothing is due.
        assert!(!sealing_report_due(None, &st, at(1_010)));
        // Declared, within the deadline, nothing reported: due.
        assert!(sealing_report_due(Some(&req), &st, at(1_010)));
        // Past the deadline: timed out, no longer due.
        assert!(!sealing_report_due(Some(&req), &st, at(1_181)));
        // Reported in full: satisfied, no longer due.
        let sealed = CloudHypervisorMachineStatus {
            sealed_partitions: vec!["COS_PERSISTENT".into()],
            ..st
        };
        assert!(!sealing_report_due(Some(&req), &sealed, at(1_010)));
    }

    #[test]
    fn errors_map_to_reasons_and_permanence() {
        let unsupported = Error::Plan(PlanError::Unsupported("vTPM".into()));
        assert_eq!(failure_reason(&unsupported), "Unsupported");
        assert!(is_permanent(&unsupported));

        let bad = Error::Plan(PlanError::UnknownStorageClass("gold".into()));
        assert_eq!(failure_reason(&bad), "InvalidSpec");
        assert!(is_permanent(&bad));

        let old = Error::Vmm(banlieue_cloud_hypervisor::Error::VersionUnsupported {
            found: "52.0.0".into(),
            minimum: "53.0".into(),
        });
        assert_eq!(failure_reason(&old), "VmmVersionUnsupported");

        let io = Error::Io(std::io::Error::other("disk full"));
        assert_eq!(failure_reason(&io), "HostError");
        assert!(!is_permanent(&io), "host errors are retried");

        assert_eq!(failure_reason(&Error::UidRangeFull), "GuestUidRangeFull");

        let died = Error::VmmExited("exit-code, status 217".into());
        assert_eq!(failure_reason(&died), "VmmExited");
        assert!(!is_permanent(&died), "retried, with backoff");
    }

    // ----------------------------------------------------------------------
    // ADR-0129: hold a CAPI machine's guest until its bootstrap data is there
    // ----------------------------------------------------------------------

    fn owner(api_version: &str, kind: &str, name: &str) -> OwnerReference {
        OwnerReference {
            api_version: api_version.into(),
            kind: kind.into(),
            name: name.into(),
            uid: format!("uid-{name}"),
            controller: Some(true),
            block_owner_deletion: None,
        }
    }

    /// A machine shaped as Cluster API's `GenerateTemplate` creates it:
    /// owned by its MachineSet, labelled with the cluster.
    fn as_capi_creates_it() -> CloudHypervisorMachine {
        let mut m = machine("wl-1-md-0-x", "banlieue-system", "ch-a", None);
        m.metadata.owner_references = Some(vec![owner(
            "cluster.x-k8s.io/v1beta2",
            "MachineSet",
            "wl-1-md-0",
        )]);
        m.metadata.labels = Some([(CLUSTER_NAME_LABEL.to_string(), "wl-1".to_string())].into());
        m
    }

    /// The same machine once the Machine controller has adopted it.
    fn adopted(m: &CloudHypervisorMachine) -> CloudHypervisorMachine {
        let mut m = m.clone();
        m.metadata.owner_references = Some(vec![owner(
            "cluster.x-k8s.io/v1beta2",
            "Machine",
            "wl-1-md-0-x",
        )]);
        m
    }

    fn with_bootstrap_condition(
        m: &CloudHypervisorMachine,
        status: &str,
    ) -> CloudHypervisorMachine {
        let mut m = m.clone();
        let mut st = m.status.clone().unwrap_or_default();
        banlieue_provider_sdk::status::set_condition(
            &mut st.conditions,
            BOOTSTRAP_DATA_READY,
            status,
            "R",
            "",
            1,
        );
        m.status = Some(st);
        m
    }

    /// The whole ordering, as Cluster API produces it: created (held), adopted
    /// without data (held), adopted with data delivered (created).
    #[test]
    fn a_capi_machine_is_held_from_creation_until_its_data_is_delivered() {
        let created = as_capi_creates_it();
        assert!(holds_guest(&created), "before adoption");
        let owned = adopted(&created);
        assert!(holds_guest(&owned), "adopted, no condition");
        assert!(
            holds_guest(&with_bootstrap_condition(&owned, "False")),
            "adopted, data not delivered"
        );
        assert!(
            !holds_guest(&with_bootstrap_condition(&owned, "True")),
            "adopted, data delivered"
        );
    }

    #[test]
    fn a_delivered_condition_before_adoption_does_not_release_the_hold() {
        assert!(holds_guest(&with_bootstrap_condition(
            &as_capi_creates_it(),
            "True"
        )));
    }

    #[test]
    fn a_standalone_or_virtualmachine_machine_is_never_held() {
        assert!(!holds_guest(&machine("a", "banlieue-system", "ch-a", None)));
        let mut vm_owned = machine("b", "banlieue-system", "ch-a", None);
        vm_owned.metadata.owner_references = Some(vec![owner(
            "banlieue.io/v1alpha1",
            "VirtualMachine",
            "vm-b",
        )]);
        assert!(!holds_guest(&vm_owned));
    }

    /// A guest uid is recorded before anything is created on the host, so a
    /// machine that has one already exists there (created before an upgrade):
    /// it is never held, or its running guest would be left unmanaged.
    #[test]
    fn a_machine_with_a_guest_uid_is_never_held() {
        let mut m = adopted(&as_capi_creates_it());
        m.status = Some(CloudHypervisorMachineStatus {
            host_uid: Some(2_000_001),
            ..Default::default()
        });
        assert!(!holds_guest(&m));
    }

    #[test]
    fn the_held_status_says_why_and_reports_nothing_provisioned() {
        let st = held_status(None, 4);
        assert_eq!(st.initialization.provisioned, Some(false));
        let ready = st
            .conditions
            .iter()
            .find(|c| c.type_ == "Ready")
            .expect("Ready");
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, bootstrap_reasons::WAITING_FOR_BOOTSTRAP_DATA);
        assert_eq!(ready.observed_generation, Some(4));
        assert_eq!(st.host_uid, None, "no guest uid is allocated while held");
    }

    /// With no `BootstrapDataReady` at all, banlieue-controller has never
    /// looked at this machine: it is not running, it does not serve the
    /// namespace (ADR-0130), or Cluster API does not serve v1beta2 Machines
    /// (the controller starts its bootstrap reconciler only then). The message
    /// names all three, instead of implying the data is merely late.
    #[test]
    fn a_machine_the_controller_never_reported_on_says_what_to_check() {
        let ready = |st: &CloudHypervisorMachineStatus| {
            st.conditions
                .iter()
                .find(|c| c.type_ == "Ready")
                .expect("Ready")
                .message
                .clone()
        };
        let never = held_status(None, 4);
        for cause in ["is running", "namespace selector", "v1beta2 Machines"] {
            assert!(ready(&never).contains(cause), "{cause}: {}", ready(&never));
        }

        let m = with_bootstrap_condition(&adopted(&as_capi_creates_it()), "False");
        let reported = held_status(m.status.as_ref(), 4);
        assert!(
            !ready(&reported).contains("namespace selector"),
            "{}",
            ready(&reported)
        );
        assert!(
            ready(&reported).contains("BootstrapDataReady"),
            "{}",
            ready(&reported)
        );
    }

    /// The provider and the controller share one predicate: a machine with a
    /// `providerID` and no status (after `clusterctl move` or a restore,
    /// which keep the spec and lose the status) has been created, so it is
    /// not held again, and the controller does not deliver to it again.
    #[test]
    fn a_machine_with_a_provider_id_is_not_held_after_losing_its_status() {
        let mut m = adopted(&as_capi_creates_it());
        m.spec.provider_id = Some("cloudhypervisor://host-1/x".to_string());
        assert!(m.status.is_none());
        assert!(!holds_guest(&m));
    }

    /// banlieue copies a VirtualMachine's labels onto its machine: a VM
    /// labelled with a cluster's name is still an ordinary VM, never held.
    #[test]
    fn a_virtualmachine_machine_with_the_cluster_label_is_not_held() {
        let mut m = machine("c", "banlieue-system", "ch-a", None);
        m.metadata.owner_references = Some(vec![owner(
            "banlieue.io/v1alpha1",
            "VirtualMachine",
            "lb-c",
        )]);
        m.metadata.labels = Some([(CLUSTER_NAME_LABEL.to_string(), "wl-1".to_string())].into());
        assert!(!holds_guest(&m));
    }

    /// The provider applies its whole status from a snapshot; it must never
    /// carry the controller's condition, or it would own that entry and could
    /// overwrite it with a stale value.
    #[test]
    fn the_status_patch_never_carries_the_controllers_condition() {
        let m = with_bootstrap_condition(&adopted(&as_capi_creates_it()), "True");
        let mut st = m.status.clone().expect("status");
        banlieue_provider_sdk::status::set_condition(
            &mut st.conditions,
            "Ready",
            "True",
            "Running",
            "",
            1,
        );
        let patch = status_patch(&st);
        let types: Vec<&str> = patch["status"]["conditions"]
            .as_array()
            .expect("conditions")
            .iter()
            .map(|c| c["type"].as_str().expect("type"))
            .collect();
        assert_eq!(types, ["Ready"]);
        assert_eq!(patch["kind"], "CloudHypervisorMachine");
    }
}
