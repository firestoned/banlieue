// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `infrastructure.banlieue.io/v1alpha1` AzureVirtualMachine CRD (ADR-0164).
//!
//! banlieue's implementation of the CAPI v1beta2 InfraMachine contract for
//! Azure virtual machines. Created by banlieue's main controller once a
//! `VirtualMachine` has been scheduled onto an `azure` `Provider`, and
//! reconciled by the Azure provider through Azure Resource Manager.
//!
//! # Shape
//!
//! Fully resolved, like [`ProxmoxMachine`](super::proxmox_machine::ProxmoxMachine):
//! the zone, size, image version, disk SKU and subnet are already chosen by
//! the controller from the `Provider`'s mappings (ADR-0160), so the provider
//! interprets none of them. A create is two ARM writes, the NIC and then the
//! VM, both named from the object's UID and tagged with its owner
//! (ADR-0158 Decision 4); the NIC and OS disk are deleted with the VM.
//!
//! Long-running ARM operations are never awaited inside a reconcile: the
//! provider records the operation in [`AzureVirtualMachineStatus::pending_operation`]
//! and observes it on the next one (ADR-0164 Decision 2).
//!
//! The CAPI contract label is emitted by `crdgen` (ADR-0005). The kind is
//! not `AzureMachine`, which Cluster API's Azure provider owns (ADR-0164
//! Decision 1).

use crate::common::*;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// URI scheme of an Azure machine's `providerID`, as CAPZ and the Azure
/// cloud provider write it.
pub const PROVIDER_ID_SCHEME: &str = "azure";

/// Largest `customData` Azure accepts, in bytes, before base64 (ADR-0163).
pub const MAX_USER_DATA_BYTES: usize = 64 * 1024;

/// Largest managed OS disk, GiB.
const MAX_OS_DISK_GIB: u32 = 32_767;

/// The `providerID` of an Azure VM:
/// `azure:///subscriptions/<sub>/resourceGroups/<rg>/providers/Microsoft.Compute/virtualMachines/<name>`
/// (ADR-0164 Decision 1).
#[must_use]
pub fn azure_provider_id(subscription_id: &str, resource_group: &str, vm_name: &str) -> String {
    format!(
        "{PROVIDER_ID_SCHEME}:///subscriptions/{subscription_id}/resourceGroups/{resource_group}/providers/Microsoft.Compute/virtualMachines/{vm_name}"
    )
}

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "AzureVirtualMachine",
    plural = "azurevirtualmachines",
    shortname = "azvm",
    namespaced,
    status = "AzureVirtualMachineStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"Provider","type":"string","jsonPath":".spec.providerRef.name"}"#,
    printcolumn = r#"{"name":"Zone","type":"string","jsonPath":".spec.zone"}"#,
    printcolumn = r#"{"name":"Size","type":"string","jsonPath":".spec.size"}"#,
    printcolumn = r#"{"name":"Provisioned","type":"boolean","jsonPath":".status.initialization.provisioned"}"#,
    printcolumn = r#"{"name":"State","type":"string","jsonPath":".status.provisioningState"}"#,
    printcolumn = r#"{"name":"Power","type":"string","jsonPath":".status.observedPowerState"}"#,
    printcolumn = r#"{"name":"ProviderID","type":"string","jsonPath":".spec.providerID","priority":1}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
/// AzureVirtualMachine: the concrete, scheduled VM request for Azure.
///
/// You normally do not create this by hand: banlieue's controller does,
/// owned by the `VirtualMachine` it was scheduled from, and the Azure
/// provider creates the NIC and VM and reports CAPI-shaped status.
pub struct AzureVirtualMachineSpec {
    // ------------------------------------------------------------------
    // CAPI v1beta2 contract fields
    // ------------------------------------------------------------------
    /// CAPI contract: Provider ID for the resulting Node. Format:
    /// `azure:///subscriptions/<sub>/resourceGroups/<rg>/providers/Microsoft.Compute/virtualMachines/<name>`.
    /// Set by the provider once the VM exists.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "providerID"
    )]
    pub provider_id: Option<String>,

    /// CAPI contract (optional): failure domain placement, the name of one of
    /// the `Provider`'s failure domains (one per availability zone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    // ------------------------------------------------------------------
    // banlieue / Azure-specific
    // ------------------------------------------------------------------
    /// The `Provider` whose connection names the subscription, region and
    /// resource group.
    pub provider_ref: ProviderReference,

    /// Availability zone the VM is created in, as the subscription names it
    /// (`1`, `2`, `3`). Resolved from the failure domain's `labels.zone`.
    #[schemars(length(min = 1))]
    pub zone: String,

    /// VM size (`Standard_D2s_v5`), resolved from the `Provider`'s
    /// `instanceSizes` mapping for the `VMClass` (ADR-0160).
    #[schemars(length(min = 1))]
    pub size: String,

    /// Resource ID of the Compute Gallery image version to boot, from the
    /// `VMImage`'s `azure` source (a hand-published `Template`, ADR-0162
    /// Decision 4).
    #[schemars(length(min = 1))]
    pub image_version_id: String,

    /// OS disk size in GiB, from the `VMClass` (ADR-0160 Decision 4).
    #[serde(rename = "osDiskSizeGiB")]
    #[schemars(range(min = 1, max = 32_767))]
    pub os_disk_size_gi_b: u32,

    /// Managed-disk SKU of the OS disk (`Premium_LRS`, `StandardSSD_LRS`),
    /// from the storage class's `sku` target.
    #[schemars(length(min = 1))]
    pub os_disk_sku: String,

    /// Resource ID of the subnet the NIC is created in, from the network
    /// class's `subnetId` target for this zone.
    #[schemars(length(min = 1))]
    pub subnet_id: String,

    /// Enable accelerated networking on the NIC. The image must not
    /// configure the VF itself (ADR-0163 Decision 3).
    #[serde(default, skip_serializing_if = "is_false")]
    pub accelerated_networking: bool,

    /// Trusted Launch: Secure Boot and a vTPM (ADR-0165). Set from the
    /// `VMClass`'s `tpmEnabled`; the image definition must declare Trusted
    /// Launch support.
    #[serde(default, skip_serializing_if = "is_false")]
    pub trusted_launch: bool,

    /// Guest bootstrap payload, already resolved and placeholder-substituted
    /// by `banlieue-controller` (ADR-0025, ADR-0038). Sent as `customData`;
    /// at most 64 KiB. The provider reads no Secret or ConfigMap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_data: Option<String>,

    /// Desired power state, resolved from the parent `VirtualMachine`.
    /// `PoweredOff` deallocates the VM, which stops compute billing
    /// (ADR-0164 Decision 3). `Suspended` is refused.
    #[serde(default)]
    pub desired_power_state: PowerState,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl AzureVirtualMachineSpec {
    /// Checks what the schema cannot: user data within Azure's `customData`
    /// limit, no `Suspended` power state, an OS disk size within range, and
    /// resource IDs that look like ARM resource IDs.
    ///
    /// # Errors
    /// A human-readable reason for the first rule broken.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(user_data) = &self.user_data
            && user_data.len() > MAX_USER_DATA_BYTES
        {
            return Err(format!(
                "userData is {} bytes; Azure customData allows {MAX_USER_DATA_BYTES}",
                user_data.len()
            ));
        }
        if self.desired_power_state == PowerState::Suspended {
            return Err("desiredPowerState Suspended is not supported on Azure".to_string());
        }
        if self.os_disk_size_gi_b == 0 || self.os_disk_size_gi_b > MAX_OS_DISK_GIB {
            return Err(format!(
                "osDiskSizeGiB must be between 1 and {MAX_OS_DISK_GIB}"
            ));
        }
        for (field, id) in [
            ("imageVersionId", &self.image_version_id),
            ("subnetId", &self.subnet_id),
        ] {
            if !id.starts_with("/subscriptions/") {
                return Err(format!(
                    "{field} must be an ARM resource ID starting with /subscriptions/"
                ));
            }
        }
        Ok(())
    }
}

/// An ARM long-running operation the provider started and has not yet seen
/// finish, kept on status so the next reconcile (or a restarted provider)
/// observes it instead of starting a second one (ADR-0164 Decision 2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AzureOperation {
    /// What the operation is doing.
    pub action: AzureOperationAction,
    /// The operation URL ARM returned. The provider polls it only when it
    /// sits under the ARM endpoint it is configured for.
    pub url: String,
    /// Which header the URL came from: `AsyncOperation`
    /// (`Azure-AsyncOperation`) or `Location`.
    pub kind: String,
}

/// The kind of write an [`AzureOperation`] tracks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AzureOperationAction {
    /// `PUT` of the NIC.
    CreateNic,
    /// `PUT` of the VM.
    CreateVm,
    /// Start a deallocated VM.
    Start,
    /// Deallocate the VM.
    Deallocate,
    /// `DELETE` of the VM (its NIC and OS disk go with it).
    DeleteVm,
    /// `DELETE` of a NIC left behind (a failed create, ADR-0158).
    DeleteNic,
    /// `DELETE` of an OS disk left behind.
    DeleteDisk,
}

/// Observed state of an AzureVirtualMachine, shaped to the CAPI v1beta2
/// InfraMachine status contract. The non-contract fields mean what they mean
/// on the siblings, so the controller's status mirror treats backends alike.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AzureVirtualMachineStatus {
    /// CAPI contract field.
    #[serde(default)]
    pub initialization: InitializationStatus,

    /// CAPI contract field (optional): observed failure domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    /// CAPI contract field (optional): the NIC's private address. No public
    /// address in v1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<MachineAddress>,

    /// The VM's resource name, derived from this object's UID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm_name: Option<String>,

    /// The ARM operation in flight, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_operation: Option<AzureOperation>,

    /// The VM's ARM `provisioningState` (`Creating`, `Succeeded`, `Failed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioning_state: Option<String>,

    /// Sticky: the guest reported ready once (ADR-0163 Decision 1,
    /// ADR-0043 Decision 5).
    #[serde(default, skip_serializing_if = "is_false")]
    pub guest_installed: bool,

    /// Consecutive failed creates of this generation: the provider waits
    /// `min(5 min * 2^(n - 1), 1 h)` before creating again (ADR-0173
    /// Decision 5). Reset when the VM exists.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub create_failures: u32,

    /// Deprecated, no longer written. v0.6.1 providers set it once they had
    /// merged the ownership tags onto the OS disk; ARM already creates the
    /// disk with the VM's tags, so the merge is gone (ADR-0173 as amended
    /// 2026-11-27). Kept so stored objects stay valid; a value already set
    /// is left as it is.
    #[serde(default, skip_serializing_if = "is_false")]
    pub os_disk_tagged: bool,

    /// The VM's last observed run state (deallocated reads `PoweredOff`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_power_state: Option<PowerState>,

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
// AzureVirtualMachineTemplate: required by CAPI for MachineDeployment use
// ----------------------------------------------------------------------

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "AzureVirtualMachineTemplate",
    plural = "azurevirtualmachinetemplates",
    shortname = "azvmt",
    namespaced,
    derive = "PartialEq"
)]
#[serde(rename_all = "camelCase")]
/// AzureVirtualMachineTemplate: a stamped-out AzureVirtualMachine spec.
///
/// CAPI requires an InfraMachineTemplate so a MachineSet or MachineDeployment
/// can mint identical machines. banlieue ships it for CAPI compatibility;
/// standalone VirtualMachine users do not need it.
pub struct AzureVirtualMachineTemplateSpec {
    /// The spec stamped into every machine created from this template.
    pub template: AzureVirtualMachineTemplateResource,
}

/// Wrapper matching CAPI's `template: { spec: {...} }` shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AzureVirtualMachineTemplateResource {
    /// The AzureVirtualMachine spec for machines created from this template.
    pub spec: AzureVirtualMachineSpec,
}

#[cfg(test)]
#[path = "azure_virtual_machine_tests.rs"]
mod azure_virtual_machine_tests;
