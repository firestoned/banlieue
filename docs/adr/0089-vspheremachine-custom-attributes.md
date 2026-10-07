<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0089 — VSphereMachine custom attributes: Template, CreatedBy, CreatedAt

- **Status:** Accepted
- **Date:** 2026-10-06
- **Deciders:** Erick Bourgeois
- **Related:** [ADR-0007](0007-admission-policies.md) (ValidatingAdmissionPolicy
  posture this extends to mutation); [ADR-0047](0047-virtualmachineclaim.md)
  Decision 10 (the `subject.id == request.userInfo.username` precedent this
  reuses); [ADR-0024](0024-vspheremachine-clone-static-ip-cloud-config.md)
  (`VSphereMachine` create path, `ensure_vm`);
  [ADR-0005](0005-capi-contract-label-codegen.md)
  (code-first CRD schema).

## Context

`sre-automations`' `cf-node` tool stamps three vCenter **custom attributes**
(`CustomFieldsManager`, not the `annotation` field — see below) on every VM it
clones: `Template` (the source template name), `CreatedBy` (the operator who
ran the rebuild), `CreatedAt` (the clone's creation timestamp). These are
`govc`/vCenter UI–visible, free-text, per-object fields operators use for
fleet auditing — "which template did this come from, who built it, when" —
without opening banlieue or the Kubernetes API at all.

banlieue has no equivalent today: a `VSphereMachine`'s provenance is legible
only through `kubectl`. This ADR brings the same three attributes to VMs
banlieue clones, auto-populated from data banlieue already has — no new
credential, no new RPC.

### Why not the `annotation` field

vCenter's free-text `config.annotation` is a different, single-value field.
`mke-build`'s own vSphere role already writes the source template name there
and its rolling-upgrade pre-flight compares it *exactly*; overwriting it with
anything else would make every banlieue-built VM look like it needs an
upgrade it does not. A vSphere **tag** is the wrong shape too — a tag is a
reusable, bounded label within a category, and a per-VM creation timestamp
would mint a new tag value on every clone. A custom attribute is free text,
per-object, and collides with neither.

### `Template` and `CreatedAt` need no new data

`VSphereMachineSpec.template` is already the resolved template name
(`crates/banlieue-api/src/infrastructure/vsphere_machine.rs`), and every
Kubernetes object already carries `metadata.creationTimestamp`. Both
attributes are a direct read of data the `VSphereMachine` already has at the
point `ensure_vm` clones it — no new field, no new source.

### `CreatedBy` needs an identity nothing today records

Nothing in banlieue's object model answers "which principal applied this
`VirtualMachine`." `metadata.managedFields` records a **field manager**
string (a client/tool name such as `kubectl-client-side-apply`), not a human
or service-account identity. Capturing the real one means reading
`request.userInfo` at admission — which exists only as the live admission
request, gone by the time any controller reconciles the persisted object.

banlieue already solved exactly this problem once:
[ADR-0047](0047-virtualmachineclaim.md) Decision 10 binds
`VirtualMachineClaim.spec.subject.id` to `request.userInfo.username` via a
`ValidatingAdmissionPolicy`'s CEL `authorizer`/`request` variables, at
admission, while the caller still exists. That precedent only *validates*;
stamping an annotation requires *mutation*, which
`ValidatingAdmissionPolicy` cannot do.

The project's architectural posture (ADR-0007) is no validating webhook —
no extra always-on service, no TLS rotation, no availability dependency on
every write. A **mutating** webhook would reopen exactly that trade-off for
a one-line annotation. Kubernetes now has an in-API-server equivalent for
mutation: `MutatingAdmissionPolicy` (`admissionregistration.k8s.io/v1`,
CEL-evaluated, alpha in 1.32, beta in 1.34, **GA and enabled by default in
1.36**). Confirmed against the upstream reference
(<https://kubernetes.io/docs/reference/access-authn-authz/mutating-admission-policy/>):
its `request` variable carries `userInfo` exactly as
`ValidatingAdmissionPolicy`'s does, and an `ApplyConfiguration`-patchType
mutation can set `metadata.annotations` via CEL `Object{}` construction —
no JSONPatch index arithmetic needed.

## Decision

1. **A `MutatingAdmissionPolicy` stamps `banlieue.io/created-by` on
   `VirtualMachine` CREATE**, reading `request.userInfo.username`. Scoped to
   `operations: ["CREATE"]` only, so the attribution is set once, at the
   moment of creation, and an in-place edit of the `VirtualMachine` later by
   a different principal never overwrites who originally created it. Shipped
   as `deploy/admission/virtualmachine-created-by.yaml`
   (`MutatingAdmissionPolicy` + `MutatingAdmissionPolicyBinding`), applied
   the same optional, separate way as every other file in `deploy/admission/`
   (ADR-0007) — a cluster that skips it simply never gets the annotation, and
   `CreatedBy` is omitted downstream (Decision 4). `failurePolicy: Ignore`:
   unlike the claim-subject policy this guards an audit convenience, not an
   authorization boundary, so a policy evaluation failure must never block a
   `VirtualMachine` create.

   This requires **Kubernetes 1.36+** (`MutatingAdmissionPolicy` GA) — a
   higher floor than ADR-0007's VAP-GA 1.30, consistent with banlieue's
   existing pattern of layering newer-apiserver-dependent optional hardening
   on top of an older baseline (several existing VAPs already need the CEL
   `authorizer`/`variables` features beyond the 1.30 floor).

2. **No immutability guard on the annotation itself.** Unlike
   `VirtualMachineClaim.spec.subject` (ADR-0047), `created-by` carries no
   authorization weight — nothing downstream trusts it to decide who may do
   what. It is informational, the same way `cf-node`'s vCenter attribute is.
   A principal with `update` on the `VirtualMachine` can edit or remove the
   annotation; this is accepted, not hardened against.

3. **`VSphereMachineSpec` gains `created_by: Option<String>`.** Populated by
   `banlieue-controller`'s `build_vsphere_machine`
   (`crates/banlieue-controller/src/reconciler/infra.rs`) by copying the
   `banlieue.io/created-by` annotation off the parent `VirtualMachine`, if
   present. `None` when the annotation is absent (cluster without the
   policy, or the `VirtualMachine` predates it) — `VSphereMachine` never
   invents a value.

4. **The vSphere provider stamps three custom attributes once, on first
   provision.** `VSphereClient` gains
   `set_custom_attributes(&self, vm_moref: &str, values: &[(String, String)])`,
   implemented against `vim_rs`'s `CustomFieldsManager` (`field()` to find an
   existing definition, `add_custom_field_def` if missing, then `set_field`
   per pair) — the same list-then-define-if-missing shape as `govc`'s
   `fields.set -add`. Called from
   `crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`'s
   `ensure_vm`, inside the same `existing_vm_ref.is_some()` early-return that
   already makes `clone_vm` / `grow_os_disk` / `add_tpm_device` run exactly
   once (ADR-0024), so no new status field is needed to prevent a repeat
   stamp on every reconcile:
   - `Template` ← `spec.template` (always set)
   - `CreatedAt` ← `Utc::now()` at clone time, RFC 3339 (always set)
   - `CreatedBy` ← `spec.created_by` (set only when `Some`; the attribute is
     simply not written when absent, matching Decision 3 — never a stand-in
     value like `"unknown"`)

5. **Proxmox and libvirt are out of scope for this ADR.** Proxmox has no
   custom-attribute concept — only free-form `tags` (a flat label set) and a
   `description`/notes field, neither of which is a 1:1 fit. libvirt has only
   an XML `<metadata>` block. Both need their own design, deferred to a
   follow-up ADR rather than guessed here.

## Consequences

**Positive**

- Matches an existing, validated operator workflow (`cf-node`) with no new
  credential and no new RPC — `set_custom_attributes` runs over the same
  vCenter session `ensure_vm` already holds.
- `CreatedBy` reuses ADR-0047's admission-time-identity-capture pattern
  rather than inventing a second one; `MutatingAdmissionPolicy` keeps the
  no-webhook posture (ADR-0007) intact for mutation the same way VAP does
  for validation.
- `Template`/`CreatedAt` need zero new fields — pure reads of data
  `VSphereMachine` already carries.

**Negative / trade-offs**

- **Floor of Kubernetes 1.36.** Higher than any existing banlieue admission
  policy. A cluster below it gets `Template`/`CreatedAt` (provider-side,
  unconditional) but never `CreatedBy` (requires the policy). Documented in
  `deploy/admission/README.md`.
- **`CreatedBy` is best-effort, not a trust boundary.** It records whichever
  principal's token created the `VirtualMachine`, with no protection against
  a later edit — explicitly accepted in Decision 2, unlike the claim-subject
  case it borrows its mechanism from.
- **Proxmox/libvirt parity is deferred**, so `VSphereMachine` is the only
  `*Machine` CRD with this feature until a follow-up ADR designs the other
  two backends' equivalents.

## Alternatives considered

- **Mutating webhook.** Rejected for the same reason ADR-0007 rejects one for
  validation: a new always-on Deployment, TLS cert rotation, and a
  `failurePolicy: Fail` availability dependency on every `VirtualMachine`
  write — disproportionate for one annotation.
- **Field-manager–derived `CreatedBy`.** `metadata.managedFields[].manager` is
  already present with no admission policy at all, but it is a client/tool
  name, not an identity (`kubectl-client-side-apply` for most `kubectl apply`
  callers) — it would not answer the question `cf-node`'s attribute answers.
- **Generic `customAttributes: map<string,string>` on all three `*Machine`
  CRDs now.** Considered and rejected for this ADR (Decision 5): Proxmox and
  libvirt have no matching native mechanism, and designing their mapping
  without a concrete backend-shaped answer would guess rather than decide.
