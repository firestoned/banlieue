<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0007 — ValidatingAdmissionPolicies for CRD invariants

- **Status:** Accepted
- **Date:** 2026-05-31
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0004 (single `banlieue` binary); the CRD-only contract (no RPC between controller and providers); `deploy/admission/`; the [Core Controller guide](../src/guides/core-controller.md). Code-first CRD schema is the source of truth (ADR-0005).

## Context

banlieue's CRDs are code-first: their OpenAPI v3 structural schema is generated
from the Rust types in `crates/banlieue-api` (ADR-0005). That schema enforces
*structural* validation — types, required fields, enums, formats. It cannot
express two classes of invariant banlieue needs:

1. **Immutability.** A `VirtualMachine`'s `classRef`/`imageRef` and a
   `Provider`'s `providerClassRef` are part of the object's identity. Mutating
   them in place would silently mean "rebuild this running VM on different
   hardware / a different OS" or "re-point this backend at a different provider
   class, orphaning everything scheduled onto it." banlieue's model is
   delete-and-recreate, so these references must be immutable after creation.
2. **Cross-field rules** the schema can't represent.

These must be rejected **at admission**, before the bad object is persisted —
not discovered later during reconciliation.

Three mechanisms can enforce admission-time validation on a CRD:

- a **validating webhook** (a `ValidatingWebhookConfiguration` + an HTTPS server);
- **`ValidatingAdmissionPolicy`** (CEL evaluated inside the API server, GA in
  Kubernetes 1.30);
- **CRD-embedded CEL** (`x-kubernetes-validations` transition rules, with
  `oldSelf`).

banlieue's architectural posture is deliberately lean: **no RPC, no extra
always-on services** (the controller and providers talk only through the
Kubernetes API). A validating webhook contradicts that — it is a new
always-on Deployment on the API request path, with TLS certificates to issue
and rotate, an availability/latency dependency (a down webhook with
`failurePolicy: Fail` wedges writes), and another thing to operate.

## Decision

**Use `ValidatingAdmissionPolicy` (admissionregistration.k8s.io/v1) for
admission-time CRD invariants that the structural schema cannot express. Ship
them as optional, separately-applied manifests under `deploy/admission/`. Do not
introduce a validating webhook.**

Initial policies (each a `ValidatingAdmissionPolicy` + a
`ValidatingAdmissionPolicyBinding`):

- `banlieue-virtualmachine-immutable-refs` — `spec.classRef` / `spec.imageRef`
  immutable on `UPDATE`.
- `banlieue-provider-immutable-class` — `spec.providerClassRef.name` immutable on
  `UPDATE`.

Conventions:

- `failurePolicy: Fail` on the policy; bindings use `validationActions: ["Deny"]`
  to enforce. Operators may roll out report-only first with
  `["Warn", "Audit"]`, then switch to `Deny`.
- `messageExpression` includes the old value for an actionable rejection.
- Policies live in `deploy/admission/` and are applied **after** the CRDs,
  **separately** from the controller — they are hardening, not a hard runtime
  dependency of the controller.

This requires a **Kubernetes 1.30+** cluster (VAP GA). The controllers
themselves do not depend on the policies; an older cluster simply runs without
them, falling back to the controller's delete-and-recreate semantics.

## Consequences

**Positive**

- **No new moving parts.** No webhook server, no Deployment, no certificate
  lifecycle, no API-path availability dependency. Consistent with the
  CRD-only / no-RPC posture.
- **In-API-server, fail-closed, fast.** CEL runs in the apiserver; rejection is
  immediate and needs no network hop.
- **Declarative and versioned.** Policies are plain manifests in `deploy/`,
  reviewable and diffable; they can be rolled out report-only (`Warn`/`Audit`)
  and toggled or removed without touching the controller or regenerating CRDs.

**Negative / trade-offs**

- **Optional ⇒ not guaranteed.** Because the policies are applied separately, a
  cluster that skips them gets no admission-time immutability — only the
  controller's recreate behavior. Documented in the guide; acceptable for
  v1alpha1.
- **Floor of Kubernetes 1.30.** Enforcement needs VAP GA. The project targets
  modern clusters; the controllers still run on 1.27+ without the policies.
- **CEL expressivity limits.** VAP CEL cannot do cross-resource lookups (e.g.
  "does the referenced VMClass exist?"). Those validations stay in the
  controller's reconcile path, surfaced via status — VAP covers only
  self-contained / transition rules.
- **Two places assert immutability conceptually** (admission policy + the
  controller treating a ref change as recreate). They must stay consistent; the
  policy is the authoritative gate when present.

## Alternatives considered

- **Validating webhook.** Most flexible (arbitrary Go logic, cross-resource
  lookups), but adds an always-on service, TLS cert rotation, and an
  availability dependency on every write — rejected as contrary to banlieue's
  no-extra-services posture for what are simple, self-contained rules.
- **CRD-embedded CEL (`x-kubernetes-validations` with `oldSelf`).** The most
  code-first option: immutability transition rules would live in the generated
  CRD schema, always present and versioned with the CRD. Attractive, and a
  likely future enhancement. Not chosen now because (a) it couples the rule to
  schema generation (kube-derive/schemars support for emitting transition rules
  is limited), and (b) it cannot be rolled out report-only or toggled per
  cluster the way a VAP binding can. VAP keeps the policy decoupled from the
  schema and operationally flexible while we are still in v1alpha1. Revisit
  promoting the stable invariants into CRD CEL once the schema stabilizes.
- **Controller-side only.** Rejecting in the reconcile loop is too late: the bad
  spec is already persisted, and the controller can only report the problem via
  status — it cannot prevent the write or the resulting drift.
