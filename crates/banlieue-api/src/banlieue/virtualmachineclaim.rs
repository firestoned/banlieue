// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue.io/v1alpha1` VirtualMachineClaim CRD (roadmap 17, ADR-0047).
//!
//! A claim is the only way a member ever leaves a [`VirtualMachinePool`],
//! and it takes exactly one member, for exactly one subject, exactly once.
//! There is no unbind and no return-to-pool: the VM *is* the isolation
//! boundary between subjects, so a member that has been exposed to one can
//! never be given to another. Releasing a claim destroys the VM.
//!
//! The claim is backend-neutral, like the pool. It binds a
//! `VirtualMachine`; which provider realises that VM is invisible here.
//!
//! [`VirtualMachinePool`]: super::VirtualMachinePool

use crate::common::{LocalObjectReference, MachineAddress};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Finalizer the claim controller holds on a `VirtualMachineClaim` so the
/// bound member is always deleted before the claim object disappears.
///
/// This is what makes "claim deleted" mean "sandbox destroyed" (ADR-0047
/// Decision 6). The name is API: changing it strands every claim already
/// carrying the old one, because nothing would be left to remove it.
pub const CLAIM_FINALIZER: &str = "banlieue.io/claim-protection";

/// Annotation mirrored onto a bound member: the claim subject's issuer.
pub const ANNOTATION_SUBJECT_ISSUER: &str = "banlieue.io/claim-subject-issuer";
/// Annotation mirrored onto a bound member: the claim subject's id.
///
/// Recorded on the member so that "who was this VM for" survives on the
/// object an operator actually looks at when investigating one, rather than
/// only on a claim that may already have been garbage-collected.
pub const ANNOTATION_SUBJECT_ID: &str = "banlieue.io/claim-subject-id";

/// Bits of randomness in [`VirtualMachineClaimStatus::nonce`].
///
/// 128 is the usual floor for a value whose only job is to be unguessable
/// within the lifetime of one claim; the nonce is not secret and is not a
/// key, so nothing here needs more.
pub const CLAIM_NONCE_BITS: usize = 128;
/// Byte length of the generated nonce.
pub const CLAIM_NONCE_BYTES: usize = CLAIM_NONCE_BITS / 8;

#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "banlieue.io",
    version = "v1alpha1",
    kind = "VirtualMachineClaim",
    plural = "virtualmachineclaims",
    shortname = "vmclaim",
    namespaced,
    status = "VirtualMachineClaimStatus",
    derive = "PartialEq",
    printcolumn = r#"{"name":"Pool","type":"string","jsonPath":".spec.poolRef.name"}"#,
    printcolumn = r#"{"name":"VM","type":"string","jsonPath":".status.virtualMachineRef.name"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    // `string`, not `date`: kubectl renders a `date` column as time SINCE
    // the timestamp, which is negative for a deadline and prints
    // `<invalid>`. `Age` below is a `date` and correctly so — it looks
    // backwards, this one looks forwards.
    printcolumn = r#"{"name":"Expires","type":"string","jsonPath":".status.expiresAt"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
/// VirtualMachineClaim: one subject's exclusive, time-boxed hold on one
/// pool member.
///
/// # What a claim guarantees
///
/// - **Bound once, ever.** The member it binds is never handed to another
///   subject, never returned to the warm set, and never reused. Release is
///   always destruction.
/// - **Deleting the claim destroys the VM**, and the claim object does not
///   disappear until it has.
/// - **`ttlSeconds` is a hard deadline**, not a grace period. At expiry the
///   member is deleted whether or not the consumer is finished.
///
/// # What a claim is not
///
/// It is not a credential channel. `spec.subject` records *who* the member
/// is for so the binding is auditable in the API server's own audit log; it
/// is opaque to banlieue, and a token must never be placed in it. Delivering
/// the subject's token to the guest is the consumer's job, over its own
/// attested channel, using `status.nonce` to tie that session to this claim.
pub struct VirtualMachineClaimSpec {
    /// The pool to take a member from. Same namespace as the claim.
    pub pool_ref: LocalObjectReference,

    /// Who this member is for. Opaque to banlieue: recorded, mirrored onto
    /// the member as annotations, and never interpreted.
    pub subject: ClaimSubject,

    /// Hard lifetime in seconds, counted from binding rather than from
    /// creation — a claim that waited ten minutes for capacity still gets
    /// its full TTL. Required, with no default: a claim without a deadline
    /// is a leaked VM, and the right value is a property of the workload.
    pub ttl_seconds: u64,
}

/// The identity a claim is made on behalf of.
///
/// Both halves are required. An issuer with no id names nobody, and an id
/// with no issuer is ambiguous across identity providers — either alone
/// makes the audit trail useless.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClaimSubject {
    /// Token issuer the subject was authenticated by, e.g. an OIDC issuer
    /// URL.
    pub issuer: String,
    /// Stable subject identifier within that issuer (the `sub` / `oid`
    /// claim), not a display name or e-mail.
    pub id: String,
}

/// Where a claim is in its one-way lifecycle.
///
/// `Pending → Bound → Releasing` is the whole of the happy path. Nothing
/// moves backwards: a claim that has bound a member never unbinds it, and a
/// claim that has started releasing never returns to `Bound`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ClaimPhase {
    /// No Ready member was available yet; the claim retries. Unbounded on
    /// purpose — the pool may simply be filling (ADR-0047 Decision 11).
    #[default]
    Pending,
    /// A member is bound and Ready. `status.virtualMachineRef` names it.
    Bound,
    /// TTL ran out or the claim was deleted; the member is being destroyed.
    Releasing,
    /// The bound member disappeared underneath the claim. Terminal: a claim
    /// is never silently rebound to a different member, because a consumer
    /// holding it believes it is talking to one specific VM.
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VirtualMachineClaimStatus {
    #[serde(default)]
    pub phase: ClaimPhase,
    /// The bound member. Set once, at bind time, and never rewritten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub virtual_machine_ref: Option<LocalObjectReference>,
    /// Mirrored from the bound member so a consumer needs one GET.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<MachineAddress>,
    /// PEM vTPM endorsement key certificate(s) of the bound member, mirrored
    /// from the infra CR (ADR-0045). Lets a verifier check that an
    /// attestation quote comes from the VM banlieue itself created for this
    /// claim. Populated since ADR-0045, from the bound member's
    /// `VirtualMachine.status`, which mirrors its infrastructure CR. Empty
    /// for a member with no vTPM.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tpm_endorsement_certificates: Vec<String>,
    /// Random, single-use, *not secret*. The consumer's attestation exchange
    /// must echo it so a quote cannot be replayed across claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_at: Option<Time>,
    /// `bound_at + spec.ttlSeconds`. The member is deleted at this instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Time>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

#[cfg(test)]
#[path = "virtualmachineclaim_tests.rs"]
mod virtualmachineclaim_tests;
