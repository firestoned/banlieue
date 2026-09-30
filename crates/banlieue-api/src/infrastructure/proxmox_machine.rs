// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `infrastructure.banlieue.io/v1alpha1` ProxmoxMachine CRD (ADR-0075).
//!
//! banlieue's implementation of the CAPI v1beta2 InfraMachine contract for
//! Proxmox VE. Created by banlieue's main controller once a `VirtualMachine`
//! has been scheduled onto a `proxmox` `Provider`, and reconciled by the
//! Proxmox provider through the REST API (ADR-0074).
//!
//! # Shape
//!
//! Fully resolved, like [`VSphereMachine`](super::vsphere_machine::VSphereMachine):
//! `node`, `storage`, each NIC's `bridge` and the `templateVmid` are already
//! chosen. They are names the Proxmox API validates against its own inventory
//! and the token's ACLs, so the provider interprets none of them.
//!
//! - **No `vmid`.** The provider allocates it and records it in
//!   `status.vmid` before cloning (ADR-0075 Decision 4).
//! - **Cloud-init is a NoCloud ISO**, uploaded to `isoStorage`, because the
//!   API cannot upload `cicustom` snippets (ADR-0075 Decision 5).
//!
//! The CAPI contract label is emitted by `crdgen` (ADR-0005).

use crate::common::*;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// URI scheme of a Proxmox machine's `providerID`.
pub const PROVIDER_ID_SCHEME: &str = "proxmox";

/// Data disks a machine may carry. Proxmox names SCSI disks `scsi0`..`scsi30`
/// and the OS disk takes `scsi0`.
pub const MAX_DATA_DISKS: usize = 30;

/// Lowest usable 802.1Q VLAN id (0 means "no tag" and 4095 is reserved).
const VLAN_MIN: u16 = 1;
/// Highest usable 802.1Q VLAN id.
const VLAN_MAX: u16 = 4094;

const fn default_sockets() -> u32 {
    1
}

/// The `providerID` for a machine: `proxmox://<provider-name>/<vmid>`
/// (ADR-0075 Decision 2).
///
/// The provider name rather than a node, because a hostname is a deployment
/// detail and a VM can move between nodes; the VMID because it is unique
/// cluster-wide and survives a move.
#[must_use]
pub fn proxmox_provider_id(provider_name: &str, vmid: u32) -> String {
    format!("{PROVIDER_ID_SCHEME}://{provider_name}/{vmid}")
}

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "ProxmoxMachine",
    plural = "proxmoxmachines",
    shortname = "pvem",
    namespaced,
    status = "ProxmoxMachineStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"Provider","type":"string","jsonPath":".spec.providerRef.name"}"#,
    printcolumn = r#"{"name":"Node","type":"string","jsonPath":".status.node"}"#,
    printcolumn = r#"{"name":"VMID","type":"integer","jsonPath":".status.vmid"}"#,
    printcolumn = r#"{"name":"Provisioned","type":"boolean","jsonPath":".status.initialization.provisioned"}"#,
    printcolumn = r#"{"name":"Power","type":"string","jsonPath":".status.observedPowerState"}"#,
    printcolumn = r#"{"name":"ProviderID","type":"string","jsonPath":".spec.providerID","priority":1}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
/// ProxmoxMachine — the concrete, scheduled VM request for the Proxmox backend.
///
/// You normally do not create this by hand: banlieue's controller does, owned
/// by the `VirtualMachine` it was scheduled from, and the Proxmox provider
/// clones the template into a VM and reports CAPI-shaped status.
pub struct ProxmoxMachineSpec {
    // ------------------------------------------------------------------
    // CAPI v1beta2 contract fields
    // ------------------------------------------------------------------
    /// CAPI contract: Provider ID for the resulting Node, if this VM becomes
    /// a Kubernetes node. Format: `proxmox://<provider-name>/<vmid>`. Set by
    /// the provider once the VM exists.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "providerID"
    )]
    pub provider_id: Option<String>,

    /// CAPI contract (optional): failure domain placement. One node is one
    /// failure domain (roadmap 06).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    // ------------------------------------------------------------------
    // banlieue / Proxmox-specific
    // ------------------------------------------------------------------
    /// The `Provider` whose connection describes the target Proxmox cluster.
    pub provider_ref: LocalObjectReference,

    /// Node the VM is created on (resolved from the failure domain).
    pub node: String,

    /// VMID of the template VM to clone. Must be a template (ADR-0075
    /// Decision 5a); the provider checks before cloning.
    pub template_vmid: u32,

    /// Storage id for the cloned and new disks (resolved from the storage
    /// class). Must allow `images` content.
    pub storage: String,

    /// Proxmox resource pool to place the VM in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,

    /// Cores per socket.
    #[schemars(range(min = 1, max = 512))]
    pub cores: u32,

    /// CPU sockets.
    #[serde(default = "default_sockets")]
    #[schemars(range(min = 1, max = 4))]
    pub sockets: u32,

    /// Memory in MiB.
    #[serde(rename = "memoryMiB")]
    #[schemars(range(min = 16, max = 4_194_304))]
    pub memory_mi_b: u32,

    /// CPU model (`host`, `x86-64-v2-AES`, …). Proxmox's default applies when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_type: Option<String>,

    /// Firmware: `bios` selects SeaBIOS; `efi` and `efi-secure` select OVMF,
    /// the latter with Microsoft/distribution keys pre-enrolled. The template
    /// must be able to boot the chosen firmware.
    #[serde(default)]
    pub firmware: Firmware,

    /// Attach a vTPM 2.0 (`tpmstate0` on `storage`), resolved from
    /// `VMClass.spec.tpmEnabled` and attached before first boot.
    #[serde(default)]
    pub tpm_enabled: bool,

    /// OS disk size in GiB: the cloned disk is grown to at least this, never
    /// shrunk.
    #[serde(rename = "osDiskSizeGiB")]
    #[schemars(range(min = 1, max = 65_536))]
    pub os_disk_size_gi_b: u32,

    /// Additional blank disks, attached after the OS disk.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 30))]
    pub data_disks: Vec<ProxmoxDataDisk>,

    /// Network interfaces.
    #[schemars(length(max = 16))]
    pub nics: Vec<ProxmoxNicSpec>,

    /// Storage the NoCloud seed ISO is uploaded to. Must allow `iso` content.
    /// Required whenever `userData` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iso_storage: Option<String>,

    /// Guest bootstrap payload, already resolved and placeholder-substituted
    /// by `banlieue-controller` (ADR-0025, ADR-0038). Rendered into a NoCloud
    /// `CIDATA` ISO (ADR-0054); the provider reads no Secret or ConfigMap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_data: Option<String>,

    /// Desired power state, resolved from the parent `VirtualMachine`.
    #[serde(default)]
    pub desired_power_state: PowerState,
}

impl ProxmoxMachineSpec {
    /// The value for the VM's `bios` config key.
    #[must_use]
    pub fn bios(&self) -> &'static str {
        match self.firmware {
            Firmware::Bios => "seabios",
            Firmware::Efi | Firmware::EfiSecure => "ovmf",
        }
    }

    /// Whether the EFI disk enrols the default Secure Boot keys.
    #[must_use]
    pub fn secure_boot(&self) -> bool {
        self.firmware == Firmware::EfiSecure
    }

    /// Whether a NoCloud seed ISO must be built and attached.
    #[must_use]
    pub fn needs_seed(&self) -> bool {
        self.user_data.is_some()
    }

    /// Reject specs the CRD schema cannot: cross-field and uniqueness rules.
    ///
    /// # Errors
    /// A message naming the offending field.
    pub fn validate(&self) -> Result<(), String> {
        if self.needs_seed() && self.iso_storage.is_none() {
            return Err("isoStorage must be set when userData is set".to_string());
        }
        if self.data_disks.len() > MAX_DATA_DISKS {
            return Err(format!("at most {MAX_DATA_DISKS} dataDisks are supported"));
        }
        let mut seen = HashSet::new();
        for d in &self.data_disks {
            if !seen.insert(d.name.as_str()) {
                return Err(format!("duplicate dataDisk name {:?}", d.name));
            }
        }
        let mut seen = HashSet::new();
        for n in &self.nics {
            if !seen.insert(n.name.as_str()) {
                return Err(format!("duplicate nic name {:?}", n.name));
            }
            if let Some(v) = n.vlan
                && !(VLAN_MIN..=VLAN_MAX).contains(&v)
            {
                return Err(format!(
                    "nic {:?}: vlan {v} outside {VLAN_MIN}..={VLAN_MAX}",
                    n.name
                ));
            }
        }
        Ok(())
    }
}

/// One additional disk on the resulting VM.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProxmoxDataDisk {
    /// Stable disk name; echoed in status.
    pub name: String,
    /// Size in GiB.
    #[serde(rename = "sizeGiB")]
    #[schemars(range(min = 1, max = 65_536))]
    pub size_gi_b: u32,
    /// Storage id; defaults to the machine's `storage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
    /// Dedicated I/O thread (virtio-scsi-single).
    #[serde(default)]
    pub iothread: bool,
    /// Pass TRIM/discard to the storage.
    #[serde(default)]
    pub discard: bool,
    /// Present as an SSD to the guest.
    #[serde(default)]
    pub ssd: bool,
}

/// Emulated NIC model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProxmoxNicModel {
    /// Paravirtual; the default.
    #[default]
    Virtio,
    /// Intel e1000.
    E1000,
    /// Realtek RTL8139.
    Rtl8139,
    /// VMware vmxnet3.
    Vmxnet3,
}

impl ProxmoxNicModel {
    /// The token Proxmox uses in a `netN` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Virtio => "virtio",
            Self::E1000 => "e1000",
            Self::Rtl8139 => "rtl8139",
            Self::Vmxnet3 => "vmxnet3",
        }
    }
}

/// One network interface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProxmoxNicSpec {
    /// Stable NIC name; echoed in status.
    pub name: String,
    /// Bridge on the node (resolved from the network class), e.g. `vmbr0`.
    pub bridge: String,
    /// 802.1Q VLAN tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 4094))]
    pub vlan: Option<u16>,
    /// Emulated NIC model.
    #[serde(default)]
    pub model: ProxmoxNicModel,
    /// Optional MAC address (Proxmox generates one otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac_address: Option<String>,
    /// IP address management for this interface.
    pub ipam: IpamSpec,
}

impl ProxmoxNicSpec {
    /// The value for the VM's `netN` config key, e.g.
    /// `virtio=BC:24:11:00:00:01,bridge=vmbr0,tag=30`.
    #[must_use]
    pub fn config_value(&self) -> String {
        let mut v = self.model.as_str().to_string();
        if let Some(mac) = &self.mac_address {
            v.push('=');
            v.push_str(mac);
        }
        v.push_str(",bridge=");
        v.push_str(&self.bridge);
        if let Some(tag) = self.vlan {
            v.push_str(&format!(",tag={tag}"));
        }
        v
    }
}

// ----------------------------------------------------------------------
// Status — CAPI v1beta2 InfraMachine contract
// ----------------------------------------------------------------------

/// Which source produced a machine's addresses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ProxmoxAddressSource {
    /// Known before boot, from IPAM (ADR-0024, ADR-0033).
    Static,
    /// The QEMU guest agent (`agent/network-get-interfaces`).
    GuestAgent,
}

/// Observed state of a ProxmoxMachine, shaped to the CAPI v1beta2 InfraMachine
/// status contract. The non-contract fields mean what they mean on the
/// siblings, so the controller's status mirror treats backends alike.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProxmoxMachineStatus {
    /// CAPI contract field.
    #[serde(default)]
    pub initialization: InitializationStatus,

    /// CAPI contract field (optional): observed failure domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    /// CAPI contract field (optional): VM addresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<MachineAddress>,

    /// Which source produced [`addresses`](Self::addresses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_source: Option<ProxmoxAddressSource>,

    /// The VMID, recorded before the clone so a crash cannot allocate a
    /// second VM (ADR-0075 Decision 4). A cache, not the source of truth: the
    /// VM's name is the ownership marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vmid: Option<u32>,

    /// The node the VM was last seen on; a migration changes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,

    /// The VM's last observed run state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_power_state: Option<PowerState>,

    /// Whether a vTPM was attached, when `spec.tpmEnabled` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpm_attached: Option<bool>,

    /// Standard Kubernetes conditions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend(
        "x-kubernetes-list-type" = "map",
        "x-kubernetes-list-map-keys" = ["type"],
    ))]
    pub conditions: Vec<Condition>,

    /// The `metadata.generation` this status was computed from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

// ----------------------------------------------------------------------
// ProxmoxMachineTemplate — required by CAPI for MachineDeployment use
// ----------------------------------------------------------------------

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "ProxmoxMachineTemplate",
    plural = "proxmoxmachinetemplates",
    shortname = "pvemt",
    namespaced,
    derive = "PartialEq"
)]
#[serde(rename_all = "camelCase")]
/// ProxmoxMachineTemplate — a stamped-out ProxmoxMachine spec.
///
/// CAPI requires an InfraMachineTemplate so a MachineSet or MachineDeployment
/// can mint identical machines. banlieue ships it for CAPI compatibility;
/// standalone VirtualMachine users do not need it.
pub struct ProxmoxMachineTemplateSpec {
    /// The spec stamped into every machine created from this template.
    pub template: ProxmoxMachineTemplateResource,
}

/// Wrapper matching CAPI's `template: { spec: {...} }` shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProxmoxMachineTemplateResource {
    /// The ProxmoxMachine spec for machines created from this template.
    pub spec: ProxmoxMachineSpec,
}

#[cfg(test)]
#[path = "proxmox_machine_tests.rs"]
mod proxmox_machine_tests;
