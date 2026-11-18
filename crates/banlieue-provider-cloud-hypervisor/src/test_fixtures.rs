// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Fixtures the unit tests of several modules share: one host config and one
//! machine spec. A test that needs a different value overrides the field at
//! its call site, so what makes it different stays visible there.

use crate::host_config::{HostConfig, TpmSection};
use banlieue_api::common::{IpamSpec, PowerState, ProviderReference};
use banlieue_api::infrastructure::{
    ChBootSource, ChBootSourceKind, ChCpuSpec, ChMemorySpec, ChNicSpec, CloudHypervisorMachineSpec,
};

/// A valid host config: Provider `ch-a`, storage class `fast`, network class
/// `lan` on `br0`, ten guest uids, no `[tpm]`.
pub(crate) fn config() -> HostConfig {
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

/// `config` with a `[tpm]` section, as on a host that offers vTPMs.
pub(crate) fn with_tpm(config: HostConfig) -> HostConfig {
    HostConfig {
        tpm: Some(TpmSection {
            swtpm: "/usr/bin/swtpm".into(),
            swtpm_setup: "/usr/bin/swtpm_setup".into(),
            setup_config: "/etc/banlieue/swtpm/swtpm_setup.conf".into(),
            ek_ca_certificate: "/var/lib/banlieue/swtpm-localca/issuercert.pem".into(),
        }),
        ..config
    }
}

/// A two-CPU, 2 GiB machine on Provider `ch-a` with one NIC on `lan`, booting
/// `kairos.raw` from storage class `fast`, no vTPM.
pub(crate) fn spec() -> CloudHypervisorMachineSpec {
    CloudHypervisorMachineSpec {
        provider_id: None,
        failure_domain: Some("ch-a".into()),
        provider_ref: ProviderReference {
            name: "ch-a".into(),
            namespace: None,
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
        first_boot_sealing: None,
        identity: None,
        move_in: None,
        move_out: None,
    }
}
