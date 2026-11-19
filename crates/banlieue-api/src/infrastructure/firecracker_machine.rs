// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `infrastructure.banlieue.io/v1alpha1` FirecrackerMachine CRD (ADR-0137).
//!
//! banlieue's implementation of the CAPI v1beta2 InfraMachine contract for
//! the Firecracker backend, beside
//! [`CloudHypervisorMachine`](super::cloud_hypervisor_machine::CloudHypervisorMachine)
//! and not a `vmm` field on it: each class keeps its own declared
//! capabilities (non-negotiable 4). Created by banlieue's main controller once
//! a `VirtualMachine` has been scheduled onto a `firecracker` `Provider`, and
//! reconciled by the host-resident provider on that one host.
//!
//! # What differs from Cloud Hypervisor, and why
//!
//! - **A guest is a kernel and a read-only root, not a disk** (ADR-0137
//!   Decision 4, ADR-0142). There is no storage class, no OS disk size and no
//!   boot source: `image` names one digest-pinned OCI artifact and the host
//!   resolves it from its own pinned repository.
//! - **Declared absences** (ADR-0137 Decision 7). No vTPM, no hotplug, no
//!   device list, no live migration, no kernel arguments. The type carries no
//!   field for any of them, so a request cannot set one.
//! - **Identity is published, not asserted.** `status.attestationSubject` is
//!   the exact certificate subject the host's signer will use, the machine's
//!   own `metadata.uid` (ADR-0143 Decision 3).
//!
//! The CAPI contract label is emitted by `crdgen`, as for every
//! `infrastructure.banlieue.io` CRD (ADR-0005).

use crate::common::*;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// URI scheme of a Firecracker machine's `providerID`.
pub const FIRECRACKER_PROVIDER_ID_SCHEME: &str = "firecracker";

/// The `providerID` for a machine: `firecracker://<provider>/<machine-uid>`.
///
/// The provider name rather than a hostname (a hostname is a deployment
/// detail), and the machine UID because nothing on the host outlives the VMM
/// process; the same choice as Cloud Hypervisor (ADR-0062 Decision 2).
#[must_use]
pub fn firecracker_provider_id(provider_name: &str, machine_uid: &str) -> String {
    format!("{FIRECRACKER_PROVIDER_ID_SCHEME}://{provider_name}/{machine_uid}")
}

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "FirecrackerMachine",
    plural = "firecrackermachines",
    shortname = "fcm",
    namespaced,
    status = "FirecrackerMachineStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"Provider","type":"string","jsonPath":".spec.providerRef.name"}"#,
    printcolumn = r#"{"name":"Provisioned","type":"boolean","jsonPath":".status.initialization.provisioned"}"#,
    printcolumn = r#"{"name":"Power","type":"string","jsonPath":".status.observedPowerState"}"#,
    printcolumn = r#"{"name":"Image","type":"string","jsonPath":".spec.image","priority":1}"#,
    printcolumn = r#"{"name":"ProviderID","type":"string","jsonPath":".spec.providerID","priority":1}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
/// FirecrackerMachine: the concrete, scheduled microVM request for a
/// Firecracker host.
///
/// You normally do not create this by hand: banlieue's controller does, owned
/// by the `VirtualMachine` it was scheduled from, and the host-resident
/// provider on the chosen host runs it as a Firecracker guest.
pub struct FirecrackerMachineSpec {
    // ------------------------------------------------------------------
    // CAPI v1beta2 contract fields
    // ------------------------------------------------------------------
    /// CAPI contract: Provider ID for the resulting Node, if this VM becomes
    /// a Kubernetes node. Format: `firecracker://<provider-name>/<machine-uid>`.
    /// Set by the provider once the guest exists.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "providerID"
    )]
    pub provider_id: Option<String>,

    /// CAPI contract (optional): failure domain placement. One host is one
    /// failure domain on this backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    // ------------------------------------------------------------------
    // banlieue / Firecracker-specific
    // ------------------------------------------------------------------
    /// The `Provider` for the one host that runs this machine. Shared-capable
    /// from the first commit (ADR-0133): the namespace may name a `Provider`
    /// outside the machine's own, which only `banlieue-controller` may set.
    pub provider_ref: ProviderReference,

    /// Virtual CPUs. Fixed at boot: there is no hotplug (ADR-0137 Decision 7),
    /// so a `VMClass` change is a `Recreate`.
    #[schemars(range(min = 1, max = 32))]
    pub vcpus: u32,

    /// Guest memory in MiB. Fixed at boot, and must cover the image's RAM
    /// overlay as well as its working set (ADR-0142 Decision 4): the provider
    /// refuses a value below the image's declared minimum.
    #[serde(rename = "memoryMiB")]
    #[schemars(range(min = 128, max = 1_048_576))]
    pub memory_mi_b: u32,

    /// The image to boot: `<repository>@sha256:<manifest digest>`, the
    /// resolved `ref` of the `VMImage`'s `OciArtifact` source (ADR-0142
    /// Decision 2). The host pulls only a digest in the repository it pins;
    /// the cluster says which image, never where images may come from.
    pub image: String,

    /// Network interfaces. One tap each, on the host's private bridge for the
    /// network class (ADR-0137 Decision 5).
    #[schemars(length(max = 8))]
    pub nics: Vec<FcNicSpec>,

    /// Guest bootstrap payload, already resolved and placeholder-substituted
    /// by `banlieue-controller` (ADR-0025, ADR-0038). Rendered into a NoCloud
    /// seed drive, attached read-only second; the provider reads no Secret or
    /// ConfigMap (ADR-0137 Decision 4, ADR-0054).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_data: Option<String>,

    /// Desired power state, resolved from the parent `VirtualMachine`.
    #[serde(default)]
    pub desired_power_state: PowerState,
}

/// One network interface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FcNicSpec {
    /// Stable NIC name; echoed in status.
    pub name: String,
    /// Network class: a name the host resolves to one of its bridges. Never a
    /// bridge name from the cluster.
    pub network_class: String,
    /// Optional MAC address. The provider derives a stable one from the
    /// machine UID otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac_address: Option<String>,
    /// IP address management for this interface. The address and gateway are
    /// passed to the guest on its kernel command line (ADR-0137 Decision 5).
    pub ipam: IpamSpec,
}

// ----------------------------------------------------------------------
// Status: CAPI v1beta2 InfraMachine contract
// ----------------------------------------------------------------------

/// Observed state of a FirecrackerMachine, shaped to the CAPI v1beta2
/// InfraMachine status contract. The non-contract fields mean what they mean
/// on `CloudHypervisorMachineStatus`, so the controller's status mirror
/// treats both backends alike.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FirecrackerMachineStatus {
    /// CAPI contract field.
    #[serde(default)]
    pub initialization: InitializationStatus,

    /// CAPI contract field (optional): observed failure domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_domain: Option<String>,

    /// CAPI contract field (optional): VM addresses. Known before boot, from
    /// IPAM (ADR-0024, ADR-0033).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<MachineAddress>,

    /// The VM's last observed run state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_power_state: Option<PowerState>,

    /// Whether the guest has announced readiness over its vsock (ADR-0137
    /// Decision 6). Sticky once true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_installed: Option<bool>,

    /// The unprivileged host uid this machine's unit runs as (ADR-0063
    /// Decision 3). Recorded so the provider re-adopts a running guest without
    /// trusting its own memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_uid: Option<u32>,

    /// The exact certificate subject CN the host's signer sets for this
    /// guest: this machine's `metadata.uid`, never a value from the guest
    /// (ADR-0143 Decision 3). Public and carrying no authorization; mirrored
    /// onto the bound claim so mediatore can compute the node entry's parent
    /// before the guest boots. This class has no vTPM, so this replaces the
    /// EK certificate as the published attestation fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation_subject: Option<String>,

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
// FirecrackerMachineTemplate: required by CAPI for MachineDeployment use
// ----------------------------------------------------------------------

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "infrastructure.banlieue.io",
    version = "v1alpha1",
    kind = "FirecrackerMachineTemplate",
    plural = "firecrackermachinetemplates",
    shortname = "fcmt",
    namespaced,
    derive = "PartialEq"
)]
#[serde(rename_all = "camelCase")]
/// FirecrackerMachineTemplate: a stamped-out FirecrackerMachine spec.
///
/// CAPI requires an InfraMachineTemplate so a MachineSet or MachineDeployment
/// can mint identical machines. A spec template, not a disk template: it
/// carries no per-VM identity (ADR-0108).
pub struct FirecrackerMachineTemplateSpec {
    /// The spec stamped into every machine created from this template.
    pub template: FirecrackerMachineTemplateResource,
}

/// Wrapper matching CAPI's `template: { spec: {...} }` shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FirecrackerMachineTemplateResource {
    /// The FirecrackerMachine spec for machines created from this template.
    pub spec: FirecrackerMachineSpec,
}

#[cfg(test)]
#[path = "firecracker_machine_tests.rs"]
mod firecracker_machine_tests;
