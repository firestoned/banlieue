# 0025 — userData resolved by banlieue-controller, namespace-scoped Role

## Status

Accepted — 2026-08-20. Extends [ADR-0024](0024-vspheremachine-clone-static-ip-cloud-config.md)
(`VSphereMachineSpec.userData`). Revises this ADR's own first draft, which
proposed operator-managed per-Provider `Role` extensions — superseded
before implementation; see *History* below.
Extended by [ADR-0038](0038-userdata-configmap-support.md) (adds ConfigMap
source alongside Secret).

## Context

ADR-0024 gave `VSphereMachineSpec` a `userData: Option<UserDataSpec>`
field — a `secretRef` + `key`, resolved from the parent `VirtualMachine`'s
own `spec.userData` — read by the vsphere provider's reconciler. That
requires the *provider* to `get` an arbitrary, user-named Secret, and
neither existing RBAC surface covers it:

- The provider's cluster-wide `ClusterRole` deliberately grants **zero**
  Secret access (security review 2026-07-31, CHAIN-002).
- `banlieue-operator`'s per-instance `Role` (ADR-0003) is
  `resourceNames`-scoped to exactly the Secrets *the Provider's own*
  `spec.connection` names — it has no way to know about a Secret some
  later, arbitrary `VirtualMachine` references.

Separately, `banlieue-controller`'s own `ClusterRole` has **no** Secret
rule either, but for a different, already-documented reason (security
review 2026-07-31, SEC-008): *"no code in banlieue-controller reads a
Secret — provider credentials are read by the providers themselves,
user-data is handed to providers via the infra CRDs... **Re-add scoped
(resourceNames/namespace) if a reconciler ever needs it.**"*

This project currently runs single-tenant — one `Provider`, one
namespace (`banlieue-system`) holding every `VirtualMachine` and Secret.
Building operator-managed, per-Provider, dynamically-recomputed RBAC (this
ADR's original proposal) to solve a multi-tenant problem that doesn't
exist yet is exactly the kind of complexity this project's current KISS
posture rejects — reconsider it if/when multiple tenants or namespaces
are actually in play.

## Decision

**`banlieue-controller` resolves and renders `VirtualMachine.spec.userData`
itself, inlining the *content* — not a reference — into
`VSphereMachineSpec.userData: Option<String>`. RBAC for that read is a
single static namespace-scoped `Role` + `RoleBinding` in `banlieue-system`,
not a `ClusterRole` change.**

Concretely:

1. `VSphereMachineSpec.userData` changes from `Option<UserDataSpec>`
   (a reference) to `Option<String>` — the already placeholder-substituted
   cloud-config content (ADR-0024's fixed `${VM_NAME}`/`${FQDN}`/etc. set,
   [`banlieue_provider_sdk::guestdata::render_placeholders`]), ready for
   the provider to base64-encode into `guestinfo.userdata` verbatim. The
   provider no longer reads a Secret at all for this — `build_guestinfo`
   and `ensure_vm` are unchanged (they already took `rendered_userdata:
   Option<&str>`, agnostic to where it came from).
2. `banlieue-controller`'s `virtualmachine.rs` reconciler resolves the
   Secret and renders it *before* calling `build_vsphere_machine`, which
   gains a `rendered_user_data: Option<&str>` parameter (still a pure,
   synchronous function — the I/O happens in the caller, matching this
   codebase's existing separation between reconcile-glue and pure decision
   logic).
3. RBAC: a namespace-scoped `Role` in `banlieue-system`
   (`deploy/controller/rbac/role.yaml` + `rolebinding.yaml`) granting
   `get` on `secrets`, bound to the `banlieue-controller` ServiceAccount.
   Not `resourceNames`-scoped (there is exactly one trusted namespace in
   play; narrowing further is complexity with no present payoff) and
   deliberately **not** added to the cluster-wide `ClusterRole` — this
   grant only ever applies within `banlieue-system`, never cluster-wide.
   This is precisely the "scoped (namespace)" case SEC-008's own comment
   anticipated, not a reversal of it.

## Consequences

- No new operator watch, no per-VM `Role` recomputation, no cross-namespace
  case to design for right now.
- Cloud-config content (which commonly includes SSH authorized keys) is
  now visible via `kubectl get vspheremachine -o yaml` to anyone who can
  read that namespaced resource — an explicit, accepted tradeoff for the
  current single-tenant posture, not a silent one. Revisit if/when
  `VSphereMachine` read access needs to be broader than "operators of this
  one environment."
- `banlieue-provider-vsphere`'s `vspheremachine.rs` gets simpler: no Secret
  read, no `guestdata` import, no `SECRET_MISSING`/`SECRET_INVALID`
  handling for userData specifically (those reasons remain, still used for
  the unrelated credentials-Secret path).
- If multi-tenancy becomes real (multiple namespaces, multiple trust
  boundaries), this decision is the one to revisit first — the original
  operator-managed-Role design (see *History*) is the shape that scales to
  that case, not this one.

## History

The first draft of this ADR (2026-08-20, superseded same day before any
code was written against it) proposed extending `banlieue-operator`'s
per-instance `Role` (ADR-0003) with a `resourceNames` rule per userData
Secret, recomputed from every `VirtualMachine` scheduled to a given
Provider. Correct for a multi-tenant deployment, but overbuilt for this
project's current single-Provider, single-namespace reality — replaced
with the simpler decision above.
