// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `infrastructure.banlieue.io/v1alpha1` VSphereMachine CRD.
//!
//! This is the reference implementation of the CAPI v1beta2 InfraMachine
//! contract for banlieue's vSphere provider. It is created by banlieue's
//! main controller once a VirtualMachine has been scheduled — all the
//! `template`, `datacenter`, `cluster`, `datastore` and per-NIC `portGroup`
//! fields here are concrete (already resolved from VMClass / VMImage /
//! Provider capabilities).
//!
//! Because this CRD complies with the CAPI InfraMachine contract, it can
//! also be used directly as a CAPI infrastructure provider — a `clusterv1.
//! Machine` with `infrastructureRef.kind: VSphereMachine` will work the
//! same way. The CAPI contract label
//! `cluster.x-k8s.io/v1beta2: v1alpha1` is emitted onto the generated CRD by
//! `crdgen` (`crdgen_support::add_capi_contract_label`), since `kube-derive`
//! cannot set CRD-level labels. See ADR-0005.

use crate::common::*;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "VSphereMachine",
    plural = "vspheremachines",
    shortname = "vsm",
    namespaced,
    status = "VSphereMachineStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"Provider","type":"string","jsonPath":".spec.providerRef.name"}"#,
    printcolumn = r#"{"name":"Provisioned","type":"boolean","jsonPath":".status.initialization.provisioned"}"#,
    printcolumn = r#"{"name":"ProviderID","type":"string","jsonPath":".spec.providerID","priority":1}"#,
    printcolumn = r#"{"name":"Cluster","type":"string","jsonPath":".spec.cluster","priority":1}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
/// VSphereMachine — the concrete, scheduled VM request for the vSphere backend.
///
/// This is banlieue's reference implementation of the CAPI v1beta2
/// InfraMachine contract. Where a VirtualMachine is abstract, every field here
/// is already resolved: a real vCenter `template`, `datacenter`, `cluster`,
/// `datastore`, and per-NIC `portGroup`. banlieue's main controller creates it
/// after scheduling; the vSphere provider reconciles it into a real VM and
/// reports CAPI-shaped status.
///
/// # Why it exists
///
/// - **It is the hand-off.** A VirtualMachine says "an ubuntu db-prod VM
///   somewhere reasonable"; a VSphereMachine says "clone template X in DC0 /
///   cluster C0 onto datastore ds-fast-01" — the thing a provider can actually
///   execute.
/// - **CAPI-compatible by contract.** Because it satisfies the InfraMachine
///   contract, it doubles as a Cluster API infrastructure provider: a
///   `clusterv1.Machine` with `infrastructureRef.kind: VSphereMachine` drives
///   it the same way.
///
/// You normally do not create this by hand — the controller does, owned by the
/// VirtualMachine. The CAPI contract label
/// `cluster.x-k8s.io/v1beta2: v1alpha1` is emitted onto the generated CRD by
/// `crdgen`, since kube-derive cannot set CRD-level labels (ADR-0005).
pub struct VSphereMachineSpec {
    // ------------------------------------------------------------------
    // CAPI v1beta2 contract fields
    // ------------------------------------------------------------------
    /// CAPI contract: Provider ID for the resulting Node, if this VM
    /// becomes a Kubernetes node. Format: `vsphere://<vm-instance-uuid>`.
    /// Set by the provider controller after the VM is created.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "providerID"
    )]
    pub provider_id: Option<String>,

    /// CAPI contract (optional): failure domain placement. The banlieue
    /// scheduler writes the chosen failure domain here; for CAPI users the
    /// parent Machine's `spec.failureDomain` is what populates this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    // ------------------------------------------------------------------
    // banlieue / vSphere-specific
    // ------------------------------------------------------------------
    /// Reference to the banlieue `Provider` whose connection details
    /// describe the target vCenter.
    pub provider_ref: LocalObjectReference,

    /// vCenter template name (resolved from `VMImage`).
    pub template: String,

    /// Datacenter name.
    pub datacenter: String,

    /// Compute cluster within the datacenter.
    pub cluster: String,

    /// Datastore or datastore cluster name (resolved from the storage class).
    pub datastore: String,

    /// VM folder path. Optional; defaults to the datacenter VM root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,

    /// Resource pool path within the cluster. Optional; defaults to the
    /// cluster's root resource pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_pool: Option<String>,

    /// Number of virtual CPUs.
    pub num_cpus: u32,

    /// Memory in MiB.
    pub memory_mi_b: u32,

    /// Firmware. EFI / EFI Secure require the template to be EFI-capable.
    pub firmware: Firmware,

    /// Disks. The first disk is the template's OS disk (grown if needed);
    /// subsequent disks are blank.
    pub disks: Vec<VSphereDiskSpec>,

    /// Network interfaces.
    pub network: Vec<VSphereNicSpec>,
}

/// One virtual disk on the resulting vSphere VM.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VSphereDiskSpec {
    /// Stable disk name; echoed in status with resolved backend identifiers.
    pub name: String,
    /// Disk size in GiB. For the OS disk this is a floor — the template's disk
    /// is grown to at least this size.
    pub size_gi_b: u32,
    /// Provisioning hint (thin / thick / eager-zeroed).
    #[serde(default)]
    pub provisioning: DiskProvisioning,
}

/// One virtual network interface on the resulting vSphere VM.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VSphereNicSpec {
    /// Stable NIC name; echoed in status.
    pub name: String,
    /// Resolved port group or distributed port group name.
    pub port_group: String,
    /// Optional MAC address (otherwise vCenter generates one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac_address: Option<String>,
    /// IP address management for this interface.
    pub ipam: IpamSpec,
}

// ----------------------------------------------------------------------
// Status — CAPI v1beta2 InfraMachine contract
// ----------------------------------------------------------------------

/// Observed state of a VSphereMachine, shaped to the CAPI v1beta2 InfraMachine
/// status contract (plus a few vSphere-specific diagnostics).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VSphereMachineStatus {
    /// CAPI contract field: replaces the deprecated v1beta1 `status.ready`.
    #[serde(default)]
    pub initialization: InitializationStatus,

    /// CAPI contract field (optional): observed failure domain.
    /// Surfaced to the parent Machine's `status.failureDomain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    /// CAPI contract field (optional): VM addresses. Surfaced to the parent
    /// Machine's `status.addresses` once initialization is complete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<MachineAddress>,

    /// VMware managed-object reference (vm-NNNN). Useful for operator
    /// diagnostics. Not part of the CAPI contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm_ref: Option<String>,

    /// VM instance UUID. Stable across vCenter restarts and the source for
    /// `spec.providerID`. Not part of the CAPI contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_uuid: Option<String>,

    /// CAPI-compatible conditions (using `metav1.Condition`). The `Ready`
    /// condition is mirrored as `InfrastructureReady` on the parent
    /// (`clusterv1.Machine` or banlieue `VirtualMachine`) per contract.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

// ----------------------------------------------------------------------
// VSphereMachineTemplate — required by CAPI for MachineDeployment use
// ----------------------------------------------------------------------

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "VSphereMachineTemplate",
    plural = "vspheremachinetemplates",
    shortname = "vsmt",
    namespaced,
    derive = "PartialEq"
)]
#[serde(rename_all = "camelCase")]
/// VSphereMachineTemplate — a stamped-out VSphereMachine spec.
///
/// CAPI requires an InfraMachineTemplate so higher-level controllers (a
/// MachineSet / MachineDeployment) can mint many identical machines from one
/// template. It wraps a single `template.spec` that is the VSphereMachine spec
/// used for each generated machine. banlieue ships it for CAPI compatibility;
/// standalone VirtualMachine users do not need it.
pub struct VSphereMachineTemplateSpec {
    /// The VSphereMachine spec stamped into every machine created from this
    /// template.
    pub template: VSphereMachineTemplateResource,
}

/// Wrapper matching CAPI's `template: { spec: {...} }` InfraMachineTemplate
/// shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VSphereMachineTemplateResource {
    /// The VSphereMachine spec for machines created from this template.
    pub spec: VSphereMachineSpec,
}

#[cfg(test)]
#[path = "vsphere_machine_tests.rs"]
mod vsphere_machine_tests;
