// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `infrastructure.banlieue.io/v1alpha1` API group.
//!
//! CRDs in this group satisfy the CAPI v1beta2 contracts (InfraMachine and
//! InfraCluster), so they can be referenced from CAPI `Machine` /
//! `Cluster` objects as well as from banlieue's own VirtualMachine.

pub mod vsphere_cluster;
pub mod vsphere_machine;

pub use vsphere_cluster::{VSphereCluster, VSphereClusterSpec, VSphereClusterStatus};
pub use vsphere_machine::{
    VSphereDiskSpec, VSphereMachine, VSphereMachineSpec, VSphereMachineStatus,
    VSphereMachineTemplate, VSphereMachineTemplateResource, VSphereMachineTemplateSpec,
    VSphereNicSpec,
};
