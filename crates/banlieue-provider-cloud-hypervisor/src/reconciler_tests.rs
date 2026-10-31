// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the pure decisions in `reconciler.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::machine::{Observed, Phase};
    use banlieue_api::common::{IpamSpec, LocalObjectReference, PowerState};
    use banlieue_api::infrastructure::{
        ChBootSource, ChBootSourceKind, ChCpuSpec, ChMemorySpec, CloudHypervisorMachineSpec,
    };
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
                provider_ref: LocalObjectReference {
                    name: provider.into(),
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
            },
            status: host_uid.map(|u| CloudHypervisorMachineStatus {
                host_uid: Some(u),
                ..Default::default()
            }),
        }
    }

    /// One Provider is one host (ADR-0060). Another host's machines, or ours
    /// in another namespace, are not touched.
    #[test]
    fn only_this_hosts_machines_are_ours() {
        let c = config();
        assert!(is_ours(&machine("a", "banlieue-system", "ch-a", None), &c));
        assert!(!is_ours(&machine("a", "banlieue-system", "ch-b", None), &c));
        assert!(!is_ours(&machine("a", "tenant", "ch-a", None), &c));
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
}
