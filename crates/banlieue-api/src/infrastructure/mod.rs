// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `infrastructure.banlieue.io/v1alpha1` API group.
//!
//! CRDs in this group satisfy the CAPI v1beta2 InfraMachine contract, so
//! they can be referenced as `infrastructureRef` from CAPI Machines as well
//! as from banlieue's own VirtualMachine.

pub mod vsphere_machine;

pub use vsphere_machine::{
    VSphereDiskSpec, VSphereMachine, VSphereMachineSpec, VSphereMachineStatus,
    VSphereMachineTemplate, VSphereMachineTemplateResource, VSphereMachineTemplateSpec,
    VSphereNicSpec,
};
