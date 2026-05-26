// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VirtualMachine` status mirror.
//!
//! Pulls status fields from the provider's infrastructure CR (currently
//! [`VSphereMachine`], soon Proxmox + Libvirt counterparts) and projects them
//! onto the parent [`VirtualMachine`]:
//!
//! - `status.initialization` ← infra.status.initialization
//! - `status.addresses` ← infra.status.addresses
//! - `Ready` condition on the infra CR → `InfrastructureReady` on the VM
//! - Aggregate `Ready` = `Scheduled` && `PlacementValid` && `InfrastructureReady`
//!
//! The trait keeps the reconciler decoupled from provider-specific types so
//! Phase 1C / 1D can add `ProxmoxMachine` / `LibvirtMachine` impls without
//! touching the reconciler.

use banlieue_api::banlieue::{VirtualMachine, VirtualMachineStatus};
use banlieue_api::common::condition_types;
use banlieue_api::common::{InitializationStatus, MachineAddress};
use banlieue_api::infrastructure::VSphereMachine;
use banlieue_provider_sdk::status::{
    condition_status, find_condition, is_condition_true, set_condition,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;

/// Provider-agnostic accessor over an InfraMachine.
///
/// Every provider's infra CR implements this so [`mirror_status_from_infra`]
/// doesn't need to know about the concrete type.
pub trait InfraMachineRead {
    /// Current `status.initialization` (CAPI v1beta2 contract).
    fn initialization(&self) -> &InitializationStatus;
    /// Current `status.addresses` (CAPI v1beta2 contract).
    fn addresses(&self) -> &[MachineAddress];
    /// Observed failure domain (provider-side; may diverge briefly during
    /// migration).
    fn failure_domain(&self) -> Option<&str>;
    /// CAPI `providerID` (e.g. `vsphere://<vm-instance-uuid>`).
    fn provider_id(&self) -> Option<&str>;
    /// Conditions on the infra CR.
    fn conditions(&self) -> &[Condition];
}

impl InfraMachineRead for VSphereMachine {
    fn initialization(&self) -> &InitializationStatus {
        self.status
            .as_ref()
            .map(|s| &s.initialization)
            .unwrap_or(&NO_INIT)
    }

    fn addresses(&self) -> &[MachineAddress] {
        self.status
            .as_ref()
            .map(|s| s.addresses.as_slice())
            .unwrap_or(&[])
    }

    fn failure_domain(&self) -> Option<&str> {
        self.status
            .as_ref()
            .and_then(|s| s.failure_domain.as_deref())
    }

    fn provider_id(&self) -> Option<&str> {
        self.spec.provider_id.as_deref()
    }

    fn conditions(&self) -> &[Condition] {
        self.status
            .as_ref()
            .map(|s| s.conditions.as_slice())
            .unwrap_or(&[])
    }
}

// Stable empty status fallbacks so accessors can return references even when
// the infra CR has not produced a `status` yet (initial reconciles).
static NO_INIT: InitializationStatus = InitializationStatus { provisioned: None };

/// Project an infra CR's status onto a parent [`VirtualMachine`] status.
///
/// Returns a *new* [`VirtualMachineStatus`] derived from the current one — the
/// caller is responsible for SSA-patching it back. This keeps the function
/// pure and unit-testable.
///
/// `generation` is the parent VM's `metadata.generation`, written into every
/// condition's `observedGeneration`.
pub fn mirror_status_from_infra(
    current: &VirtualMachineStatus,
    infra: &dyn InfraMachineRead,
    generation: i64,
) -> VirtualMachineStatus {
    let mut next = current.clone();

    // Mirror the simple fields.
    next.initialization = infra.initialization().clone();
    next.addresses = infra.addresses().to_vec();

    // Mirror Ready → InfrastructureReady.
    let infra_ready = is_condition_true(infra.conditions(), condition_types::READY);
    let infra_ready_status = if infra_ready {
        condition_status::TRUE
    } else {
        condition_status::FALSE
    };

    let (infra_reason, infra_message) =
        match find_condition(infra.conditions(), condition_types::READY) {
            Some(c) => (c.reason.as_str(), c.message.as_str()),
            None => (
                "Pending",
                "infrastructure has not reported a Ready condition yet",
            ),
        };

    set_condition(
        &mut next.conditions,
        condition_types::INFRASTRUCTURE_READY,
        infra_ready_status,
        infra_reason,
        infra_message.to_string(),
        generation,
    );

    // Aggregate Ready.
    let scheduled = is_condition_true(&next.conditions, condition_types::SCHEDULED);
    let placement_valid = !find_condition(&next.conditions, condition_types::PLACEMENT_VALID)
        .map(|c| c.status == condition_status::FALSE)
        .unwrap_or(false);
    let ready = scheduled && placement_valid && infra_ready;

    let (ready_status, ready_reason, ready_msg) = if ready {
        (
            condition_status::TRUE,
            "Reconciled",
            "VirtualMachine reconciled successfully",
        )
    } else if !scheduled {
        (
            condition_status::FALSE,
            "Scheduling",
            "scheduling not yet successful",
        )
    } else if !placement_valid {
        (
            condition_status::FALSE,
            "PlacementInvalid",
            "current placement no longer satisfies the spec",
        )
    } else {
        (
            condition_status::FALSE,
            "InfrastructureNotReady",
            "waiting for the infrastructure CR to report Ready",
        )
    };

    set_condition(
        &mut next.conditions,
        condition_types::READY,
        ready_status,
        ready_reason,
        ready_msg.to_string(),
        generation,
    );

    next.observed_generation = Some(generation);
    next
}

/// Convenience wrapper: mirror directly onto a [`VirtualMachine`] copy.
pub fn mirror_onto_vm(vm: &VirtualMachine, infra: &dyn InfraMachineRead) -> VirtualMachineStatus {
    let current = vm.status.clone().unwrap_or_default();
    let generation = vm.metadata.generation.unwrap_or(0);
    mirror_status_from_infra(&current, infra, generation)
}

#[cfg(test)]
#[path = "status_mirror_tests.rs"]
mod status_mirror_tests;
