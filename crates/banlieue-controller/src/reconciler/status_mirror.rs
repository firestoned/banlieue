// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `VirtualMachine` status mirror.
//!
//! Pulls status fields from the provider's infrastructure CR ([`VSphereMachine`],
//! [`LibvirtMachine`], [`CloudHypervisorMachine`], [`ProxmoxMachine`],
//! [`AzureVirtualMachine`] or [`EC2Machine`]) and
//! projects them onto the parent [`VirtualMachine`]:
//!
//! - `status.initialization` ← infra.status.initialization
//! - `status.addresses` ← infra.status.addresses
//! - `status.tpmEndorsementCertificates` / `tpmEndorsementPublicKeys` ← the
//!   infra CR's (ADR-0045; ADR-0167 Decision 1 for EC2's public keys)
//! - `Ready` condition on the infra CR → `InfrastructureReady` on the VM
//! - Aggregate `Ready` = `Scheduled` && `PlacementValid` && `InfrastructureReady`
//!
//! The trait keeps the reconciler decoupled from provider-specific types.
//! Adding libvirt (ADR-0050) needed nothing here but the impl below — which
//! is the entire point of the trait, and why Proxmox (ADR-0075) is the same
//! twenty lines.

use banlieue_api::banlieue::{VirtualMachine, VirtualMachineStatus};
use banlieue_api::common::condition_types;
use banlieue_api::common::{InitializationStatus, MachineAddress, PowerState};
use banlieue_api::infrastructure::{
    AzureVirtualMachine, CloudHypervisorMachine, EC2Machine, FirecrackerMachine, LibvirtMachine,
    ProxmoxMachine, VSphereMachine,
};
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
    /// Last-observed backend power state (ADR-0034); `None` until the
    /// infra CR's own reconciler has read it at least once.
    fn observed_power_state(&self) -> Option<&PowerState>;
    /// PEM vTPM endorsement key certificate(s) this machine published
    /// (ADR-0045). Empty for a machine with no vTPM, and until its guest
    /// has produced one.
    fn tpm_endorsement_certificates(&self) -> &[String];
    /// PEM `SubjectPublicKeyInfo` endorsement key public keys this machine
    /// published where its backend offers no EK certificate (an
    /// `EC2Machine` with NitroTPM, ADR-0167 Decision 1). Empty for every
    /// other kind.
    fn tpm_endorsement_public_keys(&self) -> &[String] {
        &[]
    }
    /// The certificate subject CN the host's signer sets for a guest with no
    /// vTPM (ADR-0143 Decision 3). `None` for every kind that attests with a
    /// vTPM, which publishes the EK certificate instead.
    fn attestation_subject(&self) -> Option<&str> {
        None
    }
    /// The power state this infra CR's spec currently asks for. Read by the
    /// duplicate-address path (ADR-0083), which must still apply a VM's
    /// `desiredPowerState` to an infra CR it otherwise leaves untouched.
    fn desired_power_state(&self) -> &PowerState;
    /// The `Provider` this infra CR names in `spec.providerRef`: the host or
    /// backend the machine was placed on. `None` only for a test double.
    fn provider_name(&self) -> Option<&str> {
        None
    }
}

/// Implements [`InfraMachineRead`] for one infra CR kind. Every kind shares
/// the CAPI v1beta2 status shape, so the accessors are identical; only the
/// `tpm_endorsement_certificates` body differs and is supplied by the caller.
macro_rules! impl_infra_machine_read {
    ($kind:ty, $(#[$tpm_doc:meta])* |$this:ident| $tpm:expr, subject = |$subj_this:ident| $subj:expr, public_keys = |$pk_this:ident| $pk:expr) => {
        impl InfraMachineRead for $kind {
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

            fn observed_power_state(&self) -> Option<&PowerState> {
                self.status
                    .as_ref()
                    .and_then(|s| s.observed_power_state.as_ref())
            }

            $(#[$tpm_doc])*
            fn tpm_endorsement_certificates(&self) -> &[String] {
                let $this = self;
                $tpm
            }

            fn attestation_subject(&self) -> Option<&str> {
                let $subj_this = self;
                $subj
            }

            fn tpm_endorsement_public_keys(&self) -> &[String] {
                let $pk_this = self;
                $pk
            }

            fn desired_power_state(&self) -> &PowerState {
                &self.spec.desired_power_state
            }

            fn provider_name(&self) -> Option<&str> {
                Some(self.spec.provider_ref.name.as_str())
            }
        }
    };
    ($kind:ty, $(#[$tpm_doc:meta])* |$this:ident| $tpm:expr, subject = |$subj_this:ident| $subj:expr) => {
        impl_infra_machine_read!(
            $kind,
            $(#[$tpm_doc])* |$this| $tpm,
            subject = |$subj_this| $subj,
            public_keys = |_p| &[]
        );
    };
    ($kind:ty, $(#[$tpm_doc:meta])* |$this:ident| $tpm:expr) => {
        impl_infra_machine_read!($kind, $(#[$tpm_doc])* |$this| $tpm, subject = |_s| None);
    };
    ($kind:ty) => {
        impl_infra_machine_read!($kind, |this| this
            .status
            .as_ref()
            .map(|s| s.tpm_endorsement_certificates.as_slice())
            .unwrap_or(&[]));
    };
}

impl_infra_machine_read!(VSphereMachine);
impl_infra_machine_read!(LibvirtMachine);
impl_infra_machine_read!(
    CloudHypervisorMachine,
    /// A Cloud Hypervisor guest with a vTPM publishes its EK certificates;
    /// one on a host that opted in to x509pop also publishes
    /// `attestation_subject` (ADR-0146).
    |this| this
        .status
        .as_ref()
        .map(|s| s.tpm_endorsement_certificates.as_slice())
        .unwrap_or(&[]),
    subject = |this| this
        .status
        .as_ref()
        .and_then(|s| s.attestation_subject.as_deref())
);
impl_infra_machine_read!(
    FirecrackerMachine,
    /// Firecracker has no vTPM, so no EK certificate: the published
    /// attestation fact is `attestation_subject` instead (ADR-0143).
    |_this| &[],
    subject = |this| this
        .status
        .as_ref()
        .and_then(|s| s.attestation_subject.as_deref())
);
impl_infra_machine_read!(
    ProxmoxMachine,
    /// Proxmox publishes no EK certificate: its vTPM is a `tpmstate0`
    /// volume with no host-side certificate read (ADR-0075 Decision 6).
    |_this| &[]
);
impl_infra_machine_read!(
    AzureVirtualMachine,
    /// Azure exposes no EK certificate in v1: a Trusted Launch vTPM attests
    /// with its attestation key only (ADR-0165, AK-only).
    |_this| &[]
);
impl_infra_machine_read!(
    EC2Machine,
    /// EC2 offers no EK certificate: NitroTPM's EK is published as public
    /// keys read host-side from the EC2 API (ADR-0167 Decision 1).
    |_this| &[],
    subject = |_s| None,
    public_keys = |this| this
        .status
        .as_ref()
        .map(|s| s.tpm_endorsement_public_keys.as_slice())
        .unwrap_or(&[])
);

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
    next.observed_power_state = infra.observed_power_state().cloned();
    // ADR-0045. Mirrored onward onto a bound VirtualMachineClaim, so a
    // verifier gets the nonce and the anchor its quote must chain to from
    // one GET.
    next.tpm_endorsement_certificates = infra.tpm_endorsement_certificates().to_vec();
    // ADR-0167 Decision 1: where the backend publishes the EK as public keys
    // (EC2's NitroTPM), they ride beside the certificates, which stay empty.
    // Replaced, not merged, like the certificates.
    next.tpm_endorsement_public_keys = infra.tpm_endorsement_public_keys().to_vec();
    // ADR-0143 Decision 3: the counterpart for a guest with no vTPM. Replaced,
    // not merged, so a subject the machine withdrew does not linger.
    next.attestation_subject = infra.attestation_subject().map(str::to_string);

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

    // Mirror GuestReady verbatim — but ONLY when the provider publishes it.
    //
    // Absence is meaningful: `pool.rs::readiness_signal_absent` decides by
    // condition *type*, so a blanket `GuestReady=False` from a provider that
    // does not implement the signal would make a pool set to `GuestReady`
    // report `Filling` forever instead of `ReadinessSignalAbsent` — turning
    // "this will never warm" into "wait a little longer" (ADR-0043,
    // ADR-0046 Decision 3).
    if let Some(guest) = find_condition(infra.conditions(), condition_types::GUEST_READY) {
        set_condition(
            &mut next.conditions,
            condition_types::GUEST_READY,
            &guest.status,
            &guest.reason,
            guest.message.clone(),
            generation,
        );
    }

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
