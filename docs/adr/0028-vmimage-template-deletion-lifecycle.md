# 0028 — VMImage deletion lifecycle: destroy per-zone vCenter templates by default

## Status

Accepted — 2026-08-22. Amends [ADR-0020](0020-vsphere-per-zone-iso-import.md)
(per-zone template import) and follows the same shape as
[ADR-0026](0026-vspheremachine-deletion-lifecycle.md) (`VSphereMachine`
deletion lifecycle) — this ADR is the same fix for `VMImage`.

## Context

Found live: deleting a `VMImage` CR removed it from Kubernetes immediately,
but every per-zone vCenter template it caused
`crates/banlieue-provider-vsphere/src/import.rs`'s import Job to build stayed
behind, orphaned. The vsphere provider's `vmimage` reconciler
(`crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs`) has no
`deletion_timestamp` check and no finalizer at all — the same class of gap
ADR-0026 already found and fixed for `VSphereMachine`/cloned VMs, just for
the *template* build path instead of the *clone* path.

Unlike `VSphereMachine` (one CR, one backend VM, one Provider), a `VMImage`
is cluster-scoped and can have per-provider status rows
(`status.perProvider[]`), each with its own per-zone rows
(`status.perProvider[].zones[]`) — a single `VMImage` can own templates
across multiple Providers and multiple failure domains within each. The
`OSArtifact` Kubernetes object `banlieue-imagebuilder` creates for a `Url`
source already cleans up correctly via ordinary owner-reference garbage
collection (it's a Kubernetes object, not an external backend resource) —
this ADR is specifically about the *vCenter* templates, which need the same
finalizer + explicit-teardown treatment as any other externally-owned
backend resource.

## Decision

### 1. `VMImageTemplate.retainOnDelete: bool` (default `false`)

Added to `crates/banlieue-api/src/banlieue/vmimage.rs`. `false` (the
default) means deleting the `VMImage` also destroys every per-zone
template it owns — declarative deletion, matching `VirtualMachine`'s own
cascade onto `VSphereMachine`. `true` opts out (e.g. the template is still
referenced by another generation, or its lifecycle is managed by hand).

### 2. `banlieue.io/vmimage` finalizer on `VMImage`

`crates/banlieue-provider-vsphere/src/reconciler/vmimage.rs::reconcile`
checks `deletion_timestamp` first, mirroring the pattern ADR-0026 already
established for `VSphereMachine` and the parent controller's own
`VirtualMachine` finalizer:

- **Set** → finalize path: unless `retainOnDelete` is `true`, for every
  `status.perProvider[]` row with at least one zone, resolve that
  Provider's vCenter client (same resolution the create path already does
  — `read_credentials` / `resolve_ca_bundle` / `ctx.vsphere.build`), then
  for each zone with a `resolvedRef`, look up the failure domain's
  datacenter (`Provider.status.failureDomains[].attributes.raw["datacenter"]`,
  already populated by the provider reconciler, ADR-0019) and
  `find_template(dc, zone.templateFolder, zone.resolvedRef)`. A template
  already absent (a prior partial finalize attempt, or manual cleanup) is
  success, not an error — same idempotency posture as `VSphereMachine`'s
  `destroy_vm`. A template that IS found is destroyed with the exact same
  `VSphereClient::destroy_vm` (power-off-then-`Destroy_Task`, ADR-0026) a
  clone's backing VM uses — a vCenter template is just a VM with the
  template bit set; the teardown mechanics are identical.
- **Unset** → existing reconcile logic, unchanged, except it now also
  calls `ensure_finalizer` first.

Errors during finalize propagate to `error_policy` and leave the finalizer
in place, for the same reason ADR-0026 gives: a destructive, irreversible
operation should fail closed and retry, not silently drop the finalizer and
leave a template it never actually destroyed.

### 3. Not covered by this ADR

- **Already-orphaned templates** from before this fix ships need manual
  identification/cleanup in vCenter — same caveat ADR-0026 already
  documents for already-orphaned cloned VMs.
- **Cross-generation template sharing** (e.g. two `VMImage`s intentionally
  pointing at the same template name/folder) is not modeled — this ADR
  assumes one `VMImage` owns the templates its own `status.perProvider[]`
  rows report. If that assumption ever needs revisiting, `retainOnDelete`
  is the escape hatch until it does.

## Consequences

- Deleting a `VMImage` now blocks (visible `Terminating`, requeue-driven
  retries) until every per-zone template it owns is confirmed destroyed —
  same observable behavior ADR-0026 gives `VirtualMachine`/`VSphereMachine`.
- `retainOnDelete: true` is the one place an operator opts back into the
  old (accidental) behavior deliberately, rather than getting it by default.
- Reuses `VSphereClient::destroy_vm` (ADR-0026) as-is — no new client
  method needed, since a template is destroyed exactly the way a clone is.
