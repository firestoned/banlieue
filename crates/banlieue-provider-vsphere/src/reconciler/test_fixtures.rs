// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Fixtures the reconciler unit tests share: the machine every
//! `VSphereMachine` test provisions, and the `FakeClient` coercion.

use banlieue_api::common::{DiskProvisioning, Firmware, IpamSpec, PowerState, ProviderReference};
use banlieue_api::infrastructure::{VSphereDiskSpec, VSphereMachineSpec, VSphereNicSpec};

use super::vspheremachine::MachineRef;
use crate::client::{FakeClient, VSphereClient};

/// `metadata.uid` of the machine under test.
pub(crate) const UID: &str = "7c0d1f9e-0000-4000-8000-0000000000db";

/// The machine under test: `db-01`, with [`UID`].
pub(crate) fn machine_ref() -> MachineRef<'static> {
    MachineRef {
        name: "db-01",
        uid: UID,
    }
}

/// Borrow a [`FakeClient`] as the trait object the reconcilers take.
pub(crate) fn as_client(c: &FakeClient) -> &dyn VSphereClient {
    c
}

/// A DHCP interface `eth0` on `port_group`.
pub(crate) fn dhcp_nic(port_group: &str) -> VSphereNicSpec {
    VSphereNicSpec {
        name: "eth0".to_string(),
        port_group: port_group.to_string(),
        mac_address: None,
        ipam: IpamSpec::default(),
    }
}

/// A 4-CPU, 8 GiB machine cloned from `ubuntu-22.04-cloudinit` onto
/// `dc1`/`cluster-a`/`ds-fast-01`, with one 40 GiB disk and one DHCP NIC on
/// `vmnet-prod`. A test changes what it is about on the returned value.
pub(crate) fn base_spec() -> VSphereMachineSpec {
    VSphereMachineSpec {
        provider_id: None,
        failure_domain: None,
        provider_ref: ProviderReference {
            name: "vc1".to_string(),
            namespace: None,
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
        network: vec![dhcp_nic("vmnet-prod")],
        user_data: None,
        desired_power_state: PowerState::PoweredOn,
        created_by: None,
        first_boot_sealing: None,
    }
}
