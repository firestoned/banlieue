<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0042 — A VirtualMachine may only reference userData its creator can read

## Status

Accepted — 2026-09-09. Amends [ADR-0025](0025-vspheremachine-userdata-secret-rbac.md)
(controller-resolved userData, namespace-scoped Role) and
[ADR-0038](0038-userdata-configmap-support.md) (ConfigMap sources). Applies the
CEL-`authorizer` pattern established by
[ADR-0007](0007-admission-policies.md) for `Provider.spec.connection.credentialsRef`.

## Context

ADR-0025 decided that `banlieue-controller` resolves
`VirtualMachine.spec.userData` itself and inlines the **rendered content** into
`VSphereMachine.spec.userData` as a plain string. ADR-0038 extended the source
to ConfigMaps. The RBAC for that read is a namespaced `Role` in
`banlieue-system` granting `get` on `secrets` and `configmaps` — deliberately
**not** `resourceNames`-scoped, on the reasoning that there is exactly one
trusted namespace in play.

ADR-0025 recorded the resulting visibility as an accepted trade-off:

> Cloud-config content (which commonly includes SSH authorized keys) is now
> visible via `kubectl get vspheremachine -o yaml` to anyone who can read that
> namespaced resource — an explicit, accepted tradeoff.

What it did not state is the consequence of combining that reflection with an
un-scoped grant. The controller will resolve **any** Secret or ConfigMap in
`banlieue-system` that a `VirtualMachine` names, on behalf of whoever wrote that
`VirtualMachine`, and publish its contents into a resource the same principal
can read. A principal holding only `create virtualmachines` — and no Secret
access whatsoever — therefore holds an effective `get` on every Secret in the
namespace, including every `Provider`'s hypervisor credentials:

1. `spec.userData.secretRef.name: <a Provider's credentials Secret>`
2. the controller reads it (its Role permits any name) and renders it into
   `VSphereMachine.spec.userData`
3. `kubectl get vspheremachine -o yaml`

This is a **confused deputy**, and it is structurally identical to CHAIN-001
from the 2026-07-31 review — where a `Provider` could name a credentials Secret
its creator could not read. That one was closed by
`banlieue-provider-credentialsref-authorization`, a `ValidatingAdmissionPolicy`
using the CEL `authorizer` variable to require that the *requesting principal*
can `get` the referenced Secret. The identical hazard on `userData` was simply
never covered: none of the seven policies in `deploy/admission/` reference
`userData` at all.

The accepted trade-off in ADR-0025 stands — content reflection into
`VSphereMachine` is a known property of the single-tenant design. The
escalation is a separate defect: the deputy performs a privileged read for a
principal that could not perform it itself.

## Decision

**The principal creating or updating a `VirtualMachine` must itself be
authorized to `get` the Secret or ConfigMap named by `spec.userData`, enforced
at admission by a `ValidatingAdmissionPolicy` using the CEL `authorizer`.**

`deploy/admission/virtualmachine-userdata-authorization.yaml` adds one policy
with two validations — one per source kind, each vacuously true when that source
is unset (`UserDataSpec` already requires exactly one of them, enforced by
`UserDataSpec::validate`):

```cel
!has(object.spec.userData) || !has(object.spec.userData.secretRef)
  || authorizer.group('').resource('secrets')
       .namespace(object.metadata.namespace)
       .name(object.spec.userData.secretRef.name)
       .check('get').allowed()
```

The namespace checked is the `VirtualMachine`'s own — matching where the
controller actually reads from — and the reason is `Forbidden`, not `Invalid`:
the request is well-formed, the principal is simply not entitled to it.

Shipped as its own file, for the same reason
`provider-credentialsref-authorization.yaml` is: `authorizer` requires a
sufficiently recent API server, and an older one must reject only this file
rather than the whole directory.

### What this deliberately does not do

- **It does not `resourceNames`-scope the controller's Role.** The controller
  legitimately needs to read whatever a *validly admitted* `VirtualMachine`
  names, and those names are not knowable when the manifest is written.
  Authorization moves to admission, where the requesting principal's identity
  still exists — by reconcile time it is long gone. This is the same division
  of labour as `credentialsRef`.
- **It does not stop a principal who can already read the Secret** from
  surfacing it in `VSphereMachine.spec`. That is ADR-0025's accepted
  reflection, unchanged.
- **It does not make `VirtualMachine` create safe to delegate broadly.** It
  makes it no longer an *escalation*: a principal can now reach exactly the
  user-data it could already read, and nothing more.

## Consequences

- `create virtualmachines` stops being a universal Secret-read grant in the
  controller namespace. The two privileges are decoupled.
- **`VSphereMachine` remains credential-bearing** and must be restricted
  accordingly: it holds rendered user-data in plaintext. The threat model's
  hardening section says so explicitly, and this ADR does not change it.
- Automation that creates `VirtualMachine`s must hold `get` on the user-data
  Secret it references. On a default install the controller's own
  ServiceAccount is unaffected (it is not the admission principal); CI or
  GitOps identities that create VMs may need a `get` grant they previously did
  not require. This is the intended, visible cost.
- The policy is `failurePolicy: Fail`. A cluster whose API server does not
  support `authorizer` must not apply this file — documented in
  `deploy/admission/README.md`, mirroring the existing note.
- If multi-tenancy becomes real, this policy is necessary but not sufficient;
  ADR-0025's superseded operator-managed per-VM Role design is still the shape
  that scales, and remains the thing to revisit first.
