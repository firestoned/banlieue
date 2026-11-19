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

use crate::common::{LabelSelector, LocalObjectReference, MachineAddress};
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

/// Bits of randomness the retired [`VirtualMachineClaimStatus::nonce`] held.
#[deprecated(
    since = "0.4.14",
    note = "status.nonce is no longer minted (ADR-0049, amended 2026-11-05); removed with the next API version"
)]
pub const CLAIM_NONCE_BITS: usize = 128;
/// Byte length the retired nonce had.
#[deprecated(
    since = "0.4.14",
    note = "status.nonce is no longer minted (ADR-0049, amended 2026-11-05); removed with the next API version"
)]
pub const CLAIM_NONCE_BYTES: usize = 128 / 8;

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
    // The pool the bound member came from, not `spec.poolRef`: a claim with
    // a `poolSelector` names no pool in its spec (ADR-0132 Decision 6).
    printcolumn = r#"{"name":"Pool","type":"string","jsonPath":".status.poolRef.name"}"#,
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
/// attested channel, anchored on the bound member's EK certificate
/// (`status.tpmEndorsementCertificates`, ADR-0045, ADR-0049).
///
/// # Which pool
///
/// Exactly one of `poolRef` (one named pool) or `poolSelector` (every pool
/// in the claim's namespace whose labels match). `poolSelector: {}` matches
/// every pool in the namespace; a claim with neither field is refused
/// rather than read as "any pool" (ADR-0132).
#[schemars(extend("x-kubernetes-validations" = [{
    "rule": "has(self.poolRef) != has(self.poolSelector)",
    "message": "a claim takes exactly one of poolRef or poolSelector; use poolSelector: {} for any pool in the namespace",
}]))]
pub struct VirtualMachineClaimSpec {
    /// The pool to take a member from. Same namespace as the claim.
    /// Exactly one of this or `poolSelector`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_ref: Option<LocalObjectReference>,

    /// Take a member from any pool in the claim's namespace whose labels
    /// match. Members of every matching pool are ranked as one set (Ready
    /// only, current image revision of their own pool first, then the one
    /// Ready longest, then name), and `status.poolRef` records the pool
    /// the bound member came from. `{}` matches every pool in the
    /// namespace. Exactly one of this or `poolRef` (ADR-0132).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_selector: Option<LabelSelector>,

    /// How the member is chosen among the Ready ones (ADR-0135). Absent
    /// means `LongestReady`, the order every claim used before this field
    /// existed. A strategy only reorders: no strategy leaves a claim
    /// `Pending` while a Ready member exists. Immutable, like the pool
    /// fields: it is read once, when the member is picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ClaimSelection>,

    /// Who this member is for. Opaque to banlieue: recorded, mirrored onto
    /// the member as annotations, and never interpreted.
    pub subject: ClaimSubject,

    /// Hard lifetime in seconds, counted from binding rather than from
    /// creation — a claim that waited ten minutes for capacity still gets
    /// its full TTL. Required, with no default: a claim without a deadline
    /// is a leaked VM, and the right value is a property of the workload.
    pub ttl_seconds: u64,
}

impl VirtualMachineClaimSpec {
    /// The strategy this claim ranks members by: `spec.selection.strategy`,
    /// or [`SelectionStrategy::LongestReady`] when unset (ADR-0135).
    #[must_use]
    pub fn strategy(&self) -> SelectionStrategy {
        self.selection
            .as_ref()
            .map(|s| s.strategy)
            .unwrap_or_default()
    }
}

/// How a claim chooses among the Ready members it may take (ADR-0135).
///
/// A struct rather than a bare enum, so later knobs (a hard spread, a
/// preferred failure domain) have somewhere to go.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClaimSelection {
    /// The ranking to apply. Defaults to `LongestReady`.
    #[serde(default)]
    pub strategy: SelectionStrategy,
}

/// The ranking a claim applies to Ready members (ADR-0135 Decision 1).
///
/// Every strategy sits inside the same fixed order: Ready and unclaimed
/// only, then current image revision of the member's own pool, then members
/// on a healthy host, then the strategy's own key, then the member Ready
/// longest, then member name. A failure domain is the pair
/// `(status.scheduled.providerName, status.scheduled.failureDomain)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SelectionStrategy {
    /// No placement preference: the member Ready longest, which is the one
    /// idle expiry would otherwise reap next.
    #[default]
    LongestReady,
    /// Take from the failure domain with the most Ready members, so warm
    /// capacity drains evenly and no single domain runs dry first.
    MostAvailable,
    /// Take from the failure domain where this claim's subject (issuer and
    /// id together) holds the fewest members, then as `MostAvailable`.
    /// Soft: a claim never waits for a free domain.
    Spread,
}

impl std::fmt::Display for SelectionStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::LongestReady => "LongestReady",
            Self::MostAvailable => "MostAvailable",
            Self::Spread => "Spread",
        })
    }
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
    /// The pool the bound member came from. Set at bind time from the
    /// member's `banlieue.io/pool` label; the only place a claim made with
    /// `spec.poolSelector` says which pool supplied its VM (ADR-0132).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_ref: Option<LocalObjectReference>,
    /// Mirrored from the bound member so a consumer needs one GET.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<MachineAddress>,
    /// PEM vTPM endorsement key certificate(s) of the bound member, mirrored
    /// from the infra CR (ADR-0045). Lets a verifier check that an
    /// attestation comes from the VM banlieue itself created for this
    /// claim. Populated since ADR-0045, from the bound member's
    /// `VirtualMachine.status`, which mirrors its infrastructure CR. Empty
    /// for a member with no vTPM.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tpm_endorsement_certificates: Vec<String>,
    /// The certificate subject CN of the bound member's node-attestation
    /// identity, mirrored from the infra CR (ADR-0143 Decision 3), beside
    /// [`tpm_endorsement_certificates`](Self::tpm_endorsement_certificates).
    /// Set for a member whose host signs it a certificate for `x509pop`
    /// (`firecracker`, or a `cloud-hypervisor` host that opted in, ADR-0146),
    /// with this subject; absent otherwise.
    /// Public and carrying no authorization, so a consumer needs one GET.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation_subject: Option<String>,
    /// Deprecated, never set since v0.4.14. It was a per-claim value for
    /// ADR-0049's quote-over-nonce handshake, which SPIRE's TPM node
    /// attestation superseded (ADR-0049, amended 2026-11-05). Kept so an
    /// existing claim's status still parses; removed with the next API
    /// version.
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
