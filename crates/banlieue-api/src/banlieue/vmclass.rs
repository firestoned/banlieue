// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue.io/v1alpha1` VMClass CRD.
//!
//! Cluster-scoped, analogous to Kubernetes `StorageClass`. Defines a tier of
//! VM hardware and the abstract capability requirements (storage class,
//! network class, features) that a Provider must satisfy for a VirtualMachine
//! using this class to be scheduled.

use crate::common::*;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "banlieue.io",
    version = "v1alpha1",
    kind = "VMClass",
    plural = "vmclasses",
    shortname = "vmc",
    derive = "PartialEq",
    printcolumn = r#"{"name":"CPUs","type":"integer","jsonPath":".spec.hardware.cpus"}"#,
    printcolumn = r#"{"name":"MemoryMiB","type":"integer","jsonPath":".spec.hardware.memoryMiB"}"#,
    printcolumn = r#"{"name":"Firmware","type":"string","jsonPath":".spec.firmware"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct VMClassSpec {
    pub hardware: HardwareSpec,
    pub network: NetworkSpec,

    /// Firmware. Providers / failure domains that lack support for the
    /// requested firmware are filtered out by the scheduler.
    #[serde(default)]
    pub firmware: Firmware,

    /// Required feature flags. The scheduler will only select a Provider
    /// + failure domain whose `features` is a superset of this list.
    ///
    /// Well-known values match those in `Provider.spec.capabilities.features`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HardwareSpec {
    /// Number of virtual CPUs.
    pub cpus: u32,

    /// Memory in MiB.
    pub memory_mi_b: u32,

    /// Disks in attachment order. The first disk is the OS disk and is
    /// backed by the VMImage resolved for the VirtualMachine; subsequent
    /// disks are blank and created with the requested size and storage class.
    pub disks: Vec<DiskSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DiskSpec {
    /// Stable name within the VM; used in status to report resolved
    /// backend identifiers.
    pub name: String,
    /// Size in GiB. For the OS disk this is the minimum size; if the image
    /// is larger, the provider grows accordingly.
    pub size_gi_b: u32,
    /// Abstract storage class name. MUST be advertised in the chosen
    /// Provider's `spec.capabilities.storageClasses`.
    pub storage_class: String,
    #[serde(default)]
    pub provisioning: DiskProvisioning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSpec {
    /// Network interfaces in attachment order.
    pub interfaces: Vec<NetworkInterfaceSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInterfaceSpec {
    /// Stable name within the VM.
    pub name: String,
    /// Abstract network class name. MUST be advertised in the chosen
    /// Provider's `spec.capabilities.networkClasses`.
    pub network_class: String,
    /// IPAM configuration. See `IpamSpec` in common.
    pub ipam: IpamSpec,
    /// Optional MTU override. Provider may ignore if unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u32>,
}

#[cfg(test)]
#[path = "vmclass_tests.rs"]
mod vmclass_tests;
