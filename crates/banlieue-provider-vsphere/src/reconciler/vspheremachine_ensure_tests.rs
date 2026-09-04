// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::ensure_vm`].

#[cfg(test)]
mod tests {
    use banlieue_api::common::{
        DiskProvisioning, IpamSpec, LocalObjectReference, StaticIpamConfig,
    };
    use banlieue_api::common::{Firmware, PowerState};
    use banlieue_api::infrastructure::{VSphereDiskSpec, VSphereMachineSpec, VSphereNicSpec};

    use crate::client::{FakeClient, Inventory, VSphereClient};

    use super::super::ensure_vm;

    fn as_client(c: &FakeClient) -> &dyn VSphereClient {
        c
    }

    fn dhcp_nic(port_group: &str) -> VSphereNicSpec {
        VSphereNicSpec {
            name: "eth0".to_string(),
            port_group: port_group.to_string(),
            mac_address: None,
            ipam: IpamSpec::default(),
        }
    }

    fn spec(datastore: &str, network: &str) -> VSphereMachineSpec {
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
            datastore: datastore.to_string(),
            folder: None,
            resource_pool: None,
            num_cpus: 4,
            memory_mi_b: 8192,
            firmware: Firmware::Efi,
            disks: vec![VSphereDiskSpec {
                name: "os".to_string(),
                size_gi_b: 40,
                provisioning: DiskProvisioning::Thin,
            }],
            network: vec![dhcp_nic(network)],
            user_data: None,
            desired_power_state: PowerState::PoweredOn,
        }
    }

    fn seeded_inventory() -> Inventory {
        Inventory::builder()
            .with_dc("dc1")
            .with_cluster("dc1", "cluster-a")
            .with_template("dc1", "ubuntu-22.04-cloudinit")
            .with_datastore("dc1", "cluster-a", "ds-fast-01", None)
            .with_network("dc1", "cluster-a", "vmnet-prod", false)
            .build()
    }

    #[tokio::test]
    async fn already_provisioned_skips_clone_and_returns_the_existing_ref() {
        let client = FakeClient::new(seeded_inventory());
        let outcome = ensure_vm(
            as_client(&client),
            &spec("ds-fast-01", "vmnet-prod"),
            "db-01",
            Some("vm-existing-123"),
            None,
        )
        .await
        .unwrap();

        assert!(outcome.already_provisioned);
        assert_eq!(outcome.vm_ref, "vm-existing-123");
        assert!(client.cloned_vms().is_empty(), "must not re-clone");
        assert_eq!(
            outcome.power_state, None,
            "the already-provisioned early return performs no vCenter read of its own (ADR-0034: reconcile's separate refresh_power_state call handles that)"
        );
    }

    #[tokio::test]
    async fn first_reconcile_clones_and_powers_on() {
        let client = FakeClient::new(seeded_inventory());
        let outcome = ensure_vm(
            as_client(&client),
            &spec("ds-fast-01", "vmnet-prod"),
            "db-01",
            None,
            None,
        )
        .await
        .unwrap();

        assert!(!outcome.already_provisioned);
        let clones = client.cloned_vms();
        assert_eq!(clones.len(), 1);
        assert_eq!(clones[0].moref, outcome.vm_ref);
        assert_eq!(clones[0].request.vm_name, "db-01");
        // The clone request carries the datastore's moref, not its display
        // name (found live: passing the name as a ManagedObjectReference
        // value faults CloneVM_Task with ManagedObjectNotFound).
        assert_eq!(
            clones[0].request.datastore_moref,
            "datastore-cluster-a-ds-fast-01"
        );
        assert_eq!(
            client.power_state_of(&outcome.vm_ref),
            Some(PowerState::PoweredOn)
        );
        assert_eq!(outcome.power_state, Some(PowerState::PoweredOn));
    }

    #[tokio::test]
    async fn drives_the_desired_power_state_from_spec() {
        let client = FakeClient::new(seeded_inventory());
        let mut s = spec("ds-fast-01", "vmnet-prod");
        s.desired_power_state = PowerState::PoweredOff;
        let outcome = ensure_vm(as_client(&client), &s, "db-01", None, None)
            .await
            .unwrap();
        assert_eq!(
            client.power_state_of(&outcome.vm_ref),
            Some(PowerState::PoweredOff)
        );
        assert_eq!(outcome.power_state, Some(PowerState::PoweredOff));
    }

    #[tokio::test]
    async fn desired_power_off_skips_the_redundant_power_state_call() {
        // clone_vm always leaves a fresh clone PoweredOff (ADR-0024). Calling
        // set_power_state(PoweredOff) again is a no-op transition that real
        // vCenter rejects with InvalidPowerState; if that error propagated
        // via `?` before ensure_vm returned, the caller would never learn
        // the new vm_ref and would re-clone (hitting DuplicateName) on every
        // subsequent reconcile — found live testing ADR-0038 userData with
        // desiredPowerState: PoweredOff.
        let client = FakeClient::new(seeded_inventory());
        let mut s = spec("ds-fast-01", "vmnet-prod");
        s.desired_power_state = PowerState::PoweredOff;
        let outcome = ensure_vm(as_client(&client), &s, "db-01", None, None)
            .await
            .unwrap();

        assert_eq!(
            client.power_state_call_count(&outcome.vm_ref),
            0,
            "must not call set_power_state when the clone is already in the desired state"
        );
        assert_eq!(
            client.power_state_of(&outcome.vm_ref),
            Some(PowerState::PoweredOff)
        );
        assert_eq!(outcome.power_state, Some(PowerState::PoweredOff));
    }

    #[tokio::test]
    async fn resolves_a_datastore_cluster_to_a_concrete_member() {
        let inv = Inventory::builder()
            .with_dc("dc1")
            .with_cluster("dc1", "cluster-a")
            .with_template("dc1", "ubuntu-22.04-cloudinit")
            .with_datastore("dc1", "cluster-a", "ds-01", Some("DSC-01"))
            .with_datastore("dc1", "cluster-a", "ds-02", Some("DSC-01"))
            .with_network("dc1", "cluster-a", "vmnet-prod", false)
            .build();
        let client = FakeClient::new(inv);
        let outcome = ensure_vm(
            as_client(&client),
            &spec("DSC-01", "vmnet-prod"),
            "db-01",
            None,
            None,
        )
        .await
        .unwrap();
        assert!(!outcome.already_provisioned);
        let clones = client.cloned_vms();
        assert!(
            ["datastore-cluster-a-ds-01", "datastore-cluster-a-ds-02"]
                .contains(&clones[0].request.datastore_moref.as_str())
        );
    }

    #[tokio::test]
    async fn passes_static_guestinfo_and_rendered_userdata_into_the_clone_request() {
        let client = FakeClient::new(seeded_inventory());
        let mut s = spec("ds-fast-01", "vmnet-prod");
        s.network[0].ipam = IpamSpec {
            static_: Some(StaticIpamConfig {
                address: "10.0.0.90".to_string(),
                prefix: 24,
                gateway: Some("10.0.0.1".to_string()),
                nameservers: vec!["10.0.1.53".to_string()],
                domain: Some("k8s.example.internal".to_string()),
            }),
            ..Default::default()
        };
        let outcome = ensure_vm(
            as_client(&client),
            &s,
            "db-01",
            None,
            Some("#cloud-config\nhostname: db-01\n"),
        )
        .await
        .unwrap();

        let clones = client.cloned_vms();
        let extra: std::collections::HashMap<_, _> =
            clones[0].request.extra_config.iter().cloned().collect();
        assert_eq!(
            extra.get("guestinfo.network.ip"),
            Some(&"10.0.0.90".to_string())
        );
        assert!(extra.contains_key("guestinfo.userdata"));
        assert!(!outcome.already_provisioned);
    }

    #[tokio::test]
    async fn errors_when_datacenter_not_found() {
        let client = FakeClient::new(seeded_inventory());
        let mut s = spec("ds-fast-01", "vmnet-prod");
        s.datacenter = "does-not-exist".to_string();
        assert!(
            ensure_vm(as_client(&client), &s, "db-01", None, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn errors_when_cluster_not_found() {
        let client = FakeClient::new(seeded_inventory());
        let mut s = spec("ds-fast-01", "vmnet-prod");
        s.cluster = "does-not-exist".to_string();
        assert!(
            ensure_vm(as_client(&client), &s, "db-01", None, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn errors_when_template_not_found() {
        let client = FakeClient::new(seeded_inventory());
        let mut s = spec("ds-fast-01", "vmnet-prod");
        s.template = "does-not-exist".to_string();
        assert!(
            ensure_vm(as_client(&client), &s, "db-01", None, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn errors_when_network_not_found() {
        let client = FakeClient::new(seeded_inventory());
        let s = spec("ds-fast-01", "does-not-exist");
        assert!(
            ensure_vm(as_client(&client), &s, "db-01", None, None)
                .await
                .is_err()
        );
    }

    // ----------------------------------------------------------------------
    // template_folder scoping — the cross-zone collision fix
    // ----------------------------------------------------------------------

    #[tokio::test]
    async fn finds_the_template_in_its_own_zone_folder() {
        let inv = Inventory::builder()
            .with_dc("dc1")
            .with_cluster("dc1", "cluster-a")
            .with_template_in_folder("dc1", "templates/cluster-a", "hadron-kairos-v0.1.0")
            .with_datastore("dc1", "cluster-a", "ds-fast-01", None)
            .with_network("dc1", "cluster-a", "vmnet-prod", false)
            .build();
        let client = FakeClient::new(inv);
        let mut s = spec("ds-fast-01", "vmnet-prod");
        s.template = "hadron-kairos-v0.1.0".to_string();
        s.template_folder = Some("templates/cluster-a".to_string());

        let outcome = ensure_vm(as_client(&client), &s, "db-01", None, None)
            .await
            .unwrap();
        assert!(!outcome.already_provisioned);
    }

    #[tokio::test]
    async fn does_not_find_an_identically_named_template_in_a_different_zone_folder() {
        // The exact bug found live: every zone's template shares the same
        // display name (ADR-0020 Decision #5), so a lookup that isn't
        // folder-scoped could silently clone from the WRONG zone's
        // template instead of failing loudly. Seed the name only in
        // cluster-b's folder; cluster-a's zone must not find it.
        let inv = Inventory::builder()
            .with_dc("dc1")
            .with_cluster("dc1", "cluster-a")
            .with_template_in_folder("dc1", "templates/cluster-b", "hadron-kairos-v0.1.0")
            .with_datastore("dc1", "cluster-a", "ds-fast-01", None)
            .with_network("dc1", "cluster-a", "vmnet-prod", false)
            .build();
        let client = FakeClient::new(inv);
        let mut s = spec("ds-fast-01", "vmnet-prod");
        s.template = "hadron-kairos-v0.1.0".to_string();
        s.template_folder = Some("templates/cluster-a".to_string());

        let err = ensure_vm(as_client(&client), &s, "db-01", None, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn no_template_folder_falls_back_to_the_datacenter_wide_lookup() {
        // Template-kind images have no per-zone folder — the existing
        // datacenter-wide with_template seeding must still work.
        let client = FakeClient::new(seeded_inventory());
        let mut s = spec("ds-fast-01", "vmnet-prod");
        s.template_folder = None; // already the default; explicit for clarity
        let outcome = ensure_vm(as_client(&client), &s, "db-01", None, None)
            .await
            .unwrap();
        assert!(!outcome.already_provisioned);
    }
}
