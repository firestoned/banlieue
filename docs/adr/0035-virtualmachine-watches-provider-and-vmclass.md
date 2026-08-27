# 0035 — VirtualMachine controller watches Provider and VMClass

## Status

Accepted — 2026-08-24. Extends `banlieue-controller`
(`crates/banlieue-controller/src/app.rs`,
`crates/banlieue-controller/src/reconciler/virtualmachine.rs`).

## Context

`VirtualMachine.reconcile()` reads the full `Provider` list and the
referenced `VMClass` fresh on every pass and feeds both into `schedule()`
to (re-)compute a placement `Decision` — but the `VirtualMachine`
`Controller` (`app.rs`) never watched either type. It only wired:

- `.owns(vsphere_api, ...)` — reacts to the owned `VSphereMachine`'s status
  changing.
- `.watches(image_api, ..., |image| ...)` — reacts to a referenced
  `VMImage` changing, filtered by `spec.imageRef.name`.

Found live: after editing a `Provider`'s labels (to fix a
`providerSelector`/`failureDomainSelector` mismatch against an existing
`VirtualMachine`), the VM's scheduling did not re-evaluate. It eventually
self-corrected on the next periodic `requeue_default()` tick, but that's
luck, not design — the interval is tuned for "nothing changed, just
double-check," not "something a VM depends on just changed, react now."
The separate `VSphereCluster` controller in the same file already
`.watches(provider_api, ..., |_| /* requeue every VSphereCluster */)` for
exactly this reason — `VirtualMachine` had no equivalent.

## Decision

1. **Watch `VMClass`, filtered by name** (mirrors the existing `VMImage`
   watcher exactly): `spec.classRef.name` is a direct reference, so only
   VMs actually referencing the changed class need requeuing.

2. **Watch `Provider`, unfiltered** (mirrors `VSphereCluster`'s existing
   `Provider` watcher exactly): `spec.placement.providerSelector` /
   `failureDomainSelector` match by label, not by name, so a Provider edit
   can change *any* `VirtualMachine`'s placement decision — there's no
   name to filter on. Requeue every `VirtualMachine` in the controller's
   store on any `Provider` event. Provider edits are rare and
   operator-driven, so requeuing all of them is cheap — same justification
   already accepted for `VSphereCluster`.

3. **Do not watch `Provider.status`-only changes differently from
   `Provider.spec`.** The watch is on the whole object; a `Provider`'s
   controller-written `status.failureDomains[]` changing (e.g. a new zone
   discovered) is exactly as relevant to re-scheduling as an operator
   editing `spec.capabilities`/labels — no reason to special-case one over
   the other.

## Consequences

- A `Provider` or `VMClass` edit now re-triggers every affected
  `VirtualMachine`'s reconcile immediately (event-driven), instead of
  waiting out `requeue_default()`.
- `VMClass`/`Provider` are both cluster-scoped-or-namespaced the same way
  `vm_api` already is (`Api::all` vs `Api::namespaced(ns)` keyed off
  `--namespace`), so the new watches use the same scoping rule the rest of
  `app.rs` already follows.
- This ADR only makes re-scheduling *reactive*. It does not change what
  happens once a new `Decision` comes out different from
  `status.scheduled` — that remains `migration.rs`'s existing
  `MigrationAction` branches (`InPlace`/`StickToOld`/`SurfaceOnly`/
  `Recreate`), which is exactly the gap ADR-0036 (live migration) exists
  to scope. Making placement drift detection fire *promptly* (this ADR)
  makes the *lack* of a graceful migration path (next ADR) considerably
  more visible in practice — a Provider edit can now trigger an immediate,
  unplanned `Recreate` (destroy + rebuild from scratch) instead of one
  that only ever happened to land on the next slow poll.
