// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::metrics`]: phase derivation, counting, and
//! the encoded collectors.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use banlieue_api::banlieue::{
        FailureDomain, MigrationPolicy, PlacementSpec, Provider, ProviderCapabilities,
        ProviderConnection, ProviderSpec, ProviderStatus, VirtualMachine, VirtualMachineSpec,
        VirtualMachineStatus,
    };
    use banlieue_api::common::{LocalObjectReference, PowerState};
    use banlieue_provider_sdk::metrics::Metrics;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
    use k8s_openapi::jiff::Timestamp;
    use kube::core::ObjectMeta;
    use kube::runtime::reflector::{self, ObjectRef};
    use kube::runtime::watcher::Event;

    use super::super::*;

    /// A tenant-chosen name that must never reach a label.
    const TENANT_VM: &str = "tenant-payroll-db-0";
    const TENANT_NS: &str = "tenant-payroll";

    fn vm(name: &str) -> VirtualMachine {
        VirtualMachine {
            metadata: ObjectMeta {
                name: Some(name.into()),
                namespace: Some(TENANT_NS.into()),
                ..Default::default()
            },
            spec: VirtualMachineSpec {
                class_ref: LocalObjectReference { name: "c".into() },
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

    fn status(json: serde_json::Value) -> Option<VirtualMachineStatus> {
        Some(serde_json::from_value(json).expect("valid status"))
    }

    fn scheduled() -> serde_json::Value {
        serde_json::json!({
            "providerName": "prod-vsphere",
            "providerClass": "vsphere",
            "failureDomain": "fd-a",
        })
    }

    fn ready(value: &str) -> serde_json::Value {
        serde_json::json!([{
            "type": "Ready",
            "status": value,
            "reason": "R",
            "message": "",
            "lastTransitionTime": "2026-01-01T00:00:00Z",
        }])
    }

    fn provider(name: &str, class: &str, domains: usize) -> Provider {
        let spec = ProviderSpec {
            provider_class_ref: LocalObjectReference { name: class.into() },
            connection: ProviderConnection {
                endpoint: "https://vcenter.example.com/sdk".to_string(),
                credentials_ref: None,
                insecure_skip_tls_verify: false,
                ca_bundle: None,
            },
            capabilities: ProviderCapabilities::default(),
            paused: false,
            use_content_library: false,
            failure_domain_name_overrides: Vec::new(),
            attestation: None,
        };
        let mut p = Provider::new(name, spec);
        p.status = Some(ProviderStatus {
            failure_domains: (0..domains)
                .map(|i| FailureDomain {
                    name: format!("{name}-fd-{i}"),
                    labels: BTreeMap::new(),
                    attributes: Default::default(),
                })
                .collect(),
            ..Default::default()
        });
        p
    }

    // ---- vm_phase -----------------------------------------------------------

    #[test]
    fn a_fresh_vm_is_pending() {
        assert_eq!(vm_phase(&vm("a")), VmPhase::Pending);
    }

    #[test]
    fn a_placed_vm_is_provisioning_until_ready() {
        let mut v = vm("a");
        v.status =
            status(serde_json::json!({ "scheduled": scheduled(), "conditions": ready("False") }));
        assert_eq!(vm_phase(&v), VmPhase::Provisioning);

        v.status =
            status(serde_json::json!({ "scheduled": scheduled(), "conditions": ready("True") }));
        assert_eq!(vm_phase(&v), VmPhase::Ready);
    }

    #[test]
    fn paused_wins_over_ready() {
        let mut v = vm("a");
        v.spec.paused = true;
        v.status =
            status(serde_json::json!({ "scheduled": scheduled(), "conditions": ready("True") }));
        assert_eq!(vm_phase(&v), VmPhase::Paused);
    }

    #[test]
    fn deleting_wins_over_everything() {
        let mut v = vm("a");
        v.spec.paused = true;
        v.metadata.deletion_timestamp = Some(Time(Timestamp::UNIX_EPOCH));
        assert_eq!(vm_phase(&v), VmPhase::Deleting);
    }

    #[test]
    fn count_phases_zero_fills_every_phase() {
        let counts = count_phases(std::iter::empty());
        assert_eq!(counts.len(), VmPhase::ALL.len());
        assert!(counts.values().all(|c| *c == 0));
    }

    #[test]
    fn count_phases_counts_each_vm_once() {
        let mut ready_vm = vm("b");
        ready_vm.status =
            status(serde_json::json!({ "scheduled": scheduled(), "conditions": ready("True") }));
        let vms = [vm("a"), ready_vm, vm("c")];
        let counts = count_phases(vms.iter());
        assert_eq!(counts[&VmPhase::Pending], 2);
        assert_eq!(counts[&VmPhase::Ready], 1);
        assert_eq!(counts.values().sum::<i64>(), 3);
    }

    #[test]
    fn failure_domains_are_counted_per_provider_and_class() {
        let providers = [
            provider("prod-vsphere", "vsphere", 3),
            provider("lab-kvm", "libvirt", 1),
        ];
        let counts = failure_domain_counts(providers.iter());
        assert_eq!(counts[&("prod-vsphere".into(), "vsphere".into())], 3);
        assert_eq!(counts[&("lab-kvm".into(), "libvirt".into())], 1);
    }

    #[test]
    fn a_provider_without_status_reports_zero() {
        let mut p = provider("new", "proxmox", 0);
        p.status = None;
        let counts = failure_domain_counts([&p]);
        assert_eq!(counts[&("new".into(), "proxmox".into())], 0);
    }

    // ---- collectors ---------------------------------------------------------

    #[test]
    fn vm_collector_encodes_phase_series_without_names() {
        let (reader, mut writer) = reflector::store::<VirtualMachine>();
        writer.apply_watcher_event(&Event::Apply(vm(TENANT_VM)));
        let metrics = Metrics::new("banlieue-controller");
        metrics.register_collector(Box::new(VirtualMachineCollector::new(reader)));

        let text = metrics.encode().expect("encode");
        assert!(
            text.contains(r#"banlieue_virtualmachines{phase="Pending"} 1"#),
            "{text}"
        );
        assert!(
            text.contains(r#"banlieue_virtualmachines{phase="Ready"} 0"#),
            "{text}"
        );
        assert!(!text.contains(TENANT_VM), "{text}");
        assert!(!text.contains(TENANT_NS), "{text}");
    }

    #[test]
    fn vm_collector_follows_deletes() {
        let (reader, mut writer) = reflector::store::<VirtualMachine>();
        let v = vm("a");
        writer.apply_watcher_event(&Event::Apply(v.clone()));
        writer.apply_watcher_event(&Event::Delete(v.clone()));
        assert!(reader.get(&ObjectRef::from_obj(&v)).is_none());

        let metrics = Metrics::new("banlieue-controller");
        metrics.register_collector(Box::new(VirtualMachineCollector::new(reader)));
        let text = metrics.encode().expect("encode");
        assert!(
            text.contains(r#"banlieue_virtualmachines{phase="Pending"} 0"#),
            "{text}"
        );
    }

    #[test]
    fn failure_domain_collector_encodes_provider_and_kind() {
        let (reader, mut writer) = reflector::store::<Provider>();
        writer.apply_watcher_event(&Event::Apply(provider("prod-vsphere", "vsphere", 2)));
        let metrics = Metrics::new("banlieue-controller");
        metrics.register_collector(Box::new(FailureDomainCollector::new(reader)));

        let text = metrics.encode().expect("encode");
        assert!(
            text.contains(
                r#"banlieue_provider_failure_domains{provider="prod-vsphere",kind="vsphere"} 2"#
            ),
            "{text}"
        );
    }
}
